//! D-Bus service implementation for the segwire daemon
//!
//! Provides the D-Bus interface for CLI communication, including service registration,
//! method call handling, and PolicyKit integration for authorization.

use async_lock::Mutex as AsyncMutex;
use std::sync::Arc;
use std::time::SystemTime;

use segwire_common::dbus::{
    interface, interface_helpers, method_signatures, DbusError, NamespaceState, OperationResult,
    ValidationResult,
};
use segwire_common::error::{ErrorContext, SegwireError};
use segwire_common::{log_info, LogContext};
use tracing::{debug, info, warn};
use zbus::{Connection, ConnectionBuilder, SignalContext};

pub struct DbusService {
    connection: Connection,
}

impl DbusService {
    /// Create a new D-Bus service instance
    pub async fn new(
        config_manager: Arc<AsyncMutex<crate::config::ConfigManager>>,
        state_manager: Arc<AsyncMutex<crate::namespace_state::NamespaceStateManager>>,
    ) -> Result<Self, SegwireError> {
        let (config_dir, namespace_prefix) = {
            let manager = config_manager.lock().await;
            (manager.get_config_dir(), manager.get_namespace_prefix())
        };

        let ctx = LogContext::new("dbus_service_initialization")
            .with_field("service_name", interface::SERVICE_NAME)
            .with_field("object_path", interface::OBJECT_PATH)
            .with_field("namespace_prefix", namespace_prefix.clone())
            .with_field("config_dir", config_dir.display().to_string());

        log_info!(ctx, "Initializing D-Bus service");

        // Build D-Bus connection with service registration
        // In simulation mode, use the session bus for unprivileged testing
        let connection = if std::env::var("SEGWIRE_SIMULATION").is_ok() {
            log_info!(ctx, "Using session D-Bus (simulation mode)");
            ConnectionBuilder::session()
        } else {
            ConnectionBuilder::system()
        }
        .map_err(|e| {
            let error_ctx = ErrorContext::new("dbus_connection_builder")
                .with_field("service_name", interface::SERVICE_NAME)
                .with_remediation("Ensure D-Bus system bus is available")
                .with_remediation("Check that the daemon has permission to access the system bus");
            SegwireError::DBus(e)
                .with_context(error_ctx)
                .log_and_return()
        })?
        .name(interface::SERVICE_NAME)
        .map_err(|e| {
            let error_ctx = ErrorContext::new("dbus_service_name_registration")
                .with_field("service_name", interface::SERVICE_NAME)
                .with_remediation("Ensure no other instance of segwire-daemon is running")
                .with_remediation("Check D-Bus service configuration");
            SegwireError::DBus(e)
                .with_context(error_ctx)
                .log_and_return()
        })?
        .build()
        .await
        .map_err(|e| {
            let error_ctx = ErrorContext::new("dbus_connection_build")
                .with_remediation("Ensure D-Bus system bus is running")
                .with_remediation("Check system D-Bus configuration");
            SegwireError::DBus(e)
                .with_context(error_ctx)
                .log_and_return()
        })?;

        let authorizer = crate::policykit::PolicyKitAuthorizer::new(connection.clone());

        let connection_clone = connection.clone();

        connection
            .object_server()
            .at(
                interface::OBJECT_PATH,
                NamespaceManagerInterface {
                    connection: connection_clone,
                    config_manager: config_manager.clone(),
                    state_manager: state_manager.clone(),
                    daemon_start_time: SystemTime::now(),
                    authorizer,
                },
            )
            .await
            .map_err(|e| {
                let error_ctx = ErrorContext::new("dbus_object_registration")
                    .with_field("object_path", interface::OBJECT_PATH)
                    .with_remediation("Check D-Bus object path permissions");
                SegwireError::DBus(e)
                    .with_context(error_ctx)
                    .log_and_return()
            })?;

        log_info!(ctx, "D-Bus service registered successfully");

        Ok(Self { connection })
    }

    /// Emit a namespace created signal
    pub async fn emit_namespace_created(
        &self,
        name: &str,
        config_path: &str,
    ) -> Result<(), SegwireError> {
        let _object_server = self.connection.object_server();

        NamespaceManagerInterface::namespace_created(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            name,
            config_path,
        )
        .await?;

        debug!("Emitted NamespaceCreated signal for {}", name);
        Ok(())
    }

    /// Emit a namespace deleted signal
    pub async fn emit_namespace_deleted(
        &self,
        name: &str,
        reason: &str,
    ) -> Result<(), SegwireError> {
        NamespaceManagerInterface::namespace_deleted(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            name,
            reason,
        )
        .await?;

        debug!("Emitted NamespaceDeleted signal for {}", name);
        Ok(())
    }

    /// Emit an operation progress signal
    pub async fn emit_operation_progress(
        &self,
        operation: &str,
        progress: f64,
        message: &str,
    ) -> Result<(), SegwireError> {
        NamespaceManagerInterface::operation_progress(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            operation,
            progress,
            message,
        )
        .await?;

        debug!(
            "Emitted OperationProgress signal: {} - {:.1}% - {}",
            operation,
            progress * 100.0,
            message
        );
        Ok(())
    }

    /// Emit a namespace status changed signal
    pub async fn emit_namespace_status_changed(
        &self,
        name: &str,
        old_status: &str,
        new_status: &str,
    ) -> Result<(), SegwireError> {
        NamespaceManagerInterface::namespace_status_changed(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            name,
            old_status,
            new_status,
        )
        .await?;

        debug!(
            "Emitted NamespaceStatusChanged signal for {}: {} -> {}",
            name, old_status, new_status
        );
        Ok(())
    }
}

/// D-Bus interface implementation
struct NamespaceManagerInterface {
    connection: Connection,
    config_manager: Arc<AsyncMutex<crate::config::ConfigManager>>,
    state_manager: Arc<AsyncMutex<crate::namespace_state::NamespaceStateManager>>,
    daemon_start_time: SystemTime,
    authorizer: crate::policykit::PolicyKitAuthorizer,
}

impl NamespaceManagerInterface {
    async fn emit_error(&self, error_type: &str, message: &str, namespace: &str) {
        if let Ok(ctx) = SignalContext::new(&self.connection, interface::OBJECT_PATH) {
            if let Err(e) = Self::error_occurred(&ctx, error_type, message, namespace).await {
                warn!("Failed to emit ErrorOccurred signal: {:?}", e);
            }
        }
    }
}

#[zbus::dbus_interface(name = "org.segwire.NamespaceManager")]
impl NamespaceManagerInterface {
    /// List all managed namespaces with basic information
    async fn list_namespaces(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
    ) -> zbus::fdo::Result<method_signatures::ListNamespacesResult> {
        debug!("D-Bus method call: ListNamespaces");

        // Check authorization
        if let Err(e) = self.check_authorization("list", &header).await {
            warn!("Authorization failed for ListNamespaces: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let prefix = {
            let config_mgr = self.config_manager.lock().await;
            config_mgr.get_namespace_prefix()
        };

        let manager = self.state_manager.lock().await;
        let namespaces: Vec<_> = manager
            .get_all_states()
            .values()
            .map(|ns| {
                (
                    ns.name.clone(),
                    ns.status.clone(),
                    ns.config_path.clone(),
                    format!("Namespace managed by {}", prefix),
                )
            })
            .collect();

        debug!("Returning {} namespaces", namespaces.len());
        Ok(namespaces)
    }

    /// Get detailed status information for a specific namespace
    async fn get_namespace_status(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
        name: String,
    ) -> zbus::fdo::Result<method_signatures::GetNamespaceStatusResult> {
        debug!("D-Bus method call: GetNamespaceStatus({})", name);

        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("status", &header).await {
            warn!("Authorization failed for GetNamespaceStatus: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let full_name = {
            let config_mgr = self.config_manager.lock().await;
            config_mgr.generate_full_namespace_name(&name)
        };

        let manager = self.state_manager.lock().await;
        match manager
            .get_namespace_state(&full_name)
            .or_else(|| manager.get_namespace_state(&name))
        {
            Some(namespace) => {
                debug!(
                    "Found namespace '{}' with status '{}'",
                    name, namespace.status
                );
                Ok(namespace.clone())
            }
            None => {
                warn!("Namespace '{}' not found", name);
                Err(create_fdo_error(SegwireError::from(
                    DbusError::NamespaceNotFound(format!("Namespace '{}' not found", name)),
                )))
            }
        }
    }

    /// Create a namespace from a configuration file
    async fn create_namespace(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
        config_path: String,
    ) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: CreateNamespace({})", config_path);

        // Validate input
        if let Err(e) = interface_helpers::validate_config_path(&config_path) {
            warn!("Invalid config path '{}': {}", config_path, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("create", &header).await {
            warn!("Authorization failed for CreateNamespace: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Load and parse the configuration file
        let config_path_buf = std::path::PathBuf::from(&config_path);
        let namespace_config = match self.load_namespace_config(&config_path_buf).await {
            Ok(config) => config,
            Err(e) => {
                warn!(
                    "Failed to load namespace config from '{}': {:?}",
                    config_path, e
                );
                self.emit_error("ConfigurationError", &e.to_string(), "")
                    .await;
                return Ok(OperationResult::failure(format!(
                    "Failed to load configuration: {}",
                    e
                )));
            }
        };

        // Create the namespace
        match self.create_namespace_from_config(namespace_config).await {
            Ok(namespace_name) => {
                info!("Successfully created namespace '{}'", namespace_name);
                Ok(OperationResult::success(format!(
                    "Namespace '{}' created successfully",
                    namespace_name
                ))
                .with_detail("namespace".to_string(), namespace_name))
            }
            Err(e) => {
                warn!("Failed to create namespace from '{}': {:?}", config_path, e);
                self.emit_error("CreationError", &e.to_string(), &config_path)
                    .await;
                Ok(OperationResult::failure(format!(
                    "Failed to create namespace: {}",
                    e
                )))
            }
        }
    }

    /// Delete a managed namespace
    async fn delete_namespace(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
        name: String,
    ) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: DeleteNamespace({})", name);

        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("delete", &header).await {
            warn!("Authorization failed for DeleteNamespace: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Delete the namespace
        match self.delete_namespace_by_name(&name).await {
            Ok(()) => {
                info!("Successfully deleted namespace '{}'", name);
                Ok(
                    OperationResult::success(format!("Namespace '{}' deleted successfully", name))
                        .with_detail("namespace".to_string(), name),
                )
            }
            Err(e) => {
                warn!("Failed to delete namespace '{}': {:?}", name, e);
                self.emit_error("DeletionError", &e.to_string(), &name)
                    .await;
                Ok(OperationResult::failure(format!(
                    "Failed to delete namespace: {}",
                    e
                )))
            }
        }
    }

    /// Reload all configuration files and update namespaces
    async fn reload_configuration(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
    ) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: ReloadConfiguration");

        // Check authorization
        if let Err(e) = self.check_authorization("reload", &header).await {
            warn!("Authorization failed for ReloadConfiguration: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Reload configuration files
        match self.reload_all_configurations().await {
            Ok((loaded_count, error_count)) => {
                info!(
                    "Configuration reload completed: {} loaded, {} errors",
                    loaded_count, error_count
                );

                if let Err(e) = NamespaceManagerInterface::configuration_reloaded(
                    &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
                    loaded_count,
                    error_count,
                )
                .await
                {
                    warn!("Failed to emit configuration_reloaded signal: {:?}", e);
                }

                let message = if error_count == 0 {
                    format!("Successfully reloaded {} configurations", loaded_count)
                } else {
                    format!(
                        "Reloaded {} configurations with {} errors",
                        loaded_count, error_count
                    )
                };

                Ok(OperationResult::success(message)
                    .with_detail("loaded_count".to_string(), loaded_count.to_string())
                    .with_detail("error_count".to_string(), error_count.to_string()))
            }
            Err(e) => {
                warn!("Configuration reload completed with errors: {:?}", e);
                self.emit_error("ReloadError", &e.to_string(), "").await;
                Ok(OperationResult::failure(format!(
                    "Configuration reload failed: {}",
                    e
                )))
            }
        }
    }

    /// Validate a configuration file without applying it
    async fn validate_configuration(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
        config_path: String,
    ) -> zbus::fdo::Result<method_signatures::ConfigValidationResult> {
        debug!("D-Bus method call: ValidateConfiguration({})", config_path);

        // Validate input
        if let Err(e) = interface_helpers::validate_config_path(&config_path) {
            warn!("Invalid config path '{}': {}", config_path, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("validate", &header).await {
            warn!("Authorization failed for ValidateConfiguration: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Validate the configuration file
        match self
            .validate_config_file(&std::path::PathBuf::from(&config_path))
            .await
        {
            Ok(validation_result) => {
                debug!("Configuration validation completed for '{}'", config_path);
                Ok(validation_result)
            }
            Err(e) => {
                warn!(
                    "Configuration validation failed for '{}': {:?}",
                    config_path, e
                );
                Ok(ValidationResult {
                    valid: false,
                    errors: vec![format!("Validation failed: {}", e)],
                    warnings: vec![],
                })
            }
        }
    }

    /// Get daemon status and statistics
    async fn get_daemon_status(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
    ) -> zbus::fdo::Result<method_signatures::DaemonStatusResult> {
        debug!("D-Bus method call: GetDaemonStatus");

        // Check authorization
        if let Err(e) = self.check_authorization("status", &header).await {
            warn!("Authorization failed for GetDaemonStatus: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let uptime = self
            .daemon_start_time
            .elapsed()
            .unwrap_or_default()
            .as_secs();

        // Get stats from state_manager and config_manager
        let state_stats = {
            let mgr = self.state_manager.lock().await;
            mgr.get_state_stats()
        };
        let config_stats = {
            let mgr = self.config_manager.lock().await;
            mgr.get_config_stats()
        };

        let managed_count = config_stats.total_configs as u32;
        let active_count = state_stats.active_namespaces as u32;

        let version = env!("CARGO_PKG_VERSION").to_string();

        debug!(
            "Daemon status: version={}, uptime={}s, managed={}, active={}, total_tracked={}",
            version, uptime, managed_count, active_count, state_stats.total_namespaces
        );

        Ok((version, uptime, managed_count, active_count))
    }

    /// Restart a specific namespace (delete and recreate)
    async fn restart_namespace(
        &self,
        #[zbus(header)] header: zbus::MessageHeader<'_>,
        name: String,
    ) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: RestartNamespace({})", name);

        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("restart", &header).await {
            warn!("Authorization failed for RestartNamespace: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // TODO: Implement actual namespace restart logic
        // For now, return a placeholder implementation
        warn!("RestartNamespace not fully implemented yet");
        Ok(OperationResult::failure(
            "RestartNamespace method not yet implemented".to_string(),
        ))
    }

    /// D-Bus signal definitions
    #[dbus_interface(signal)]
    async fn namespace_created(
        ctx: &SignalContext<'_>,
        name: &str,
        config_path: &str,
    ) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn namespace_deleted(
        ctx: &SignalContext<'_>,
        name: &str,
        reason: &str,
    ) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn configuration_reloaded(
        ctx: &SignalContext<'_>,
        count: u32,
        errors: u32,
    ) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn operation_progress(
        ctx: &SignalContext<'_>,
        operation: &str,
        progress: f64,
        message: &str,
    ) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn namespace_status_changed(
        ctx: &SignalContext<'_>,
        name: &str,
        old_status: &str,
        new_status: &str,
    ) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn error_occurred(
        ctx: &SignalContext<'_>,
        error_type: &str,
        message: &str,
        namespace: &str,
    ) -> zbus::Result<()>;
}

impl NamespaceManagerInterface {
    async fn check_authorization(
        &self,
        action: &str,
        header: &zbus::MessageHeader<'_>,
    ) -> Result<(), SegwireError> {
        let sender_result = header.sender().map_err(SegwireError::DBus)?;
        let sender = sender_result
            .ok_or_else(|| SegwireError::Permission("Unknown D-Bus sender".to_string()))?;

        self.authorizer.check_authorization(action, sender).await
    }

    /// Load and parse a namespace configuration file
    async fn load_namespace_config(
        &self,
        config_path: &std::path::Path,
    ) -> Result<segwire_common::NamespaceConfig, SegwireError> {
        use segwire_common::NamespaceConfig;
        use std::fs;

        debug!("Loading namespace config from: {}", config_path.display());

        // Read the configuration file
        let config_content = fs::read_to_string(config_path).map_err(SegwireError::System)?;

        // Parse the TOML configuration
        let config: NamespaceConfig = toml::from_str(&config_content).map_err(|e| {
            SegwireError::Config(segwire_common::error::ConfigError::InvalidToml(e))
        })?;

        debug!(
            "Successfully loaded config for namespace: {}",
            config.namespace.name
        );
        Ok(config)
    }

    /// Create a namespace from a parsed configuration
    async fn create_namespace_from_config(
        &self,
        config: segwire_common::NamespaceConfig,
    ) -> Result<String, SegwireError> {
        use segwire_common::netlink::NetlinkManager;

        let full_name = {
            let config_mgr = self.config_manager.lock().await;
            config_mgr.generate_full_namespace_name(&config.namespace.name)
        };

        debug!("Creating namespace '{}' from configuration", full_name);

        // Create the network namespace using netlink
        let netlink_manager = NetlinkManager::new()?;
        netlink_manager.create_namespace(&full_name)?;

        // Create namespace state for tracking
        let namespace_state = NamespaceState::new(
            config.namespace.name.clone(),
            full_name.clone(),
            std::path::PathBuf::from(""), // Will be set properly when integrated with config manager
        );

        // Add to managed namespaces
        {
            let mut manager = self.state_manager.lock().await;
            manager.update_namespace_state(namespace_state);
        }

        info!("Successfully created namespace '{}'", full_name);
        Ok(full_name)
    }

    /// Delete a namespace by name
    async fn delete_namespace_by_name(&self, name: &str) -> Result<(), SegwireError> {
        use segwire_common::netlink::NetlinkManager;

        let full_name = {
            let config_mgr = self.config_manager.lock().await;
            config_mgr.generate_full_namespace_name(name)
        };

        // Check if namespace exists in our managed set
        {
            let manager = self.state_manager.lock().await;
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

        // Delete the network namespace using netlink
        let netlink_manager = NetlinkManager::new()?;
        netlink_manager.delete_namespace(&full_name)?;

        // Remove from managed namespaces
        {
            let mut manager = self.state_manager.lock().await;
            manager.remove_namespace_state(name);
            manager.remove_namespace_state(&full_name);
        }

        info!("Successfully deleted namespace '{}'", full_name);
        Ok(())
    }

    /// Reload all configuration files and return counts
    async fn reload_all_configurations(&self) -> Result<(u32, u32), SegwireError> {
        debug!("Starting configuration reload process");

        // Get the configuration directory
        let config_dir = {
            let manager = self.config_manager.lock().await;
            manager.get_config_dir()
        };

        // Scan for configuration files
        let config_files = self.scan_config_directory(&config_dir).await?;
        let total_files = config_files.len() as u32;
        let mut loaded_count = 0u32;
        let mut error_count = 0u32;

        debug!("Found {} configuration files to process", total_files);

        // Process each configuration file
        for (index, config_file) in config_files.iter().enumerate() {
            let progress = (index as f64) / (total_files as f64);

            // Emit progress signal
            if let Err(e) = self
                .emit_progress_signal(
                    "reload_configuration",
                    progress,
                    &format!("Processing {}", config_file.display()),
                )
                .await
            {
                warn!("Failed to emit progress signal: {:?}", e);
            }

            // Try to load and validate the configuration
            match self.load_namespace_config(config_file).await {
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

        // Emit completion signal
        if let Err(e) = self
            .emit_progress_signal(
                "reload_configuration",
                1.0,
                "Configuration reload completed",
            )
            .await
        {
            warn!("Failed to emit completion signal: {:?}", e);
        }

        info!(
            "Configuration reload completed: {} loaded, {} errors",
            loaded_count, error_count
        );
        Ok((loaded_count, error_count))
    }

    /// Scan configuration directory for TOML files
    async fn scan_config_directory(
        &self,
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

            // Only process .toml files
            if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
                config_files.push(path);
            }
        }

        config_files.sort();
        debug!("Found {} configuration files", config_files.len());
        Ok(config_files)
    }

    /// Validate a configuration file and return validation result
    async fn validate_config_file(
        &self,
        config_path: &std::path::Path,
    ) -> Result<ValidationResult, SegwireError> {
        debug!("Validating configuration file: {}", config_path.display());

        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // Check if file exists and is readable
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

        // Try to load and parse the configuration
        match self.load_namespace_config(config_path).await {
            Ok(config) => {
                debug!(
                    "Configuration syntax is valid for: {}",
                    config.namespace.name
                );

                // Perform semantic validation
                self.validate_config_semantics(&config, &mut errors, &mut warnings);

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

    /// Validate configuration semantics and add errors/warnings
    fn validate_config_semantics(
        &self,
        config: &segwire_common::NamespaceConfig,
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
    ) {
        // Validate namespace name
        if config.namespace.name.is_empty() {
            errors.push("Namespace name cannot be empty".to_string());
        }

        if config.namespace.name.len() > 255 {
            errors.push("Namespace name is too long (max 255 characters)".to_string());
        }

        // Check for valid characters in namespace name
        if !config
            .namespace
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            errors.push("Namespace name contains invalid characters (only alphanumeric, hyphens, and underscores allowed)".to_string());
        }

        // Validate interfaces
        if config.interfaces.move_interfaces.is_empty()
            && config.interfaces.virtual_interfaces.is_empty()
        {
            warnings.push("No interfaces specified for namespace".to_string());
        }

        for interface in &config.interfaces.move_interfaces {
            if interface.is_empty() {
                errors.push("Interface name cannot be empty".to_string());
            }
        }

        for virtual_interface in &config.interfaces.virtual_interfaces {
            if virtual_interface.name.is_empty() {
                errors.push("Virtual interface name cannot be empty".to_string());
            }
            if virtual_interface.interface_type.is_empty() {
                errors.push("Virtual interface type cannot be empty".to_string());
            }
        }

        // Validate routing
        for route in &config.routing.routes {
            if route.destination.is_empty() {
                errors.push("Route destination cannot be empty".to_string());
            }
            if route.gateway.is_empty() {
                errors.push("Route gateway cannot be empty".to_string());
            }
        }

        // Validate DNS
        if config.dns.servers.is_empty() {
            warnings.push("No DNS servers specified".to_string());
        }

        for server in &config.dns.servers {
            if server.is_empty() {
                errors.push("DNS server address cannot be empty".to_string());
            }
        }
    }

    /// Emit a progress signal for long-running operations
    async fn emit_progress_signal(
        &self,
        operation: &str,
        progress: f64,
        message: &str,
    ) -> Result<(), SegwireError> {
        // This would normally emit a D-Bus signal, but for now we'll just log
        debug!(
            "Progress: {} - {:.1}% - {}",
            operation,
            progress * 100.0,
            message
        );
        // TODO: Emit actual D-Bus signal when signal emission is properly implemented
        /*
        NamespaceManagerInterface::operation_progress(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            operation,
            progress,
            message,
        ).await?;
        */

        Ok(())
    }
}

/// Helper function to create D-Bus errors from SegwireError
fn create_fdo_error(error: SegwireError) -> zbus::fdo::Error {
    let dbus_error = segwire_common::dbus::DbusError::from(error);
    zbus::fdo::Error::Failed(dbus_error.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use segwire_common::error::ConfigError;

    #[test]
    fn test_create_fdo_error() {
        let err = SegwireError::Config(ConfigError::InvalidToml(
            toml::from_str::<toml::Value>("invalid = [").unwrap_err(),
        ));
        let fdo_err = create_fdo_error(err);
        match fdo_err {
            zbus::fdo::Error::Failed(msg) => {
                assert!(msg.contains("Invalid TOML syntax"));
            }
            _ => panic!("Expected Failed error"),
        }
    }

    #[test]
    fn test_create_fdo_error_generic() {
        let err = SegwireError::System(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file missing",
        ));
        let fdo_err = create_fdo_error(err);
        match fdo_err {
            zbus::fdo::Error::Failed(msg) => {
                assert!(msg.contains("file missing"));
            }
            _ => panic!("Expected Failed error"),
        }
    }
}
