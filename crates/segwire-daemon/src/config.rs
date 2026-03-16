//! Configuration management for segwire-daemon
//!
//! Handles loading and managing daemon configuration, including master configuration
//! and namespace configuration scanning and monitoring.

use monoio::time::sleep;
use segwire_common::{
    config::{DaemonConfig, NamespaceConfig},
    error::{ConfigError, SegwireResult},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

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
    /// Create a new configuration manager by loading the config from disk.
    pub fn new(config_file_path: PathBuf) -> SegwireResult<Self> {
        info!(
            "Loading daemon configuration from: {}",
            config_file_path.display()
        );

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

    /// Create a new configuration manager from an already-loaded config.
    ///
    /// This avoids a redundant re-parse of the config file when the caller has
    /// already loaded it (e.g. `main()` reads the config for logging setup).
    pub fn from_config(daemon_config: DaemonConfig, config_file_path: PathBuf) -> Self {
        info!(
            "Configuration manager initialized. Namespace prefix: '{}', Config directory: '{}'",
            daemon_config.daemon.namespace_prefix,
            daemon_config.daemon.config_dir.display()
        );

        Self {
            daemon_config,
            config_file_path,
            namespace_configs: HashMap::new(),
        }
    }

    /// Load master daemon configuration from file
    fn load_master_config(config_path: &Path) -> SegwireResult<DaemonConfig> {
        debug!(
            "Reading master configuration file: {}",
            config_path.display()
        );

        // Check if file exists
        if !config_path.exists() {
            error!(
                "Master configuration file not found: {}",
                config_path.display()
            );
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
                error!(
                    "Failed to read file metadata for {}: {}",
                    config_path.display(),
                    e
                );
                return Err(ConfigError::FileNotFound(config_path.display().to_string()).into());
            }
        }

        // Load and parse configuration
        let config = DaemonConfig::from_file(config_path).map_err(|e| {
            error!("Failed to parse master configuration: {}", e);
            e
        })?;

        // Validate configuration directory exists
        if !config.daemon.config_dir.exists() {
            error!(
                "Configuration directory does not exist: {}",
                config.daemon.config_dir.display()
            );
            return Err(ConfigError::InvalidValue {
                field: "daemon.config_dir".to_string(),
                value: config.daemon.config_dir.display().to_string(),
            }
            .into());
        }

        // Validate configuration directory is readable
        match std::fs::read_dir(&config.daemon.config_dir) {
            Ok(_) => {
                debug!(
                    "Configuration directory is accessible: {}",
                    config.daemon.config_dir.display()
                );
            }
            Err(e) => {
                error!(
                    "Configuration directory is not readable: {} - {}",
                    config.daemon.config_dir.display(),
                    e
                );
                return Err(ConfigError::InvalidValue {
                    field: "daemon.config_dir".to_string(),
                    value: format!("Directory not readable: {}", e),
                }
                .into());
            }
        }

        debug!("Master configuration loaded and validated successfully");
        Ok(config)
    }

    /// Get the namespace prefix for this daemon instance
    pub fn namespace_prefix(&self) -> &str {
        &self.daemon_config.daemon.namespace_prefix
    }

    /// Get the configuration directory path
    pub fn config_directory(&self) -> &Path {
        &self.daemon_config.daemon.config_dir
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
        info!(
            "Reloading master configuration from: {}",
            self.config_file_path.display()
        );

        let new_config = Self::load_master_config(&self.config_file_path)?;

        // Check if namespace prefix changed
        if new_config.daemon.namespace_prefix != self.daemon_config.daemon.namespace_prefix {
            warn!(
                "Namespace prefix changed from '{}' to '{}' - this may require daemon restart",
                self.daemon_config.daemon.namespace_prefix, new_config.daemon.namespace_prefix
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

    /// Check if the daemon should cleanup namespaces on shutdown
    pub fn should_cleanup_on_shutdown(&self) -> bool {
        self.daemon_config.daemon.cleanup_on_shutdown
    }

    /// Scan the configuration directory for namespace configuration files
    pub fn scan_namespace_configs(&mut self) -> SegwireResult<Vec<String>> {
        info!(
            "Scanning configuration directory: {}",
            self.config_directory().display()
        );

        let config_dir = self.config_directory();

        // Read directory contents
        let entries = std::fs::read_dir(config_dir).map_err(|e| {
            error!(
                "Failed to read configuration directory {}: {}",
                config_dir.display(),
                e
            );
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

            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");

            debug!("Processing configuration file: {}", path.display());

            match self.load_namespace_config(&path) {
                Ok(Some(entry)) => {
                    info!(
                        "Loaded namespace configuration: {} -> {}",
                        filename, entry.full_name
                    );
                    loaded_configs.push(entry.full_name.clone());
                    new_namespace_configs.insert(entry.full_name.clone(), entry);
                }
                Ok(None) => {
                    debug!(
                        "Skipped configuration file {} (doesn't match prefix)",
                        filename
                    );
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

        info!(
            "Configuration scan complete. Loaded {} namespace configurations",
            loaded_configs.len()
        );
        Ok(loaded_configs)
    }

    /// Load a single namespace configuration file
    fn load_namespace_config(
        &self,
        config_path: &Path,
    ) -> SegwireResult<Option<NamespaceConfigEntry>> {
        debug!("Loading namespace configuration: {}", config_path.display());

        // Get file metadata for modification time
        let metadata = std::fs::metadata(config_path).map_err(|e| {
            error!(
                "Failed to read metadata for {}: {}",
                config_path.display(),
                e
            );
            ConfigError::FileNotFound(config_path.display().to_string())
        })?;

        let last_modified = metadata.modified().map_err(|e| {
            error!(
                "Failed to get modification time for {}: {}",
                config_path.display(),
                e
            );
            ConfigError::InvalidValue {
                field: "file_metadata".to_string(),
                value: format!("Cannot get modification time: {}", e),
            }
        })?;

        // Load and parse the configuration
        let config = NamespaceConfig::from_file(config_path).map_err(|e| {
            error!(
                "Failed to parse namespace configuration {}: {}",
                config_path.display(),
                e
            );
            e
        })?;

        // Check if this namespace should be managed by this daemon
        // If the name already has a prefix, it must match our prefix
        // If the name doesn't have a prefix, we'll add ours
        let should_manage =
            if config.namespace.name.contains('-') || config.namespace.name.contains('_') {
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
        info!(
            "Reloading namespace configuration: {}",
            config_path.display()
        );

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
                error!(
                    "Failed to reload configuration {}: {}",
                    config_path.display(),
                    e
                );
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
    #[cfg(test)]
    pub fn validate_namespace_config_file(&self, config_path: &Path) -> SegwireResult<Vec<String>> {
        let mut errors = Vec::new();

        debug!(
            "Validating namespace configuration file: {}",
            config_path.display()
        );

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
            info!(
                "Configuration file validation successful: {}",
                config_path.display()
            );
        } else {
            warn!(
                "Configuration file validation failed: {} errors",
                errors.len()
            );
        }

        Ok(errors)
    }

    /// Get statistics about loaded configurations
    pub fn get_config_stats(&self) -> ConfigStats {
        ConfigStats {
            total_configs: self.namespace_configs.len(),
        }
    }
}

/// Statistics about loaded configurations
#[derive(Debug, Clone)]
pub struct ConfigStats {
    pub total_configs: usize,
}

/// File system event types for configuration monitoring
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

/// Configuration file monitor using inotify for kernel-based file watching
pub struct ConfigFileMonitor {
    config_dir: PathBuf,
    debounce_duration: Duration,
}

impl ConfigFileMonitor {
    /// Create a new configuration file monitor
    pub fn new(config_dir: PathBuf, debounce_duration: Duration) -> Self {
        Self {
            config_dir,
            debounce_duration,
        }
    }

    /// Start monitoring configuration files for changes.
    ///
    /// Uses Linux inotify for event-driven file watching instead of polling.
    /// Returns a `local-sync` receiver for debounced file system events.
    pub async fn start_monitoring(
        &mut self,
    ) -> SegwireResult<local_sync::mpsc::unbounded::Rx<ConfigFileEvent>> {
        info!(
            "Starting inotify-based configuration file monitoring for directory: {}",
            self.config_dir.display()
        );

        let (tx, rx) = local_sync::mpsc::unbounded::channel();

        let config_dir = self.config_dir.clone();
        let debounce_duration = self.debounce_duration;

        monoio::spawn(async move {
            if let Err(e) = Self::watch_directory_inotify(config_dir, debounce_duration, tx).await {
                error!("inotify-based file monitoring failed: {}", e);
            }
        });

        Ok(rx)
    }

    /// Watch a directory for file system changes using inotify.
    ///
    /// Uses the Linux inotify API to receive kernel notifications for file
    /// create/modify/delete events, replacing the old polling loop.
    /// Debouncing is per-file-path to correctly handle rapid edits to
    /// individual files.
    async fn watch_directory_inotify(
        config_dir: PathBuf,
        debounce_duration: Duration,
        tx: local_sync::mpsc::unbounded::Tx<ConfigFileEvent>,
    ) -> SegwireResult<()> {
        use inotify::{Inotify, WatchMask};

        let mut inotify = Inotify::init().map_err(|e| {
            error!("Failed to initialize inotify: {}", e);
            segwire_common::error::ConfigError::InvalidValue {
                field: "inotify".to_string(),
                value: format!("Failed to initialize inotify: {}", e),
            }
        })?;

        inotify
            .watches()
            .add(
                &config_dir,
                WatchMask::CREATE
                    | WatchMask::MODIFY
                    | WatchMask::DELETE
                    | WatchMask::MOVED_FROM
                    | WatchMask::MOVED_TO
                    | WatchMask::CLOSE_WRITE,
            )
            .map_err(|e| {
                error!(
                    "Failed to add inotify watch for {}: {}",
                    config_dir.display(),
                    e
                );
                segwire_common::error::ConfigError::InvalidValue {
                    field: "inotify_watch".to_string(),
                    value: format!("Failed to watch directory: {}", e),
                }
            })?;

        info!("inotify watch established for: {}", config_dir.display());

        // Per-file debounce tracking
        let mut last_events: HashMap<PathBuf, Instant> = HashMap::new();
        let mut buffer = [0u8; 4096];

        loop {
            // Read inotify events (non-blocking; Inotify::init uses IN_NONBLOCK
            // is NOT the default, so we use a short async sleep to yield control
            // instead of blocking the monoio runtime).
            match inotify.read_events(&mut buffer) {
                Ok(events) => {
                    for event in events {
                        // Only process events for .toml files
                        let name = match event.name {
                            Some(name) => name,
                            None => continue,
                        };
                        let name_str = match name.to_str() {
                            Some(s) => s,
                            None => continue,
                        };
                        if !name_str.ends_with(".toml") {
                            continue;
                        }

                        let path = config_dir.join(name);

                        // Per-file-path debounce: skip if we emitted an event
                        // for this path within the debounce window.
                        let now = Instant::now();
                        if let Some(last) = last_events.get(&path) {
                            if now.duration_since(*last) < debounce_duration {
                                debug!("Debouncing event for: {}", path.display());
                                continue;
                            }
                        }
                        last_events.insert(path.clone(), now);

                        let mask = event.mask;
                        let file_event = if mask.contains(inotify::EventMask::CREATE)
                            || mask.contains(inotify::EventMask::MOVED_TO)
                        {
                            Some(ConfigFileEvent::Created(path))
                        } else if mask.contains(inotify::EventMask::MODIFY)
                            || mask.contains(inotify::EventMask::CLOSE_WRITE)
                        {
                            Some(ConfigFileEvent::Modified(path))
                        } else if mask.contains(inotify::EventMask::DELETE)
                            || mask.contains(inotify::EventMask::MOVED_FROM)
                        {
                            Some(ConfigFileEvent::Deleted(path))
                        } else {
                            None
                        };

                        if let Some(evt) = file_event {
                            debug!("Sending inotify file event: {:?}", evt);
                            if tx.send(evt).is_err() {
                                warn!("Failed to send file event — receiver dropped");
                                return Ok(());
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No events available yet — this is normal for non-blocking reads
                }
                Err(e) => {
                    error!("inotify read error: {}", e);
                    // Brief backoff before retrying
                    sleep(Duration::from_secs(1)).await;
                }
            }

            // Yield to the monoio runtime before polling again.
            // This is much lighter than the old 500ms full-directory scan.
            sleep(Duration::from_millis(100)).await;

            // Periodically prune stale debounce entries (older than 10× debounce window)
            let prune_cutoff = debounce_duration * 10;
            let now = Instant::now();
            last_events.retain(|_, last| now.duration_since(*last) < prune_cutoff);
        }
    }
}

impl ConfigManager {
    /// Start monitoring configuration files for changes
    pub async fn start_file_monitoring(
        &mut self,
    ) -> SegwireResult<local_sync::mpsc::unbounded::Rx<ConfigFileEvent>> {
        let mut monitor = ConfigFileMonitor::new(
            self.config_directory().to_path_buf(),
            Duration::from_millis(1000), // 1 second debounce
        );

        monitor.start_monitoring().await
    }

    /// Handle a file system event by updating configurations
    pub async fn handle_file_event(
        &mut self,
        event: ConfigFileEvent,
    ) -> SegwireResult<Vec<String>> {
        match event {
            ConfigFileEvent::Created(path) => {
                info!("Configuration file created: {}", path.display());
                match self.reload_namespace_config(&path) {
                    Ok(Some(name)) => Ok(vec![name]),
                    Ok(None) => Ok(vec![]),
                    Err(e) => {
                        error!(
                            "Failed to load new configuration file {}: {}",
                            path.display(),
                            e
                        );
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
                    self.scan_namespace_configs()
                } else {
                    // Handle namespace configuration file modification
                    match self.reload_namespace_config(&path) {
                        Ok(Some(name)) => Ok(vec![name]),
                        Ok(None) => Ok(vec![]),
                        Err(e) => {
                            error!(
                                "Failed to reload configuration file {}: {}",
                                path.display(),
                                e
                            );
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
    use std::path::PathBuf;
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
        fs::create_dir_all(temp_dir.path().join("namespaces"))
            .expect("Failed to create namespaces dir");

        config_path
    }

    #[test]
    fn test_config_manager_creation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");

        let config_manager =
            ConfigManager::new(config_path).expect("Failed to create config manager");

        assert_eq!(config_manager.namespace_prefix(), "test");
        assert_eq!(
            config_manager.config_directory(),
            temp_dir.path().join("namespaces")
        );
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

        let config_manager =
            ConfigManager::new(config_path).expect("Failed to create config manager");

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

        let config_manager =
            ConfigManager::new(config_path).expect("Failed to create config manager");

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

        let _config_manager =
            ConfigManager::new(config_path).expect("Failed to create config manager");
    }

    #[test]
    fn test_config_reload() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = create_test_daemon_config(&temp_dir, "test");

        let mut config_manager =
            ConfigManager::new(config_path.clone()).expect("Failed to create config manager");

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
        config_manager
            .reload_master_config()
            .expect("Failed to reload config");

        assert_eq!(config_manager.namespace_prefix(), "updated");

        // Note: log_target is a config field but no accessor method exists yet
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

        let config_path = temp_dir
            .path()
            .join("namespaces")
            .join(format!("{}.toml", name));
        fs::write(&config_path, config_content).expect("Failed to write test namespace config");

        config_path
    }

    #[test]
    fn test_namespace_config_scanning() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");

        create_test_namespace_config(&temp_dir, "app1");
        create_test_namespace_config(&temp_dir, "app2");
        create_test_namespace_config(&temp_dir, "other-app");

        let non_toml_path = temp_dir.path().join("namespaces").join("readme.txt");
        fs::write(&non_toml_path, "This is not a TOML file")
            .expect("Failed to write non-TOML file");

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");

        let loaded_configs = config_manager
            .scan_namespace_configs()
            .expect("Failed to scan configs");

        assert_eq!(loaded_configs.len(), 2);
        assert!(loaded_configs.contains(&"test-app1".to_string()));
        assert!(loaded_configs.contains(&"test-app2".to_string()));
        assert!(!loaded_configs.contains(&"test-other-app".to_string()));

        assert_eq!(config_manager.namespace_configs().len(), 2);
        assert!(config_manager.get_namespace_config("test-app1").is_some());
        assert!(config_manager.get_namespace_config("test-app2").is_some());
        assert!(config_manager
            .get_namespace_config("test-other-app")
            .is_none());
    }

    #[test]
    fn test_namespace_config_prefix_filtering() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "segwire");

        create_test_namespace_config(&temp_dir, "app");
        create_test_namespace_config(&temp_dir, "segwire-service");
        create_test_namespace_config(&temp_dir, "other-service");

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");

        let loaded_configs = config_manager
            .scan_namespace_configs()
            .expect("Failed to scan configs");

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

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");

        let result = config_manager
            .reload_namespace_config(&namespace_config_path)
            .expect("Failed to reload config");
        assert_eq!(result, Some("test-app".to_string()));

        assert!(config_manager.get_namespace_config("test-app").is_some());

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

        fs::write(&namespace_config_path, updated_config_content)
            .expect("Failed to write updated config");

        let result = config_manager
            .reload_namespace_config(&namespace_config_path)
            .expect("Failed to reload updated config");
        assert_eq!(result, Some("test-app".to_string()));

        let config_entry = config_manager.get_namespace_config("test-app").unwrap();
        assert_eq!(
            config_entry.config.namespace.description,
            "Updated test namespace"
        );
        assert_eq!(
            config_entry.config.interfaces.move_interfaces,
            vec!["eth1", "wlan0"]
        );
        assert_eq!(
            config_entry.config.routing.default_gateway,
            Some("192.168.2.1".to_string())
        );
        assert_eq!(config_entry.config.dns.servers, vec!["1.1.1.1"]);
    }

    #[test]
    fn test_namespace_config_removal() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");

        let namespace_config_path = create_test_namespace_config(&temp_dir, "app");

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");

        config_manager
            .reload_namespace_config(&namespace_config_path)
            .expect("Failed to load config");
        assert!(config_manager.get_namespace_config("test-app").is_some());

        let removed_name = config_manager.remove_namespace_config(&namespace_config_path);
        assert_eq!(removed_name, Some("test-app".to_string()));

        assert!(config_manager.get_namespace_config("test-app").is_none());
        assert_eq!(config_manager.namespace_configs().len(), 0);
    }

    #[test]
    fn test_config_stats() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");

        create_test_namespace_config(&temp_dir, "app1");
        create_test_namespace_config(&temp_dir, "app2");

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");
        config_manager
            .scan_namespace_configs()
            .expect("Failed to scan configs");

        let stats = config_manager.get_config_stats();

        assert_eq!(stats.total_configs, 2);
    }

    #[monoio::test]
    async fn test_file_event_handling() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let daemon_config_path = create_test_daemon_config(&temp_dir, "test");

        let mut config_manager =
            ConfigManager::new(daemon_config_path).expect("Failed to create config manager");

        let new_config_path = create_test_namespace_config(&temp_dir, "newapp");
        let event = ConfigFileEvent::Created(new_config_path.clone());
        let result = config_manager
            .handle_file_event(event)
            .await
            .expect("Failed to handle create event");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");
        assert!(config_manager.get_namespace_config("test-newapp").is_some());

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

        fs::write(&new_config_path, updated_config_content)
            .expect("Failed to write updated config");

        let event = ConfigFileEvent::Modified(new_config_path.clone());
        let result = config_manager
            .handle_file_event(event)
            .await
            .expect("Failed to handle modify event");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");

        let config_entry = config_manager.get_namespace_config("test-newapp").unwrap();
        assert_eq!(
            config_entry.config.namespace.description,
            "Updated test namespace"
        );
        assert_eq!(config_entry.config.interfaces.move_interfaces, vec!["eth1"]);

        let event = ConfigFileEvent::Deleted(new_config_path);
        let result = config_manager
            .handle_file_event(event)
            .await
            .expect("Failed to handle delete event");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "test-newapp");
        assert!(config_manager.get_namespace_config("test-newapp").is_none());
    }
}
