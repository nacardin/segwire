//! D-Bus service implementation for the segwire daemon
//!
//! Provides the D-Bus interface for CLI communication using the `dbus` crate
//! with `dbus-crossroads` for method dispatch. Fully synchronous — no internal
//! async executor.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use dbus_crossroads::Crossroads;
use segwire_common::dbus::{
    interface, interface_helpers, DbusError, NamespaceState, OperationResult, ValidationResult,
};
use segwire_common::error::{ErrorContext, SegwireError};
use segwire_common::{log_info, LogContext};
use tracing::{debug, info, warn};

/// Shared state accessible from D-Bus method handlers
struct ServiceState {
    config_manager: Arc<Mutex<crate::config::ConfigManager>>,
    state_manager: Arc<Mutex<crate::namespace_state::NamespaceStateManager>>,
    daemon_start_time: SystemTime,
    authorizer: Option<crate::policykit::PolicyKitAuthorizer>,
}

pub struct DbusService {
    connection: Connection,
}

impl DbusService {
    /// Create a new D-Bus service instance
    pub fn new(
        config_manager: Arc<Mutex<crate::config::ConfigManager>>,
        state_manager: Arc<Mutex<crate::namespace_state::NamespaceStateManager>>,
    ) -> Result<Self, SegwireError> {
        let (config_dir, namespace_prefix) = {
            let manager = config_manager.lock().unwrap();
            (
                manager.config_directory().to_path_buf(),
                manager.namespace_prefix().to_owned(),
            )
        };

        let ctx = LogContext::new("dbus_service_initialization")
            .with_field("service_name", interface::SERVICE_NAME)
            .with_field("object_path", interface::OBJECT_PATH)
            .with_field("namespace_prefix", namespace_prefix.clone())
            .with_field("config_dir", config_dir.display().to_string());

        log_info!(ctx, "Initializing D-Bus service");

        // Connect to D-Bus — use session bus in simulation or test mode
        let use_session_bus = std::env::var("SEGWIRE_TEST_SESSION_BUS").is_ok();

        let connection = if use_session_bus {
            log_info!(ctx, "Using session D-Bus (simulation/test mode)");
            Connection::new_session()
        } else {
            Connection::new_system()
        }
        .map_err(|e| {
            let error_ctx = ErrorContext::new("dbus_connection")
                .with_field("service_name", interface::SERVICE_NAME)
                .with_remediation("Ensure D-Bus system bus is available")
                .with_remediation("Check that the daemon has permission to access the system bus");
            SegwireError::DBus(e.to_string())
                .with_context(error_ctx)
                .log_and_return()
        })?;

        // Request the well-known service name from the config
        let service_name = {
            let manager = config_manager.lock().unwrap();
            manager.dbus_service_name().to_owned()
        };

        connection
            .request_name(&service_name, false, true, false)
            .map_err(|e| {
                let error_ctx = ErrorContext::new("dbus_service_name_registration")
                    .with_field("service_name", &service_name)
                    .with_remediation("Ensure no other instance of segwire-daemon is running")
                    .with_remediation("Check D-Bus service configuration");
                SegwireError::DBus(e.to_string())
                    .with_context(error_ctx)
                    .log_and_return()
            })?;

        // Build the PolicyKit authorizer
        let authorizer = match crate::policykit::PolicyKitAuthorizer::new(&connection) {
            Ok(auth) => Some(auth),
            Err(e) => {
                warn!("PolicyKit authorizer initialization failed: {}, authorization checks will use UID fallback", e);
                None
            }
        };

        // Set up crossroads for method dispatch
        let mut cr = Crossroads::new();

        // Allow processing of incoming messages by the crossroads dispatcher
        cr.set_async_support(None);

        let shared_state = Arc::new(Mutex::new(ServiceState {
            config_manager: config_manager.clone(),
            state_manager: state_manager.clone(),
            daemon_start_time: SystemTime::now(),
            authorizer,
        }));

        let iface_token = {
            let state = shared_state.clone();
            cr.register(interface::INTERFACE_NAME, move |b| {
                // ── ListNamespaces ──
                {
                    let state = state.clone();
                    b.method(
                        "ListNamespaces",
                        (),
                        ("namespaces",),
                        move |ctx, _cr: &mut (), ()| {
                            debug!("D-Bus method call: ListNamespaces");
                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "list", ctx)?;

                            let prefix = {
                                let config_mgr = svc.config_manager.lock().unwrap();
                                config_mgr.namespace_prefix().to_owned()
                            };

                            let manager = svc.state_manager.lock().unwrap();
                            let namespaces: Vec<(String, String, String, String)> = manager
                                .get_all_states()
                                .values()
                                .map(|ns| {
                                    (
                                        ns.name.clone(),
                                        ns.status.to_string(),
                                        ns.config_path.clone(),
                                        format!("Namespace managed by {}", prefix),
                                    )
                                })
                                .collect();

                            debug!("Returning {} namespaces", namespaces.len());
                            Ok((namespaces,))
                        },
                    );
                }

                // ── GetNamespaceStatus ──
                {
                    let state = state.clone();
                    b.method(
                        "GetNamespaceStatus",
                        ("name",),
                        (
                            "name",
                            "full_name",
                            "status",
                            "config_path",
                            "created_at",
                            "last_updated",
                        ),
                        move |ctx, _cr: &mut (), (name,): (String,)| {
                            debug!("D-Bus method call: GetNamespaceStatus({})", name);

                            if let Err(e) = interface_helpers::validate_namespace_name(&name) {
                                warn!("Invalid namespace name '{}': {}", name, e.message());
                                return Err(create_method_err(SegwireError::from(e)));
                            }

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "status", ctx)?;

                            let full_name = {
                                let config_mgr = svc.config_manager.lock().unwrap();
                                config_mgr.generate_full_namespace_name(&name)
                            };

                            let manager = svc.state_manager.lock().unwrap();
                            match manager
                                .get_namespace_state(&full_name)
                                .or_else(|| manager.get_namespace_state(&name))
                            {
                                Some(namespace) => {
                                    debug!(
                                        "Found namespace '{}' with status '{}'",
                                        name, namespace.status
                                    );
                                    Ok((
                                        namespace.name.clone(),
                                        namespace.full_name.clone(),
                                        namespace.status.to_string(),
                                        namespace.config_path.clone(),
                                        namespace.created_at,
                                        namespace.last_updated,
                                    ))
                                }
                                None => {
                                    warn!("Namespace '{}' not found", name);
                                    Err(create_method_err(SegwireError::from(
                                        DbusError::NamespaceNotFound(format!(
                                            "Namespace '{}' not found",
                                            name
                                        )),
                                    )))
                                }
                            }
                        },
                    );
                }

                // ── DeleteNamespace ──
                {
                    let state = state.clone();
                    b.method(
                        "DeleteNamespace",
                        ("name",),
                        ("success", "message", "details"),
                        move |ctx, _cr: &mut (), (name,): (String,)| {
                            debug!("D-Bus method call: DeleteNamespace({})", name);

                            if let Err(e) = interface_helpers::validate_namespace_name(&name) {
                                warn!("Invalid namespace name '{}': {}", name, e.message());
                                return Err(create_method_err(SegwireError::from(e)));
                            }

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "delete", ctx)?;

                            match delete_namespace_by_name(&svc, &name) {
                                Ok(()) => {
                                    info!("Successfully deleted namespace '{}'", name);
                                    let result = OperationResult::success(format!(
                                        "Namespace '{}' deleted successfully",
                                        name
                                    ))
                                    .with_detail("namespace".to_string(), name);
                                    Ok((result.success, result.message, result.details))
                                }
                                Err(e) => {
                                    warn!("Failed to delete namespace '{}': {:?}", name, e);
                                    let result = OperationResult::failure(format!(
                                        "Failed to delete namespace: {}",
                                        e
                                    ));
                                    Ok((result.success, result.message, result.details))
                                }
                            }
                        },
                    );
                }

                // ── ReloadConfiguration ──
                {
                    let state = state.clone();
                    b.method(
                        "ReloadConfiguration",
                        (),
                        ("success", "message", "details"),
                        move |ctx, _cr: &mut (), ()| {
                            debug!("D-Bus method call: ReloadConfiguration");

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "reload", ctx)?;

                            match reload_all_configurations(&svc) {
                                Ok((loaded_count, error_count)) => {
                                    info!(
                                        "Configuration reload completed: {} loaded, {} errors",
                                        loaded_count, error_count
                                    );

                                    let message = if error_count == 0 {
                                        format!(
                                            "Successfully reloaded {} configurations",
                                            loaded_count
                                        )
                                    } else {
                                        format!(
                                            "Reloaded {} configurations with {} errors",
                                            loaded_count, error_count
                                        )
                                    };

                                    let result = OperationResult::success(message)
                                        .with_detail(
                                            "loaded_count".to_string(),
                                            loaded_count.to_string(),
                                        )
                                        .with_detail(
                                            "error_count".to_string(),
                                            error_count.to_string(),
                                        );
                                    Ok((result.success, result.message, result.details))
                                }
                                Err(e) => {
                                    warn!("Configuration reload failed: {:?}", e);
                                    let result = OperationResult::failure(format!(
                                        "Configuration reload failed: {}",
                                        e
                                    ));
                                    Ok((result.success, result.message, result.details))
                                }
                            }
                        },
                    );
                }

                // ── ValidateConfiguration ──
                {
                    let state = state.clone();
                    b.method(
                        "ValidateConfiguration",
                        ("config_path",),
                        ("valid", "errors", "warnings"),
                        move |ctx, _cr: &mut (), (config_path,): (String,)| {
                            debug!("D-Bus method call: ValidateConfiguration({})", config_path);

                            if let Err(e) = interface_helpers::validate_config_path(&config_path) {
                                warn!("Invalid config path '{}': {}", config_path, e.message());
                                return Err(create_method_err(SegwireError::from(e)));
                            }

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "validate", ctx)?;

                            match validate_config_file(
                                &svc,
                                &std::path::PathBuf::from(&config_path),
                            ) {
                                Ok(result) => {
                                    debug!(
                                        "Configuration validation completed for '{}'",
                                        config_path
                                    );
                                    Ok((result.valid, result.errors, result.warnings))
                                }
                                Err(e) => {
                                    warn!(
                                        "Configuration validation failed for '{}': {:?}",
                                        config_path, e
                                    );
                                    Ok((
                                        false,
                                        vec![format!("Validation failed: {}", e)],
                                        Vec::<String>::new(),
                                    ))
                                }
                            }
                        },
                    );
                }

                // ── GetDaemonStatus ──
                {
                    let state = state.clone();
                    b.method(
                        "GetDaemonStatus",
                        (),
                        ("version", "uptime", "managed_count", "active_count"),
                        move |ctx, _cr: &mut (), ()| {
                            debug!("D-Bus method call: GetDaemonStatus");

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "status", ctx)?;

                            let uptime = svc
                                .daemon_start_time
                                .elapsed()
                                .unwrap_or_default()
                                .as_secs();

                            let state_stats = {
                                let mgr = svc.state_manager.lock().unwrap();
                                mgr.get_state_stats()
                            };
                            let config_stats = {
                                let mgr = svc.config_manager.lock().unwrap();
                                mgr.get_config_stats()
                            };

                            let managed_count = config_stats.total_configs as u32;
                            let active_count = state_stats.active_namespaces as u32;
                            let version = env!("CARGO_PKG_VERSION").to_string();

                            debug!(
                                "Daemon status: version={}, uptime={}s, managed={}, active={}",
                                version, uptime, managed_count, active_count
                            );

                            Ok((version, uptime, managed_count, active_count))
                        },
                    );
                }

                // ── RestartNamespace ──
                {
                    let state = state.clone();
                    b.method(
                        "RestartNamespace",
                        ("name",),
                        ("success", "message", "details"),
                        move |ctx, _cr: &mut (), (name,): (String,)| {
                            debug!("D-Bus method call: RestartNamespace({})", name);

                            if let Err(e) = interface_helpers::validate_namespace_name(&name) {
                                warn!("Invalid namespace name '{}': {}", name, e.message());
                                return Err(create_method_err(SegwireError::from(e)));
                            }

                            let svc = state.lock().unwrap();

                            check_authorization(&svc, "restart", ctx)?;

                            // Look up the namespace configuration before deleting
                            let config = {
                                let config_mgr = svc.config_manager.lock().unwrap();
                                let full_name = config_mgr.generate_full_namespace_name(&name);
                                config_mgr
                                    .get_namespace_config(&full_name)
                                    .or_else(|| config_mgr.get_namespace_config(&name))
                                    .map(|entry| entry.config.clone())
                            };

                            let config = match config {
                                Some(c) => c,
                                None => {
                                    let result = OperationResult::failure(format!(
                                        "No configuration found for namespace '{}', cannot restart",
                                        name
                                    ));
                                    return Ok((result.success, result.message, result.details));
                                }
                            };

                            // Delete the existing namespace
                            if let Err(e) = delete_namespace_by_name(&svc, &name) {
                                warn!(
                                    "Failed to delete namespace '{}' during restart: {:?}",
                                    name, e
                                );
                                let result = OperationResult::failure(format!(
                                    "Restart failed during deletion: {}",
                                    e
                                ));
                                return Ok((result.success, result.message, result.details));
                            }

                            // Recreate from config
                            match create_namespace_from_config(&svc, config) {
                                Ok(full_name) => {
                                    info!("Successfully restarted namespace '{}'", full_name);
                                    let result = OperationResult::success(format!(
                                        "Namespace '{}' restarted successfully",
                                        full_name
                                    ));
                                    Ok((result.success, result.message, result.details))
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to recreate namespace '{}' during restart: {:?}",
                                        name, e
                                    );
                                    let result = OperationResult::failure(format!(
                                        "Restart failed during recreation: {}",
                                        e
                                    ));
                                    Ok((result.success, result.message, result.details))
                                }
                            }
                        },
                    );
                }
            })
        };

        // Register the object path with the crossroads dispatcher
        cr.insert(interface::OBJECT_PATH, &[iface_token], ());

        // Start serving — hand the crossroads dispatcher to the connection
        connection.start_receive(
            dbus::message::MatchRule::new_method_call(),
            Box::new(move |msg, conn| {
                cr.handle_message(msg, conn).unwrap();
                true
            }),
        );

        log_info!(ctx, "D-Bus service registered successfully");

        Ok(Self { connection })
    }

    /// Process incoming D-Bus messages for a given duration.
    ///
    /// Call this from the event loop to dispatch method calls. Returns when the
    /// timeout expires or a message was processed.
    pub fn process(&self, timeout: std::time::Duration) -> Result<(), SegwireError> {
        self.connection
            .process(timeout)
            .map_err(|e| SegwireError::DBus(e.to_string()))?;
        Ok(())
    }

    /// Emit a namespace created signal
    pub fn emit_namespace_created(
        &self,
        name: &str,
        config_path: &str,
    ) -> Result<(), SegwireError> {
        let signal = dbus::Message::new_signal(
            interface::OBJECT_PATH,
            interface::INTERFACE_NAME,
            interface::SIGNAL_NAMESPACE_CREATED,
        )
        .map_err(SegwireError::DBus)?;

        let signal = signal.append2(name, config_path);

        self.connection
            .channel()
            .send(signal)
            .map_err(|_| SegwireError::DBus("Failed to send signal".to_string()))?;

        debug!("Emitted NamespaceCreated signal for {}", name);
        Ok(())
    }

    /// Emit a namespace deleted signal
    pub fn emit_namespace_deleted(&self, name: &str, reason: &str) -> Result<(), SegwireError> {
        let signal = dbus::Message::new_signal(
            interface::OBJECT_PATH,
            interface::INTERFACE_NAME,
            interface::SIGNAL_NAMESPACE_DELETED,
        )
        .map_err(SegwireError::DBus)?;

        let signal = signal.append2(name, reason);

        self.connection
            .channel()
            .send(signal)
            .map_err(|_| SegwireError::DBus("Failed to send signal".to_string()))?;

        debug!("Emitted NamespaceDeleted signal for {}", name);
        Ok(())
    }

    /// Emit an operation progress signal
    pub fn emit_operation_progress(
        &self,
        operation: &str,
        progress: f64,
        message: &str,
    ) -> Result<(), SegwireError> {
        let signal = dbus::Message::new_signal(
            interface::OBJECT_PATH,
            interface::INTERFACE_NAME,
            interface::SIGNAL_OPERATION_PROGRESS,
        )
        .map_err(SegwireError::DBus)?;

        let signal = signal.append3(operation, progress, message);

        self.connection
            .channel()
            .send(signal)
            .map_err(|_| SegwireError::DBus("Failed to send signal".to_string()))?;

        debug!(
            "Emitted OperationProgress signal: {} - {:.1}% - {}",
            operation,
            progress * 100.0,
            message
        );
        Ok(())
    }

    /// Emit a namespace status changed signal
    pub fn emit_namespace_status_changed(
        &self,
        name: &str,
        old_status: &str,
        new_status: &str,
    ) -> Result<(), SegwireError> {
        let signal = dbus::Message::new_signal(
            interface::OBJECT_PATH,
            interface::INTERFACE_NAME,
            interface::SIGNAL_NAMESPACE_STATUS_CHANGED,
        )
        .map_err(SegwireError::DBus)?;

        let signal = signal.append3(name, old_status, new_status);

        self.connection
            .channel()
            .send(signal)
            .map_err(|_| SegwireError::DBus("Failed to send signal".to_string()))?;

        debug!(
            "Emitted NamespaceStatusChanged signal for {}: {} -> {}",
            name, old_status, new_status
        );
        Ok(())
    }
}

// ─── Free functions used by method handlers ───

/// Check authorization for a D-Bus method call
fn check_authorization(
    svc: &ServiceState,
    action: &str,
    ctx: &mut dbus_crossroads::Context,
) -> Result<(), dbus_crossroads::MethodErr> {
    let sender = ctx
        .message()
        .sender()
        .map(|s| s.to_string())
        .unwrap_or_default();

    if let Some(ref authorizer) = svc.authorizer {
        authorizer
            .check_authorization(action, &sender)
            .map_err(|e| {
                warn!("Authorization failed for {}: {:?}", action, e);
                create_method_err(e)
            })
    } else {
        Ok(())
    }
}

/// Create a dbus-crossroads MethodErr from a SegwireError
fn create_method_err(error: SegwireError) -> dbus_crossroads::MethodErr {
    let dbus_error = DbusError::from(error);
    dbus_crossroads::MethodErr::failed(&dbus_error.message().to_string())
}

/// Load and parse a namespace configuration file
fn load_namespace_config(
    config_path: &std::path::Path,
) -> Result<segwire_common::NamespaceConfig, SegwireError> {
    use segwire_common::NamespaceConfig;
    use std::fs;

    debug!("Loading namespace config from: {}", config_path.display());

    let config_content = fs::read_to_string(config_path).map_err(SegwireError::System)?;

    let config: NamespaceConfig = toml::from_str(&config_content)
        .map_err(|e| SegwireError::Config(segwire_common::error::ConfigError::InvalidToml(e)))?;

    debug!(
        "Successfully loaded config for namespace: {}",
        config.namespace.name
    );
    Ok(config)
}

/// Create a namespace from configuration
fn create_namespace_from_config(
    svc: &ServiceState,
    config: segwire_common::NamespaceConfig,
) -> Result<String, SegwireError> {
    use crate::netlink::NetlinkManager;

    let full_name = {
        let config_mgr = svc.config_manager.lock().unwrap();
        config_mgr.generate_full_namespace_name(&config.namespace.name)
    };

    debug!("Creating namespace '{}' from configuration", full_name);

    let netlink_manager = NetlinkManager::new()?;
    netlink_manager.create_namespace(&full_name)?;

    let config_path = {
        let config_mgr = svc.config_manager.lock().unwrap();
        config_mgr
            .get_namespace_config(&full_name)
            .map(|entry| entry.file_path.clone())
            .unwrap_or_default()
    };

    let namespace_state = NamespaceState::new(
        config.namespace.name.clone(),
        full_name.clone(),
        config_path,
    );

    {
        let mut manager = svc.state_manager.lock().unwrap();
        manager.update_namespace_state(namespace_state);
    }

    info!("Successfully created namespace '{}'", full_name);
    Ok(full_name)
}

/// Delete a namespace by name
fn delete_namespace_by_name(svc: &ServiceState, name: &str) -> Result<(), SegwireError> {
    use crate::netlink::NetlinkManager;

    let full_name = {
        let config_mgr = svc.config_manager.lock().unwrap();
        config_mgr.generate_full_namespace_name(name)
    };

    {
        let manager = svc.state_manager.lock().unwrap();
        if manager.get_namespace_state(name).is_none()
            && manager.get_namespace_state(&full_name).is_none()
        {
            return Err(SegwireError::Network(format!(
                "Namespace '{}' not found",
                name
            )));
        }
    }

    debug!("Deleting namespace '{}'", full_name);

    let netlink_manager = NetlinkManager::new()?;
    netlink_manager.delete_namespace(&full_name)?;

    {
        let mut manager = svc.state_manager.lock().unwrap();
        manager.remove_namespace_state(name);
        manager.remove_namespace_state(&full_name);
    }

    info!("Successfully deleted namespace '{}'", full_name);
    Ok(())
}

/// Reload all configuration files and return counts
fn reload_all_configurations(svc: &ServiceState) -> Result<(u32, u32), SegwireError> {
    debug!("Starting configuration reload process");

    let config_dir = {
        let manager = svc.config_manager.lock().unwrap();
        manager.config_directory().to_path_buf()
    };

    let config_files = scan_config_directory(&config_dir)?;
    let total_files = config_files.len() as u32;
    let mut loaded_count = 0u32;
    let mut error_count = 0u32;

    debug!("Found {} configuration files to process", total_files);

    for config_file in config_files.iter() {
        match load_namespace_config(config_file) {
            Ok(_config) => {
                loaded_count += 1;
                debug!("Successfully loaded config: {}", config_file.display());
            }
            Err(e) => {
                error_count += 1;
                warn!("Failed to load config {}: {:?}", config_file.display(), e);
            }
        }
    }

    info!(
        "Configuration reload completed: {} loaded, {} errors",
        loaded_count, error_count
    );
    Ok((loaded_count, error_count))
}

/// Scan configuration directory for TOML files
fn scan_config_directory(
    config_dir: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, SegwireError> {
    use std::fs;

    debug!("Scanning configuration directory: {}", config_dir.display());

    if !config_dir.exists() {
        return Err(SegwireError::System(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Configuration directory not found: {}",
                config_dir.display()
            ),
        )));
    }

    let mut config_files = Vec::new();

    let entries = fs::read_dir(config_dir).map_err(SegwireError::System)?;

    for entry in entries {
        let entry = entry.map_err(SegwireError::System)?;
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            config_files.push(path);
        }
    }

    config_files.sort();
    debug!("Found {} configuration files", config_files.len());
    Ok(config_files)
}

/// Validate a configuration file and return validation result
fn validate_config_file(
    _svc: &ServiceState,
    config_path: &std::path::Path,
) -> Result<ValidationResult, SegwireError> {
    debug!("Validating configuration file: {}", config_path.display());

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !config_path.exists() {
        errors.push(format!(
            "Configuration file does not exist: {}",
            config_path.display()
        ));
        return Ok(ValidationResult {
            valid: false,
            errors,
            warnings,
        });
    }

    match load_namespace_config(config_path) {
        Ok(config) => {
            debug!(
                "Configuration syntax is valid for: {}",
                config.namespace.name
            );

            if let Err(e) = config.validate() {
                errors.push(e.to_string());
            }

            if config.interfaces.move_interfaces.is_empty()
                && config.interfaces.virtual_interfaces.is_empty()
            {
                warnings.push("No interfaces specified for namespace".to_string());
            }

            if config.dns.servers.is_empty() {
                warnings.push("No DNS servers specified".to_string());
            }

            let is_valid = errors.is_empty();
            debug!(
                "Configuration validation result: valid={}, errors={}, warnings={}",
                is_valid,
                errors.len(),
                warnings.len()
            );

            Ok(ValidationResult {
                valid: is_valid,
                errors,
                warnings,
            })
        }
        Err(e) => {
            errors.push(format!("Configuration parsing failed: {}", e));
            Ok(ValidationResult {
                valid: false,
                errors,
                warnings,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use segwire_common::error::ConfigError;

    #[test]
    fn test_create_method_err() {
        let err = SegwireError::Config(ConfigError::InvalidToml(
            toml::from_str::<toml::Value>("invalid = [").unwrap_err(),
        ));
        let method_err = create_method_err(err);
        // MethodErr implements Display
        let msg = format!("{}", method_err);
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_create_method_err_generic() {
        let err = SegwireError::System(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file missing",
        ));
        let method_err = create_method_err(err);
        let msg = format!("{}", method_err);
        assert!(msg.contains("file missing"));
    }
}
