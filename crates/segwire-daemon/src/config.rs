//! Configuration management for segwire-daemon
//! 
//! Handles loading and managing daemon configuration, including master configuration
//! and namespace configuration scanning and monitoring.

use segwire_common::{
    config::{DaemonConfig, NamespaceConfig},
    error::{ConfigError, SegwireResult},
};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn, error, debug};
use monoio::time::sleep;

/// Configuration manager for the daemon
pub struct ConfigManager {
    /// Master daemon configuration
    daemon_config: DaemonConfig,
    
    /// Path to the master configuration file
    config_file_path: PathBuf,
    
    /// Currently loaded namespace configurations
    namespace_configs: HashMap<String, NamespaceConfigEntry>,
}

/// Entry for a namespace configuration with metadata
#[derive(Debug, Clone)]
pub struct NamespaceConfigEntry {
    /// The parsed configuration
    pub config: NamespaceConfig,
    
    /// Path to the configuration file
    pub file_path: PathBuf,
    
    /// Full namespace name (with prefix)
    pub full_name: String,
    
    /// Last modification time of the file
    pub last_modified: std::time::SystemTime,
}

impl ConfigManager {
    /// Create a new configuration manager
    pub fn new(config_file_path: PathBuf) -> SegwireResult<Self> {
        info!("Loading daemon configuration from: {}", config_file_path.display());
        
        let daemon_config = Self::load_master_config(&config_file_path)?;
        
        info!(
            "Daemon configuration loaded successfully. Namespace prefix: '{}', Config directory: '{}'",
            daemon_config.daemon.namespace_prefix,
            daemon_config.daemon.config_dir.display()
        );
        
        Ok(Self {
            daemon_config,
            config_file_path,
            namespace_configs: HashMap::new(),
        })
    }
    
    /// Load master daemon configuration from file
    fn load_master_config(config_path: &Path) -> SegwireResult<DaemonConfig> {
        debug!("Reading master configuration file: {}", config_path.display());
        
        // Check if file exists
        if !config_path.exists() {
            error!("Master configuration file not found: {}", config_path.display());
            return Err(ConfigError::FileNotFound(config_path.display().to_string()).into());
        }
        
        // Check file permissions (should be readable)
        match std::fs::metadata(config_path) {
            Ok(metadata) => {
                if metadata.permissions().readonly() {
                    warn!("Configuration file is read-only: {}", config_path.display());
                }
            }
            Err(e) => {
                error!("Failed to read file metadata for {}: {}", config_path.display(), e);
                return Err(ConfigError::FileNotFound(config_path.display().to_string()).into());
            }
        }
        
        // Load and parse configuration
        let config = DaemonConfig::from_file(config_path)
            .map_err(|e| {
                error!("Failed to parse master configuration: {}", e);
                e
            })?;
        
        // Validate configuration directory exists
        if !config.daemon.config_dir.exists() {
            error!("Configuration directory does not exist: {}", config.daemon.config_dir.display());
            return Err(ConfigError::InvalidValue {
                field: "daemon.config_dir".to_string(),
                value: config.daemon.config_dir.display().to_string(),
            }.into());
        }
        
        // Validate configuration directory is readable
        match std::fs::read_dir(&config.daemon.config_dir) {
            Ok(_) => {
                debug!("Configuration directory is accessible: {}", config.daemon.config_dir.display());
            }
            Err(e) => {
                error!("Configuration directory is not readable: {} - {}", config.daemon.config_dir.display(), e);
                return Err(ConfigError::InvalidValue {
                    field: "daemon.config_dir".to_string(),
                    value: format!("Directory not readable: {}", e),
                }.into());
            }
        }
        
        debug!("Master configuration loaded and validated successfully");
        Ok(config)
    }
    
    /// Get the daemon configuration
    pub fn daemon_config(&self) -> &DaemonConfig {
        &self.daemon_config
    }
    
    /// Get the namespace prefix for this daemon instance
    pub fn namespace_prefix(&self) -> &str {
        &self.daemon_config.daemon.namespace_prefix
    }
    
    /// Get the namespace prefix for this daemon instance (alternative method name)
    pub fn get_namespace_prefix(&self) -> String {
        self.daemon_config.daemon.namespace_prefix.clone()
    }
    
    /// Get the configuration directory path
    pub fn config_directory(&self) -> &Path {
        &self.daemon_config.daemon.config_dir
    }
    
    /// Get the configuration directory path (alternative method name)
    pub fn get_config_dir(&self) -> PathBuf {
        self.daemon_config.daemon.config_dir.clone()
    }
    
    /// Get all currently loaded namespace configurations
    pub fn namespace_configs(&self) -> &HashMap<String, NamespaceConfigEntry> {
        &self.namespace_configs
    }
    
    /// Get a specific namespace configuration by name
    pub fn get_namespace_config(&self, name: &str) -> Option<&NamespaceConfigEntry> {
        self.namespace_configs.get(name)
    }
    
    /// Reload the master configuration from disk
    pub fn reload_master_config(&mut self) -> SegwireResult<()> {
        info!("Reloading master configuration from: {}", self.config_file_path.display());
        
        let new_config = Self::load_master_config(&self.config_file_path)?;
        
        // Check if namespace prefix changed
        if new_config.daemon.namespace_prefix != self.daemon_config.daemon.namespace_prefix {
            warn!(
                "Namespace prefix changed from '{}' to '{}' - this may require daemon restart",
                self.daemon_config.daemon.namespace_prefix,
                new_config.daemon.namespace_prefix
            );
        }
        
        // Check if config directory changed
        if new_config.daemon.config_dir != self.daemon_config.daemon.config_dir {
            info!(
                "Configuration directory changed from '{}' to '{}'",
                self.daemon_config.daemon.config_dir.display(),
                new_config.daemon.config_dir.display()
            );
        }
        
        self.daemon_config = new_config;
        
        info!("Master configuration reloaded successfully");
        Ok(())
    }
    
    /// Validate that a namespace name matches this daemon's prefix
    pub fn matches_namespace_prefix(&self, namespace_name: &str) -> bool {
        let prefix = &self.daemon_config.daemon.namespace_prefix;
        
        // Check if the namespace name starts with the prefix followed by a separator
        if namespace_name.starts_with(prefix) {
            // If the name is exactly the prefix, it matches
            if namespace_name.len() == prefix.len() {
                return true;
            }
            
            // If there's more after the prefix, it should be separated by a dash or underscore
            let remaining = &namespace_name[prefix.len()..];
            remaining.starts_with('-') || remaining.starts_with('_')
        } else {
            false
        }
    }
    
    /// Generate the full namespace name with prefix
    pub fn generate_full_namespace_name(&self, config_name: &str) -> String {
        let prefix = &self.daemon_config.daemon.namespace_prefix;
        
        // If the config name already starts with the prefix, use it as-is
        if self.matches_namespace_prefix(config_name) {
            config_name.to_string()
        } else {
            // Otherwise, prepend the prefix with a dash separator
            format!("{}-{}", prefix, config_name)
        }
    }
    
    /// Get configuration file path resolution
    pub fn resolve_config_path(&self, filename: &str) -> PathBuf {
        self.daemon_config.daemon.config_dir.join(filename)
    }
    
    /// Check if the daemon should cleanup namespaces on shutdown
    pub fn should_cleanup_on_shutdown(&self) -> bool {
        self.daemon_config.daemon.cleanup_on_shutdown
    }
    
    /// Get the configured log level
    pub fn log_level(&self) -> &str {
        &self.daemon_config.daemon.logging.level
    }
    

    /// Get D-Bus service configuration
    pub fn dbus_service_name(&self) -> &str {
        &self.daemon_config.dbus.service_name
    }
    
    /// Get D-Bus object path
    pub fn dbus_object_path(&self) -> &str {
        &self.daemon_config.dbus.object_path
    }
    
    /// Scan the configuration directory for namespace configuration files
    pub fn scan_namespace_configs(&mut self) -> SegwireResult<Vec<String>> {
        info!("Scanning configuration directory: {}", self.config_directory().display());
        
        let config_dir = self.config_directory();
        
        // Read directory contents
        let entries = std::fs::read_dir(config_dir)
            .map_err(|e| {
                error!("Failed to read configuration directory {}: {}", config_dir.display(), e);
                ConfigError::InvalidValue {
                    field: "config_dir".to_string(),
                    value: format!("Cannot read directory: {}", e),
                }
            })?;
        
        let mut loaded_configs = Vec::new();
        let mut new_namespace_configs = HashMap::new();
        
        for entry in entries {
            let entry = entry.map_err(|e| {
                error!("Failed to read directory entry: {}", e);
                ConfigError::InvalidValue {
                    field: "config_dir".to_string(),
                    value: format!("Cannot read directory entry: {}", e),
                }
            })?;
            
            let path = entry.path();
            
            // Skip non-files
            if !path.is_file() {
                debug!("Skipping non-file: {}", path.display());
                continue;
            }
            
            // Only process .toml files
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                debug!("Skipping non-TOML file: {}", path.display());
                continue;
            }
            
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            
            debug!("Processing configuration file: {}", path.display());
            
            match self.load_namespace_config(&path) {
                Ok(Some(entry)) => {
                    info!("Loaded namespace configuration: {} -> {}", filename, entry.full_name);
                    loaded_configs.push(entry.full_name.clone());
                    new_namespace_configs.insert(entry.full_name.clone(), entry);
                }
                Ok(None) => {
                    debug!("Skipped configuration file {} (doesn't match prefix)", filename);
                }
                Err(e) => {
                    error!("Failed to load configuration file {}: {}", filename, e);
                    // Continue processing other files instead of failing completely
                    continue;
                }
            }
        }
        
        // Update the stored configurations
        self.namespace_configs = new_namespace_configs;
        
        info!("Configuration scan complete. Loaded {} namespace configurations", loaded_configs.len());
        Ok(loaded_configs)
    }
    
    /// Load a single namespace configuration file
    fn load_namespace_config(&self, config_path: &Path) -> SegwireResult<Option<NamespaceConfigEntry>> {
        debug!("Loading namespace configuration: {}", config_path.display());
        
        // Get file metadata for modification time
        let metadata = std::fs::metadata(config_path)
            .map_err(|e| {
                error!("Failed to read metadata for {}: {}", config_path.display(), e);
                ConfigError::FileNotFound(config_path.display().to_string())
            })?;
        
        let last_modified = metadata.modified()
            .map_err(|e| {
                error!("Failed to get modification time for {}: {}", config_path.display(), e);
                ConfigError::InvalidValue {
                    field: "file_metadata".to_string(),
                    value: format!("Cannot get modification time: {}", e),
                }
            })?;
        
        // Load and parse the configuration
        let config = NamespaceConfig::from_file(config_path)
            .map_err(|e| {
                error!("Failed to parse namespace configuration {}: {}", config_path.display(), e);
                e
            })?;
        
        // Check if this namespace should be managed by this daemon
        // If the name already has a prefix, it must match our prefix
        // If the name doesn't have a prefix, we'll add ours
        let should_manage = if config.namespace.name.contains('-') || config.namespace.name.contains('_') {
            // Name already has separators, check if it matches our prefix
            self.matches_namespace_prefix(&config.namespace.name)
        } else {
            // Name doesn't have separators, we'll manage it by adding our prefix
            true
        };
        
        if !should_manage {
            debug!(
                "Namespace '{}' doesn't match prefix '{}', skipping",
                config.namespace.name,
                self.namespace_prefix()
            );
            return Ok(None);
        }
        
        let full_name = self.generate_full_namespace_name(&config.namespace.name);
        
        debug!("Successfully loaded namespace configuration: {}", full_name);
        
        Ok(Some(NamespaceConfigEntry {
            config,
            file_path: config_path.to_path_buf(),
            full_name,
            last_modified,
        }))
    }
    
    /// Reload a specific namespace configuration file
    pub fn reload_namespace_config(&mut self, config_path: &Path) -> SegwireResult<Option<String>> {
        info!("Reloading namespace configuration: {}", config_path.display());
        
        match self.load_namespace_config(config_path) {
            Ok(Some(entry)) => {
                let full_name = entry.full_name.clone();
                
                // Check if this is an update to an existing configuration
                if let Some(existing) = self.namespace_configs.get(&full_name) {
                    if existing.last_modified != entry.last_modified {
                        info!("Configuration updated: {}", full_name);
                    } else {
                        debug!("Configuration unchanged: {}", full_name);
                    }
                } else {
                    info!("New configuration loaded: {}", full_name);
                }
                
                self.namespace_configs.insert(full_name.clone(), entry);
                Ok(Some(full_name))
            }
            Ok(None) => {
                debug!("Configuration doesn't match prefix, ignoring");
                Ok(None)
            }
            Err(e) => {
                error!("Failed to reload configuration {}: {}", config_path.display(), e);
                Err(e)
            }
        }
    }
    
    /// Remove a namespace configuration (when file is deleted)
    pub fn remove_namespace_config(&mut self, config_path: &Path) -> Option<String> {
        // Find the configuration entry by file path
        let mut removed_name = None;
        
        self.namespace_configs.retain(|name, entry| {
            if entry.file_path == config_path {
                removed_name = Some(name.clone());
                info!("Removed namespace configuration: {}", name);
                false
            } else {
                true
            }
        });
        
        removed_name
    }
    
    /// Get detailed error information for configuration parsing
    pub fn validate_namespace_config_file(&self, config_path: &Path) -> SegwireResult<Vec<String>> {
        let mut errors = Vec::new();
        
        debug!("Validating namespace configuration file: {}", config_path.display());
        
        // Check if file exists and is readable
        if !config_path.exists() {
            errors.push(format!("File does not exist: {}", config_path.display()));
            return Ok(errors);
        }
        
        if !config_path.is_file() {
            errors.push(format!("Path is not a file: {}", config_path.display()));
            return Ok(errors);
        }
        
        // Try to read the file
        let content = match std::fs::read_to_string(config_path) {
            Ok(content) => content,
            Err(e) => {
                errors.push(format!("Cannot read file: {}", e));
                return Ok(errors);
            }
        };
        
        // Try to parse as TOML
        let config = match toml::from_str::<NamespaceConfig>(&content) {
            Ok(mut config) => {
                // Try environment variable substitution
                if let Err(e) = config.substitute_environment_variables() {
                    errors.push(format!("Environment variable substitution failed: {}", e));
                    return Ok(errors);
                }
                config
            }
            Err(e) => {
                errors.push(format!("TOML parsing failed: {}", e));
                return Ok(errors);
            }
        };
        
        // Validate the configuration
        if let Err(e) = config.validate() {
            errors.push(format!("Configuration validation failed: {}", e));
        }
        
        // Check namespace prefix matching - only check if the name already has a prefix
        if config.namespace.name.contains('-') || config.namespace.name.contains('_') {
            // If the name already has separators, check if it matches our prefix
            if !self.matches_namespace_prefix(&config.namespace.name) {
                errors.push(format!(
                    "Namespace '{}' doesn't match daemon prefix '{}'",
                    config.namespace.name,
                    self.namespace_prefix()
                ));
            }
        }
        // If the name doesn't have separators, it will get the prefix added automatically
        
        if errors.is_empty() {
            info!("Configuration file validation successful: {}", config_path.display());
        } else {
            warn!("Configuration file validation failed: {} errors", errors.len());
        }
        
        Ok(errors)
    }
    
    /// Get statistics about loaded configurations
    pub fn get_config_stats(&self) -> ConfigStats {
        ConfigStats {
            total_configs: self.namespace_configs.len(),
            namespace_prefix: self.namespace_prefix().to_string(),
            config_directory: self.config_directory().to_path_buf(),
            loaded_namespaces: self.namespace_configs.keys().cloned().collect(),
        }
    }
}

/// Statistics about loaded configurations
#[derive(Debug, Clone)]
pub struct ConfigStats {
    pub total_configs: usize,
    pub namespace_prefix: String,
    pub config_directory: PathBuf,
    pub loaded_namespaces: Vec<String>,
}

/// File system event types for configuration monitoring
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

/// Configuration file monitor using monoio for io_uring-based file watching
pub struct ConfigFileMonitor {
    config_dir: PathBuf,
    debounce_duration: Duration,
    last_events: HashMap<PathBuf, Instant>,
}

impl ConfigFileMonitor {
    /// Create a new configuration file monitor
    pub fn new(config_dir: PathBuf, debounce_duration: Duration) -> Self {
        Self {
            config_dir,
            debounce_duration,
            last_events: HashMap::new(),
        }
    }
    
    /// Start monitoring configuration files for changes
    /// Returns a receiver for debounced file system events
    pub async fn start_monitoring(&mut self) -> SegwireResult<std::sync::mpsc::Receiver<ConfigFileEvent>> {
        info!("Starting configuration file monitoring for directory: {}", self.config_dir.display());
        
        // Create a channel for file system events
        let (tx, rx) = std::sync::mpsc::channel();
        
        // Start the file system watcher task
        let config_dir = self.config_dir.clone();
        let debounce_duration = self.debounce_duration;
        
        monoio::spawn(async move {
            if let Err(e) = Self::watch_directory(config_dir, debounce_duration, tx).await {
                error!("File system monitoring failed: {}", e);
            }
        });
        
        Ok(rx)
    }
    
    /// Watch a directory for file system changes using polling with async sleep
    async fn watch_directory(
        config_dir: PathBuf,
        debounce_duration: Duration,
        tx: std::sync::mpsc::Sender<ConfigFileEvent>,
    ) -> SegwireResult<()> {
        let mut known_files = HashMap::new();
        let mut last_scan = Instant::now();
        
        // Initial scan to establish baseline
        if let Ok(entries) = std::fs::read_dir(&config_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                            if let Ok(modified) = metadata.modified() {
                                known_files.insert(path, modified);
                            }
                        }
                    }
                }
            }
        }
        
        info!("Initial scan found {} configuration files", known_files.len());
        
        // Polling loop with async sleep
        loop {
            sleep(Duration::from_millis(500)).await; // Poll every 500ms
            
            let scan_start = Instant::now();
            let mut current_files = HashMap::new();
            let mut events = Vec::new();
            
            // Scan directory for current state
            if let Ok(entries) = std::fs::read_dir(&config_dir) {
                for entry in entries.flatten() {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.is_file() {
                            let path = entry.path();
                            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                                if let Ok(modified) = metadata.modified() {
                                    current_files.insert(path.clone(), modified);
                                    
                                    // Check for new or modified files
                                    match known_files.get(&path) {
                                        None => {
                                            // New file
                                            events.push(ConfigFileEvent::Created(path));
                                        }
                                        Some(old_modified) if *old_modified != modified => {
                                            // Modified file
                                            events.push(ConfigFileEvent::Modified(path));
                                        }
                                        _ => {
                                            // Unchanged file
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            
            // Check for deleted files
            for (path, _) in &known_files {
                if !current_files.contains_key(path) {
                    events.push(ConfigFileEvent::Deleted(path.clone()));
                }
            }
            
            // Update known files
            known_files = current_files;
            
            // Send debounced events
            let now = Instant::now();
            for event in events {
                // Simple debouncing: only send event if enough time has passed since last scan
                let should_send = now.duration_since(last_scan) >= debounce_duration;
                
                if should_send {
                    debug!("Sending file system event: {:?}", event);
                    if tx.send(event).is_err() {
                        warn!("Failed to send file system event - receiver dropped");
                        break;
                    }
                }
            }
            
            last_scan = scan_start;
        }
    }
}

impl ConfigManager {
    /// Start monitoring configuration files for changes
    pub async fn start_file_monitoring(&mut self) -> SegwireResult<std::sync::mpsc::Receiver<ConfigFileEvent>> {
        let mut monitor = ConfigFileMonitor::new(
            self.config_directory().to_path_buf(),
            Duration::from_millis(1000), // 1 second debounce
        );
        
        monitor.start_monitoring().await
    }
    
    /// Handle a file system event by updating configurations
    pub async fn handle_file_event(&mut self, event: ConfigFileEvent) -> SegwireResult<Vec<String>> {
        match event {
            ConfigFileEvent::Created(path) => {
                info!("Configuration file created: {}", path.display());
                match self.reload_namespace_config(&path) {
                    Ok(Some(name)) => Ok(vec![name]),
                    Ok(None) => Ok(vec![]),
                    Err(e) => {
                        error!("Failed to load new configuration file {}: {}", path.display(), e);
                        Err(e)
                    }
                }
            }
            ConfigFileEvent::Modified(path) => {
                info!("Configuration file modified: {}", path.display());
                
                // Check if this is the master configuration file
                if path == self.config_file_path {
                    info!("Master configuration file modified, reloading");
                    self.reload_master_config()?;
                    // After reloading master config, rescan all namespace configs
                    return Ok(self.scan_namespace_configs()?);
                } else {
                    // Handle namespace configuration file modification
                    match self.reload_namespace_config(&path) {
                        Ok(Some(name)) => Ok(vec![name]),
                        Ok(None) => Ok(vec![]),
                        Err(e) => {
                            error!("Failed to reload configuration file {}: {}", path.display(), e);
                            Err(e)
                        }
                    }
                }
            }
            ConfigFileEvent::Deleted(path) => {
                info!("Configuration file deleted: {}", path.display());
                
                if let Some(removed_name) = self.remove_namespace_config(&path) {
                    Ok(vec![removed_name])
                } else {
                    Ok(vec![])
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_daemon_config(temp_dir: &TempDir, namespace_prefix: &str) -> PathBuf {
        let config_content = format!(
            r#"
[daemon]
namespace_prefix = "{}"
config_dir = "{}"
cleanup_on_shutdown = true
log_level = "info"
log_target = "stdout"

[dbus]
service_name = "org.segwire.NamespaceManager"
object_path = "/org/segwire/NamespaceManager"
"#,
            namespace_prefix,
            temp_dir.path().join("namespaces").display()
        );
        
        let config_path = temp_dir.path().join("daemon.toml");
        fs::write(&config_path, config_content).expect("Failed to write test config");
        
        // Create the namespaces directory
        fs::create_dir_all(temp_dir.path().join("namespaces")).expect("Failed to create namespaces dir");
        
        config_path
    }

    #[test]
    fn test_config_manager_creation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");
        
        let config_manager = ConfigManager::new(config_path).expect("Failed to create config manager");
        
        assert_eq!(config_manager.namespace_prefix(), "test");
        assert_eq!(config_manager.config_directory(), temp_dir.path().join("namespaces"));
        assert_eq!(config_manager.dbus_service_name(), "org.segwire.NamespaceManager");
        assert_eq!(config_manager.dbus_object_path(), "/org/segwire/NamespaceManager");
    }

    #[test]
    fn test_config_manager_missing_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("nonexistent.toml");
        
        let result = ConfigManager::new(config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_manager_invalid_config_dir() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_content = r#"
[daemon]
namespace_prefix = "test"
config_dir = "/nonexistent/directory"
cleanup_on_shutdown = true
log_level = "info"
log_target = "stdout"

[dbus]
service_name = "org.segwire.NamespaceManager"
object_path = "/org/segwire/NamespaceManager"
"#;
        
        let config_path = temp_dir.path().join("daemon.toml");
        fs::write(&config_path, config_content).expect("Failed to write test config");
        
        let result = ConfigManager::new(config_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_namespace_prefix_matching() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "segwire");
        
        let config_manager = ConfigManager::new(config_path).expect("Failed to create config manager");
        
        // Test exact prefix match
        assert!(config_manager.matches_namespace_prefix("segwire"));
        
        // Test prefix with dash separator
        assert!(config_manager.matches_namespace_prefix("segwire-app"));
        
        // Test prefix with underscore separator
        assert!(config_manager.matches_namespace_prefix("segwire_app"));
        
        // Test non-matching names
        assert!(!config_manager.matches_namespace_prefix("other-app"));
        assert!(!config_manager.matches_namespace_prefix("segwireapp")); // No separator
        assert!(!config_manager.matches_namespace_prefix("seg"));
    }

    #[test]
    fn test_full_namespace_name_generation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "segwire");
        
        let config_manager = ConfigManager::new(config_path).expect("Failed to create config manager");
        
        // Test name that doesn't have prefix
        assert_eq!(
            config_manager.generate_full_namespace_name("app"),
            "segwire-app"
        );
        
        // Test name that already has prefix
        assert_eq!(
            config_manager.generate_full_namespace_name("segwire-app"),
            "segwire-app"
        );
        
        // Test exact prefix
        assert_eq!(
            config_manager.generate_full_namespace_name("segwire"),
            "segwire"
        );
    }

    #[test]
    fn test_config_path_resolution() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");
        
        let config_manager = ConfigManager::new(config_path).expect("Failed to create config manager");
        
        let resolved_path = config_manager.resolve_config_path("app.toml");
        let expected_path = temp_dir.path().join("namespaces").join("app.toml");
        
        assert_eq!(resolved_path, expected_path);
    }

    #[test]
    fn test_config_reload() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");
        
        let mut config_manager = ConfigManager::new(config_path.clone()).expect("Failed to create config manager");
        
        assert_eq!(config_manager.namespace_prefix(), "test");
        
        // Update the configuration file
        let new_config_content = format!(
            r#"
[daemon]
namespace_prefix = "updated"
config_dir = "{}"
cleanup_on_shutdown = false
log_level = "debug"
log_target = "stderr"

[dbus]
service_name = "org.segwire.NamespaceManager"
object_path = "/org/segwire/NamespaceManager"
"#,
            temp_dir.path().join("namespaces").display()
        );
        
        fs::write(&config_path, new_config_content).expect("Failed to write updated config");
        
        // Reload configuration
        config_manager.reload_master_config().expect("Failed to reload config");
        
        assert_eq!(config_manager.namespace_prefix(), "updated");
        assert_eq!(config_manager.log_level(), "debug");
        assert_eq!(config_manager.log_target(), "stderr");
        assert!(!config_manager.should_cleanup_on_shutdown());
    }

    fn create_test_namespace_config(temp_dir: &TempDir, name: &str) -> PathBuf {
        // Use shorter names to avoid 15-character interface name limit
        let short_name = if name.len() > 6 { &name[..6] } else { name };
        let config_content = format!(
            r#"
[namespace]
name = "{}"
description = "Test namespace"

[interfaces]
move_interfaces = ["eth0"]

[[interfaces.virtual_interfaces]]
name = "v{}"
interface_type = "veth"
peer = "v{}-h"

[routing]
default_gateway = "192.168.1.1"

[[routing.routes]]
destination = "10.0.0.0/8"
gateway = "192.168.1.1"
metric = 100

[dns]
servers = ["8.8.8.8", "8.8.4.4"]
search = ["example.com"]

[environment]
APP_NAME = "{}"
"#,
            name, short_name, short_name, name
        );
        
        let config_path = temp_dir.path().join("namespaces").join(format!("{}.toml", name));
        fs::write(&config_path, config_content).expect("Failed to write test namespace config");
        
        config_path
    }

    #[test]
    fn test_namespace_config_scanning() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        // Create some test namespace configurations
        create_test_namespace_config(&temp_dir, "app1");
        create_test_namespace_config(&temp_dir, "app2");
        create_test_namespace_config(&temp_dir, "other-app"); // This should be skipped due to prefix
        
        // Create a non-TOML file that should be ignored
        let non_toml_path = temp_dir.path().join("namespaces").join("readme.txt");
        fs::write(&non_toml_path, "This is not a TOML file").expect("Failed to write non-TOML file");
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        // Scan for namespace configurations
        let loaded_configs = config_manager.scan_namespace_configs().expect("Failed to scan configs");
        
        // Should load app1 and app2, but not other-app (wrong prefix) or readme.txt (not TOML)
        assert_eq!(loaded_configs.len(), 2);
        assert!(loaded_configs.contains(&"test-app1".to_string()));
        assert!(loaded_configs.contains(&"test-app2".to_string()));
        assert!(!loaded_configs.contains(&"test-other-app".to_string()));
        
        // Check that configurations are stored
        assert_eq!(config_manager.namespace_configs().len(), 2);
        assert!(config_manager.get_namespace_config("test-app1").is_some());
        assert!(config_manager.get_namespace_config("test-app2").is_some());
        assert!(config_manager.get_namespace_config("test-other-app").is_none());
    }

    #[test]
    fn test_namespace_config_prefix_filtering() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "segwire");
        
        // Create namespace configs with different names
        create_test_namespace_config(&temp_dir, "app"); // Should become segwire-app
        create_test_namespace_config(&temp_dir, "segwire-service"); // Already has prefix
        create_test_namespace_config(&temp_dir, "other-service"); // Wrong prefix (starts with "other-")
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        let loaded_configs = config_manager.scan_namespace_configs().expect("Failed to scan configs");
        
        // Should load app (as segwire-app) and segwire-service, but not other-service
        assert_eq!(loaded_configs.len(), 2);
        assert!(loaded_configs.contains(&"segwire-app".to_string()));
        assert!(loaded_configs.contains(&"segwire-service".to_string()));
        assert!(!loaded_configs.contains(&"other-service".to_string()));
    }

    #[test]
    fn test_namespace_config_reload() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        let namespace_config_path = create_test_namespace_config(&temp_dir, "app");
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        // Initial load
        let result = config_manager.reload_namespace_config(&namespace_config_path)
            .expect("Failed to reload config");
        assert_eq!(result, Some("test-app".to_string()));
        
        // Verify it's loaded
        assert!(config_manager.get_namespace_config("test-app").is_some());
        
        // Update the configuration file
        let updated_config_content = r#"
[namespace]
name = "app"
description = "Updated test namespace"

[interfaces]
move_interfaces = ["eth1", "wlan0"]

[routing]
default_gateway = "192.168.2.1"

[dns]
servers = ["1.1.1.1"]

[environment]
APP_NAME = "app"
"#;
        
        fs::write(&namespace_config_path, updated_config_content).expect("Failed to write updated config");
        
        // Reload the configuration
        let result = config_manager.reload_namespace_config(&namespace_config_path)
            .expect("Failed to reload updated config");
        assert_eq!(result, Some("test-app".to_string()));
        
        // Verify the configuration was updated
        let config_entry = config_manager.get_namespace_config("test-app").unwrap();
        assert_eq!(config_entry.config.namespace.description, "Updated test namespace");
        assert_eq!(config_entry.config.interfaces.move_interfaces, vec!["eth1", "wlan0"]);
        assert_eq!(config_entry.config.routing.default_gateway, Some("192.168.2.1".to_string()));
        assert_eq!(config_entry.config.dns.servers, vec!["1.1.1.1"]);
    }

    #[test]
    fn test_namespace_config_removal() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        let namespace_config_path = create_test_namespace_config(&temp_dir, "app");
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        // Load the configuration
        config_manager.reload_namespace_config(&namespace_config_path)
            .expect("Failed to load config");
        assert!(config_manager.get_namespace_config("test-app").is_some());
        
        // Remove the configuration
        let removed_name = config_manager.remove_namespace_config(&namespace_config_path);
        assert_eq!(removed_name, Some("test-app".to_string()));
        
        // Verify it's removed
        assert!(config_manager.get_namespace_config("test-app").is_none());
        assert_eq!(config_manager.namespace_configs().len(), 0);
    }

    #[test]
    fn test_namespace_config_validation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        let config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        // Test valid configuration
        let valid_config_path = create_test_namespace_config(&temp_dir, "validapp");
        let errors = config_manager.validate_namespace_config_file(&valid_config_path)
            .expect("Failed to validate config");
        assert!(errors.is_empty());
        
        // Test invalid configuration (invalid TOML)
        let invalid_config_path = temp_dir.path().join("namespaces").join("invalid.toml");
        fs::write(&invalid_config_path, "invalid toml content [[[").expect("Failed to write invalid config");
        
        let errors = config_manager.validate_namespace_config_file(&invalid_config_path)
            .expect("Failed to validate invalid config");
        assert!(!errors.is_empty());
        assert!(errors[0].contains("TOML parsing failed"));
        
        // Test configuration with wrong prefix - use a name that already has a different prefix
        let wrong_prefix_content = r#"
[namespace]
name = "other-wrongapp"
description = "App with wrong prefix"
"#;
        let wrong_prefix_path = temp_dir.path().join("namespaces").join("wrong-prefix.toml");
        fs::write(&wrong_prefix_path, wrong_prefix_content).expect("Failed to write wrong prefix config");
        
        let errors = config_manager.validate_namespace_config_file(&wrong_prefix_path)
            .expect("Failed to validate wrong prefix config");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.contains("doesn't match daemon prefix")));
    }

    #[test]
    fn test_config_stats() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        create_test_namespace_config(&temp_dir, "app1");
        create_test_namespace_config(&temp_dir, "app2");
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        config_manager.scan_namespace_configs().expect("Failed to scan configs");
        
        let stats = config_manager.get_config_stats();
        
        assert_eq!(stats.total_configs, 2);
        assert_eq!(stats.namespace_prefix, "test");
        assert_eq!(stats.config_directory, temp_dir.path().join("namespaces"));
        assert_eq!(stats.loaded_namespaces.len(), 2);
        assert!(stats.loaded_namespaces.contains(&"test-app1".to_string()));
        assert!(stats.loaded_namespaces.contains(&"test-app2".to_string()));
    }

    #[monoio::test]
    async fn test_file_event_handling() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");
        
        let mut config_manager = ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        
        // Test file creation event
        let new_config_path = create_test_namespace_config(&temp_dir, "newapp");
        let event = ConfigFileEvent::Created(new_config_path.clone());
        let result = config_manager.handle_file_event(event).await.expect("Failed to handle create event");
        
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");
        assert!(config_manager.get_namespace_config("test-newapp").is_some());
        
        // Test file modification event
        let updated_config_content = r#"
[namespace]
name = "newapp"
description = "Updated test namespace"

[interfaces]
move_interfaces = ["eth1"]

[routing]
default_gateway = "192.168.2.1"

[dns]
servers = ["1.1.1.1"]

[environment]
APP_NAME = "newapp"
"#;
        
        fs::write(&new_config_path, updated_config_content).expect("Failed to write updated config");
        
        let event = ConfigFileEvent::Modified(new_config_path.clone());
        let result = config_manager.handle_file_event(event).await.expect("Failed to handle modify event");
        
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");
        
        // Verify the configuration was updated
        let config_entry = config_manager.get_namespace_config("test-newapp").unwrap();
        assert_eq!(config_entry.config.namespace.description, "Updated test namespace");
        assert_eq!(config_entry.config.interfaces.move_interfaces, vec!["eth1"]);
        
        // Test file deletion event
        let event = ConfigFileEvent::Deleted(new_config_path);
        let result = config_manager.handle_file_event(event).await.expect("Failed to handle delete event");
        
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");
        assert!(config_manager.get_namespace_config("test-newapp").is_none());
    }

    #[test]
    fn test_config_file_monitor_creation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_dir = temp_dir.path().join("namespaces");
        fs::create_dir_all(&config_dir).expect("Failed to create config dir");
        
        let monitor = ConfigFileMonitor::new(config_dir.clone(), Duration::from_millis(500));
        
        assert_eq!(monitor.config_dir, config_dir);
        assert_eq!(monitor.debounce_duration, Duration::from_millis(500));
    }

    #[test]
    fn test_config_file_event_types() {
        let path1 = PathBuf::from("/test/path1.toml");
        let path2 = PathBuf::from("/test/path2.toml");
        
        let event1 = ConfigFileEvent::Created(path1.clone());
        let event2 = ConfigFileEvent::Modified(path1.clone());
        let event3 = ConfigFileEvent::Deleted(path2.clone());
        
        // Test event equality
        assert_eq!(event1, ConfigFileEvent::Created(path1.clone()));
        assert_ne!(event1, event2);
        assert_ne!(event2, event3);
        
        // Test debug formatting
        let debug_str = format!("{:?}", event1);
        assert!(debug_str.contains("Created"));
        assert!(debug_str.contains("path1.toml"));
    }
}