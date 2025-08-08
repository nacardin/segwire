//! D-Bus service implementation for the segwire daemon
//! 
//! Provides the D-Bus interface for CLI communication, including service registration,
//! method call handling, and PolicyKit integration for authorization.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use segwire_common::dbus::{
    interface, method_signatures, DbusError, NamespaceState, OperationResult,
    ValidationResult, interface_helpers,
};
use segwire_common::error::SegwireError;
use tracing::{debug, info, warn};
use zbus::{Connection, ConnectionBuilder, SignalContext};

/// D-Bus service for the segwire daemon
pub struct DbusService {
    connection: Connection,
    namespace_manager: Arc<Mutex<NamespaceManager>>,
    daemon_start_time: SystemTime,
}

/// Namespace manager state for D-Bus operations
pub struct NamespaceManager {
    namespaces: HashMap<String, NamespaceState>,
    config_dir: std::path::PathBuf,
    namespace_prefix: String,
}

impl DbusService {
    /// Create a new D-Bus service instance
    pub async fn new(
        config_dir: std::path::PathBuf,
        namespace_prefix: String,
    ) -> Result<Self, SegwireError> {
        info!("Initializing D-Bus service");

        // Create namespace manager
        let namespace_manager = Arc::new(Mutex::new(NamespaceManager {
            namespaces: HashMap::new(),
            config_dir,
            namespace_prefix,
        }));

        // Build D-Bus connection with service registration
        let connection = ConnectionBuilder::system()?
            .name(interface::SERVICE_NAME)?
            .serve_at(interface::OBJECT_PATH, NamespaceManagerInterface {
                namespace_manager: namespace_manager.clone(),
                daemon_start_time: SystemTime::now(),
            })?
            .build()
            .await?;

        info!("D-Bus service registered at {}", interface::SERVICE_NAME);

        Ok(Self {
            connection,
            namespace_manager,
            daemon_start_time: SystemTime::now(),
        })
    }

    /// Get the D-Bus connection for signal emission
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Get the namespace manager for internal operations
    pub fn namespace_manager(&self) -> Arc<Mutex<NamespaceManager>> {
        self.namespace_manager.clone()
    }

    /// Run the D-Bus service event loop
    pub async fn run(&self) -> Result<(), SegwireError> {
        info!("Starting D-Bus service event loop");
        
        // The connection will handle incoming method calls automatically
        // We just need to keep the service running
        loop {
            monoio::time::sleep(std::time::Duration::from_secs(1)).await;
            // Service runs in background, this is just to keep the task alive
        }
    }

    /// Emit a namespace created signal
    pub async fn emit_namespace_created(&self, name: &str, config_path: &str) -> Result<(), SegwireError> {
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
    pub async fn emit_namespace_deleted(&self, name: &str, reason: &str) -> Result<(), SegwireError> {
        NamespaceManagerInterface::namespace_deleted(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            name,
            reason,
        )
        .await?;
        
        debug!("Emitted NamespaceDeleted signal for {}", name);
        Ok(())
    }

    /// Emit a configuration reloaded signal
    pub async fn emit_configuration_reloaded(&self, count: u32, errors: u32) -> Result<(), SegwireError> {
        NamespaceManagerInterface::configuration_reloaded(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            count,
            errors,
        )
        .await?;
        
        debug!("Emitted ConfigurationReloaded signal: {} configs, {} errors", count, errors);
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
        
        debug!("Emitted OperationProgress signal: {} - {:.1}% - {}", operation, progress * 100.0, message);
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
        
        debug!("Emitted NamespaceStatusChanged signal for {}: {} -> {}", name, old_status, new_status);
        Ok(())
    }

    /// Emit an error occurred signal
    pub async fn emit_error_occurred(
        &self,
        error_type: &str,
        message: &str,
        namespace: &str,
    ) -> Result<(), SegwireError> {
        NamespaceManagerInterface::error_occurred(
            &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
            error_type,
            message,
            namespace,
        )
        .await?;
        
        debug!("Emitted ErrorOccurred signal: {} - {} (namespace: {})", error_type, message, namespace);
        Ok(())
    }
}

/// D-Bus interface implementation
struct NamespaceManagerInterface {
    namespace_manager: Arc<Mutex<NamespaceManager>>,
    daemon_start_time: SystemTime,
}

#[zbus::dbus_interface(name = "org.segwire.NamespaceManager")]
impl NamespaceManagerInterface {
    /// List all managed namespaces with basic information
    async fn list_namespaces(&self) -> zbus::fdo::Result<method_signatures::ListNamespacesResult> {
        debug!("D-Bus method call: ListNamespaces");
        
        // Check authorization
        if let Err(e) = self.check_authorization("list").await {
            warn!("Authorization failed for ListNamespaces: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let manager = self.namespace_manager.lock().unwrap();
        let namespaces: Vec<_> = manager
            .namespaces
            .values()
            .map(|ns| {
                (
                    ns.name.clone(),
                    ns.status.clone(),
                    ns.config_path.clone(),
                    format!("Namespace managed by {}", manager.namespace_prefix),
                )
            })
            .collect();

        debug!("Returning {} namespaces", namespaces.len());
        Ok(namespaces)
    }

    /// Get detailed status information for a specific namespace
    async fn get_namespace_status(&self, name: String) -> zbus::fdo::Result<method_signatures::GetNamespaceStatusResult> {
        debug!("D-Bus method call: GetNamespaceStatus({})", name);
        
        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("status").await {
            warn!("Authorization failed for GetNamespaceStatus: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let manager = self.namespace_manager.lock().unwrap();
        match manager.namespaces.get(&name) {
            Some(namespace) => {
                debug!("Found namespace '{}' with status '{}'", name, namespace.status);
                Ok(namespace.clone())
            }
            None => {
                warn!("Namespace '{}' not found", name);
                Err(create_fdo_error(SegwireError::from(DbusError::NamespaceNotFound(
                    format!("Namespace '{}' not found", name),
                ))))
            }
        }
    }

    /// Create a namespace from a configuration file
    async fn create_namespace(&self, config_path: String) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: CreateNamespace({})", config_path);
        
        // Validate input
        if let Err(e) = interface_helpers::validate_config_path(&config_path) {
            warn!("Invalid config path '{}': {}", config_path, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("create").await {
            warn!("Authorization failed for CreateNamespace: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Load and parse the configuration file
        let config_path_buf = std::path::PathBuf::from(&config_path);
        let namespace_config = match self.load_namespace_config(&config_path_buf).await {
            Ok(config) => config,
            Err(e) => {
                warn!("Failed to load namespace config from '{}': {:?}", config_path, e);
                return Ok(OperationResult::failure(
                    format!("Failed to load configuration: {}", e)
                ));
            }
        };

        // Create the namespace
        match self.create_namespace_from_config(namespace_config).await {
            Ok(namespace_name) => {
                info!("Successfully created namespace '{}'", namespace_name);
                Ok(OperationResult::success(
                    format!("Namespace '{}' created successfully", namespace_name)
                ).with_detail("namespace".to_string(), namespace_name))
            }
            Err(e) => {
                warn!("Failed to create namespace from '{}': {:?}", config_path, e);
                Ok(OperationResult::failure(
                    format!("Failed to create namespace: {}", e)
                ))
            }
        }
    }

    /// Delete a managed namespace
    async fn delete_namespace(&self, name: String) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: DeleteNamespace({})", name);
        
        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("delete").await {
            warn!("Authorization failed for DeleteNamespace: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Delete the namespace
        match self.delete_namespace_by_name(&name).await {
            Ok(()) => {
                info!("Successfully deleted namespace '{}'", name);
                Ok(OperationResult::success(
                    format!("Namespace '{}' deleted successfully", name)
                ).with_detail("namespace".to_string(), name))
            }
            Err(e) => {
                warn!("Failed to delete namespace '{}': {:?}", name, e);
                Ok(OperationResult::failure(
                    format!("Failed to delete namespace: {}", e)
                ))
            }
        }
    }

    /// Reload all configuration files and update namespaces
    async fn reload_configuration(&self) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: ReloadConfiguration");
        
        // Check authorization
        if let Err(e) = self.check_authorization("reload").await {
            warn!("Authorization failed for ReloadConfiguration: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Reload configuration files
        match self.reload_all_configurations().await {
            Ok((loaded_count, error_count)) => {
                info!("Configuration reload completed: {} loaded, {} errors", loaded_count, error_count);
                
                let message = if error_count == 0 {
                    format!("Successfully reloaded {} configurations", loaded_count)
                } else {
                    format!("Reloaded {} configurations with {} errors", loaded_count, error_count)
                };
                
                Ok(OperationResult::success(message)
                    .with_detail("loaded_count".to_string(), loaded_count.to_string())
                    .with_detail("error_count".to_string(), error_count.to_string()))
            }
            Err(e) => {
                warn!("Configuration reload failed: {:?}", e);
                Ok(OperationResult::failure(
                    format!("Configuration reload failed: {}", e)
                ))
            }
        }
    }

    /// Validate a configuration file without applying it
    async fn validate_configuration(&self, config_path: String) -> zbus::fdo::Result<method_signatures::ConfigValidationResult> {
        debug!("D-Bus method call: ValidateConfiguration({})", config_path);
        
        // Validate input
        if let Err(e) = interface_helpers::validate_config_path(&config_path) {
            warn!("Invalid config path '{}': {}", config_path, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("validate").await {
            warn!("Authorization failed for ValidateConfiguration: {:?}", e);
            return Err(create_fdo_error(e));
        }

        // Validate the configuration file
        match self.validate_config_file(&std::path::PathBuf::from(&config_path)).await {
            Ok(validation_result) => {
                debug!("Configuration validation completed for '{}'", config_path);
                Ok(validation_result)
            }
            Err(e) => {
                warn!("Configuration validation failed for '{}': {:?}", config_path, e);
                Ok(ValidationResult {
                    valid: false,
                    errors: vec![format!("Validation failed: {}", e)],
                    warnings: vec![],
                })
            }
        }
    }

    /// Get daemon status and statistics
    async fn get_daemon_status(&self) -> zbus::fdo::Result<method_signatures::DaemonStatusResult> {
        debug!("D-Bus method call: GetDaemonStatus");
        
        // Check authorization
        if let Err(e) = self.check_authorization("status").await {
            warn!("Authorization failed for GetDaemonStatus: {:?}", e);
            return Err(create_fdo_error(e));
        }

        let manager = self.namespace_manager.lock().unwrap();
        let uptime = self.daemon_start_time
            .elapsed()
            .unwrap_or_default()
            .as_secs();
        
        let managed_count = manager.namespaces.len() as u32;
        let active_count = manager
            .namespaces
            .values()
            .filter(|ns| ns.is_active())
            .count() as u32;

        let version = env!("CARGO_PKG_VERSION").to_string();
        
        debug!("Daemon status: version={}, uptime={}s, managed={}, active={}", 
               version, uptime, managed_count, active_count);
        
        Ok((version, uptime, managed_count, active_count))
    }

    /// Restart a specific namespace (delete and recreate)
    async fn restart_namespace(&self, name: String) -> zbus::fdo::Result<method_signatures::StandardOperationResult> {
        debug!("D-Bus method call: RestartNamespace({})", name);
        
        // Validate input
        if let Err(e) = interface_helpers::validate_namespace_name(&name) {
            warn!("Invalid namespace name '{}': {}", name, e.message());
            return Err(create_fdo_error(SegwireError::from(e)));
        }

        // Check authorization
        if let Err(e) = self.check_authorization("restart").await {
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
    async fn namespace_created(ctx: &SignalContext<'_>, name: &str, config_path: &str) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn namespace_deleted(ctx: &SignalContext<'_>, name: &str, reason: &str) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn configuration_reloaded(ctx: &SignalContext<'_>, count: u32, errors: u32) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn operation_progress(ctx: &SignalContext<'_>, operation: &str, progress: f64, message: &str) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn namespace_status_changed(ctx: &SignalContext<'_>, name: &str, old_status: &str, new_status: &str) -> zbus::Result<()>;

    #[dbus_interface(signal)]
    async fn error_occurred(ctx: &SignalContext<'_>, error_type: &str, message: &str, namespace: &str) -> zbus::Result<()>;
}

impl NamespaceManagerInterface {
    /// Check authorization for D-Bus method calls using PolicyKit
    async fn check_authorization(&self, action: &str) -> Result<(), SegwireError> {
        // TODO: Implement actual PolicyKit integration
        // For now, we'll do basic permission checking
        
        debug!("Checking authorization for action: {}", action);
        
        // In a real implementation, this would:
        // 1. Get the calling process UID/PID from D-Bus message context
        // 2. Call PolicyKit to check permissions for the specific action
        // 3. Return appropriate error if not authorized
        
        // For now, we'll allow all operations but log the check
        debug!("Authorization check passed for action: {} (placeholder implementation)", action);
        Ok(())
    }

    /// Load and parse a namespace configuration file
    async fn load_namespace_config(&self, config_path: &std::path::Path) -> Result<segwire_common::NamespaceConfig, SegwireError> {
        use std::fs;
        use segwire_common::NamespaceConfig;
        
        debug!("Loading namespace config from: {}", config_path.display());
        
        // Read the configuration file
        let config_content = fs::read_to_string(config_path)
            .map_err(|e| SegwireError::System(e))?;
        
        // Parse the TOML configuration
        let config: NamespaceConfig = toml::from_str(&config_content)
            .map_err(|e| SegwireError::Config(segwire_common::error::ConfigError::InvalidToml(e)))?;
        
        debug!("Successfully loaded config for namespace: {}", config.namespace.name);
        Ok(config)
    }

    /// Create a namespace from a parsed configuration
    async fn create_namespace_from_config(&self, config: segwire_common::NamespaceConfig) -> Result<String, SegwireError> {
        use segwire_common::netlink::NetlinkManager;
        
        let full_name = {
            let manager = self.namespace_manager.lock().unwrap();
            format!("{}-{}", manager.namespace_prefix, config.namespace.name)
        };
        
        debug!("Creating namespace '{}' from configuration", full_name);
        
        // Create the network namespace using netlink
        let netlink_manager = NetlinkManager::new().await?;
        netlink_manager.create_namespace(&full_name).await?;
        
        // Create namespace state for tracking
        let namespace_state = NamespaceState::new(
            config.namespace.name.clone(),
            full_name.clone(),
            std::path::PathBuf::from(""), // Will be set properly when integrated with config manager
        );
        
        // Add to managed namespaces
        {
            let mut manager = self.namespace_manager.lock().unwrap();
            manager.add_namespace(namespace_state);
        }
        
        info!("Successfully created namespace '{}'", full_name);
        Ok(full_name)
    }

    /// Delete a namespace by name
    async fn delete_namespace_by_name(&self, name: &str) -> Result<(), SegwireError> {
        use segwire_common::netlink::NetlinkManager;
        
        let full_name = {
            let manager = self.namespace_manager.lock().unwrap();
            let full_name = if name.starts_with(&manager.namespace_prefix) {
                name.to_string()
            } else {
                format!("{}-{}", manager.namespace_prefix, name)
            };
            
            // Check if namespace exists in our managed set
            if !manager.namespaces.contains_key(name) && !manager.namespaces.contains_key(&full_name) {
                return Err(SegwireError::Network(format!("Namespace '{}' not found", name)));
            }
            
            full_name
        };
        
        debug!("Deleting namespace '{}'", full_name);
        
        // Delete the network namespace using netlink
        let netlink_manager = NetlinkManager::new().await?;
        netlink_manager.delete_namespace(&full_name).await?;
        
        // Remove from managed namespaces
        {
            let mut manager = self.namespace_manager.lock().unwrap();
            manager.remove_namespace(name);
            manager.remove_namespace(&full_name);
        }
        
        info!("Successfully deleted namespace '{}'", full_name);
        Ok(())
    }

    /// Reload all configuration files and return counts
    async fn reload_all_configurations(&self) -> Result<(u32, u32), SegwireError> {
        debug!("Starting configuration reload process");
        
        // Get the configuration directory
        let config_dir = {
            let manager = self.namespace_manager.lock().unwrap();
            manager.config_dir.clone()
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
            if let Err(e) = self.emit_progress_signal(
                "reload_configuration",
                progress,
                &format!("Processing {}", config_file.display())
            ).await {
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
        if let Err(e) = self.emit_progress_signal(
            "reload_configuration",
            1.0,
            "Configuration reload completed"
        ).await {
            warn!("Failed to emit completion signal: {:?}", e);
        }
        
        info!("Configuration reload completed: {} loaded, {} errors", loaded_count, error_count);
        Ok((loaded_count, error_count))
    }

    /// Scan configuration directory for TOML files
    async fn scan_config_directory(&self, config_dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, SegwireError> {
        use std::fs;
        
        debug!("Scanning configuration directory: {}", config_dir.display());
        
        if !config_dir.exists() {
            return Err(SegwireError::System(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Configuration directory not found: {}", config_dir.display())
            )));
        }
        
        let mut config_files = Vec::new();
        
        let entries = fs::read_dir(config_dir)
            .map_err(|e| SegwireError::System(e))?;
        
        for entry in entries {
            let entry = entry.map_err(|e| SegwireError::System(e))?;
            let path = entry.path();
            
            // Only process .toml files
            if path.is_file() && path.extension().map_or(false, |ext| ext == "toml") {
                config_files.push(path);
            }
        }
        
        config_files.sort();
        debug!("Found {} configuration files", config_files.len());
        Ok(config_files)
    }

    /// Validate a configuration file and return validation result
    async fn validate_config_file(&self, config_path: &std::path::Path) -> Result<ValidationResult, SegwireError> {
        debug!("Validating configuration file: {}", config_path.display());
        
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        
        // Check if file exists and is readable
        if !config_path.exists() {
            errors.push(format!("Configuration file does not exist: {}", config_path.display()));
            return Ok(ValidationResult {
                valid: false,
                errors,
                warnings,
            });
        }
        
        // Try to load and parse the configuration
        match self.load_namespace_config(config_path).await {
            Ok(config) => {
                debug!("Configuration syntax is valid for: {}", config.namespace.name);
                
                // Perform semantic validation
                self.validate_config_semantics(&config, &mut errors, &mut warnings);
                
                let is_valid = errors.is_empty();
                debug!("Configuration validation result: valid={}, errors={}, warnings={}", 
                       is_valid, errors.len(), warnings.len());
                
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
    fn validate_config_semantics(&self, config: &segwire_common::NamespaceConfig, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
        // Validate namespace name
        if config.namespace.name.is_empty() {
            errors.push("Namespace name cannot be empty".to_string());
        }
        
        if config.namespace.name.len() > 255 {
            errors.push("Namespace name is too long (max 255 characters)".to_string());
        }
        
        // Check for valid characters in namespace name
        if !config.namespace.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            errors.push("Namespace name contains invalid characters (only alphanumeric, hyphens, and underscores allowed)".to_string());
        }
        
        // Validate interfaces
        if config.interfaces.move_interfaces.is_empty() && config.interfaces.virtual_interfaces.is_empty() {
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
    async fn emit_progress_signal(&self, operation: &str, progress: f64, message: &str) -> Result<(), SegwireError> {
        // This would normally emit a D-Bus signal, but for now we'll just log
        debug!("Progress: {} - {:.1}% - {}", operation, progress * 100.0, message);
        
        // TODO: Emit actual D-Bus signal when signal emission is properly implemented
        // NamespaceManagerInterface::operation_progress(
        //     &SignalContext::new(&self.connection, interface::OBJECT_PATH)?,
        //     operation,
        //     progress,
        //     message,
        // ).await?;
        
        Ok(())
    }
}

impl NamespaceManager {
    /// Add a namespace to the managed set
    pub fn add_namespace(&mut self, namespace: NamespaceState) {
        debug!("Adding namespace '{}' to managed set", namespace.name);
        self.namespaces.insert(namespace.name.clone(), namespace);
    }

    /// Remove a namespace from the managed set
    pub fn remove_namespace(&mut self, name: &str) -> Option<NamespaceState> {
        debug!("Removing namespace '{}' from managed set", name);
        self.namespaces.remove(name)
    }

    /// Get a namespace by name
    pub fn get_namespace(&self, name: &str) -> Option<&NamespaceState> {
        self.namespaces.get(name)
    }

    /// Get a mutable reference to a namespace by name
    pub fn get_namespace_mut(&mut self, name: &str) -> Option<&mut NamespaceState> {
        self.namespaces.get_mut(name)
    }

    /// List all managed namespaces
    pub fn list_namespaces(&self) -> Vec<&NamespaceState> {
        self.namespaces.values().collect()
    }

    /// Get the count of managed namespaces
    pub fn namespace_count(&self) -> usize {
        self.namespaces.len()
    }

    /// Get the count of active namespaces
    pub fn active_namespace_count(&self) -> usize {
        self.namespaces.values().filter(|ns| ns.is_active()).count()
    }
}

/// Helper function to create D-Bus errors from SegwireError
fn create_fdo_error(error: SegwireError) -> zbus::fdo::Error {
    let dbus_error = DbusError::from(error);
    zbus::fdo::Error::Failed(dbus_error.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[monoio::test]
    async fn test_namespace_manager_operations() {
        let mut manager = NamespaceManager {
            namespaces: HashMap::new(),
            config_dir: PathBuf::from("/tmp"),
            namespace_prefix: "test".to_string(),
        };

        // Test adding namespace
        let ns = NamespaceState::new(
            "test-ns".to_string(),
            "test-test-ns".to_string(),
            PathBuf::from("/tmp/test.toml"),
        );
        manager.add_namespace(ns);

        assert_eq!(manager.namespace_count(), 1);
        assert!(manager.get_namespace("test-ns").is_some());

        // Test removing namespace
        let removed = manager.remove_namespace("test-ns");
        assert!(removed.is_some());
        assert_eq!(manager.namespace_count(), 0);
    }

    #[test]
    fn test_input_validation() {
        // Test namespace name validation
        assert!(interface_helpers::validate_namespace_name("valid-name").is_ok());
        assert!(interface_helpers::validate_namespace_name("valid_name").is_ok());
        assert!(interface_helpers::validate_namespace_name("validname123").is_ok());
        
        assert!(interface_helpers::validate_namespace_name("").is_err());
        assert!(interface_helpers::validate_namespace_name("invalid name").is_err());
        assert!(interface_helpers::validate_namespace_name("invalid@name").is_err());

        // Test config path validation
        assert!(interface_helpers::validate_config_path("/path/to/config.toml").is_ok());
        assert!(interface_helpers::validate_config_path("config.toml").is_ok());
        
        assert!(interface_helpers::validate_config_path("").is_err());
        assert!(interface_helpers::validate_config_path("config.txt").is_err());
        assert!(interface_helpers::validate_config_path("../config.toml").is_err());
    }
}