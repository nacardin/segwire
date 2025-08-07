//! Configuration structures and parsing for segwire
//! 
//! Defines TOML configuration structures for both daemon and namespace
//! configurations, with validation and environment variable substitution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use crate::error::{ConfigError, SegwireResult};

/// Master daemon configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub daemon: DaemonSettings,
    pub dbus: DBusSettings,
}

/// Daemon-specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// Namespace prefix for this daemon instance
    pub namespace_prefix: String,
    
    /// Configuration directory to monitor
    pub config_dir: PathBuf,
    
    /// Cleanup policy on shutdown
    #[serde(default = "default_cleanup_on_shutdown")]
    pub cleanup_on_shutdown: bool,
    
    /// Logging configuration
    #[serde(default = "default_log_level")]
    pub log_level: String,
    
    /// Log target (syslog, stdout, file)
    #[serde(default = "default_log_target")]
    pub log_target: String,
}

/// D-Bus service settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DBusSettings {
    /// D-Bus service name
    #[serde(default = "default_service_name")]
    pub service_name: String,
    
    /// D-Bus object path
    #[serde(default = "default_object_path")]
    pub object_path: String,
}

/// Individual namespace configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceConfig {
    pub namespace: NamespaceSettings,
    #[serde(default)]
    pub interfaces: InterfaceConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub dns: DnsConfig,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

/// Namespace-specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceSettings {
    /// Namespace name (will be prefixed with daemon prefix)
    pub name: String,
    
    /// Description for documentation
    #[serde(default)]
    pub description: String,
}

/// Network interface configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InterfaceConfig {
    /// Network interfaces to move into namespace
    #[serde(default)]
    pub move_interfaces: Vec<String>,
    
    /// Virtual interfaces to create
    #[serde(default)]
    pub virtual_interfaces: Vec<VirtualInterface>,
}

/// Virtual interface definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualInterface {
    pub name: String,
    pub interface_type: String,
    pub peer: Option<String>,
}

/// Routing configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    /// Default gateway within namespace
    pub default_gateway: Option<String>,
    
    /// Static routes
    #[serde(default)]
    pub routes: Vec<Route>,
}

/// Static route definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub destination: String,
    pub gateway: String,
    pub metric: Option<u32>,
}

/// DNS configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DnsConfig {
    /// DNS servers for namespace
    #[serde(default)]
    pub servers: Vec<String>,
    
    /// Search domains
    #[serde(default)]
    pub search: Vec<String>,
}

// Default value functions
fn default_cleanup_on_shutdown() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_target() -> String {
    "syslog".to_string()
}

fn default_service_name() -> String {
    "org.segwire.NamespaceManager".to_string()
}

fn default_object_path() -> String {
    "/org/segwire/NamespaceManager".to_string()
}

impl DaemonConfig {
    /// Load daemon configuration from TOML file
    pub fn from_file(path: &std::path::Path) -> SegwireResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::FileNotFound(path.display().to_string()))?;
        
        let config: DaemonConfig = toml::from_str(&content)
            .map_err(ConfigError::InvalidToml)?;
        config.validate()?;
        Ok(config)
    }
    
    /// Validate daemon configuration
    pub fn validate(&self) -> SegwireResult<()> {
        if self.daemon.namespace_prefix.is_empty() {
            return Err(ConfigError::MissingField("namespace_prefix".to_string()).into());
        }
        
        if !self.daemon.config_dir.exists() {
            return Err(ConfigError::InvalidValue {
                field: "config_dir".to_string(),
                value: self.daemon.config_dir.display().to_string(),
            }.into());
        }
        
        Ok(())
    }
}

impl NamespaceConfig {
    /// Load namespace configuration from TOML file
    pub fn from_file(path: &std::path::Path) -> SegwireResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::FileNotFound(path.display().to_string()))?;
        
        let mut config: NamespaceConfig = toml::from_str(&content)
            .map_err(ConfigError::InvalidToml)?;
        config.substitute_environment_variables()?;
        config.validate()?;
        Ok(config)
    }
    
    /// Substitute environment variables in configuration values
    pub fn substitute_environment_variables(&mut self) -> SegwireResult<()> {
        // This is a placeholder for environment variable substitution
        // Will be implemented in task 2.3
        Ok(())
    }
    
    /// Validate namespace configuration
    pub fn validate(&self) -> SegwireResult<()> {
        if self.namespace.name.is_empty() {
            return Err(ConfigError::MissingField("namespace.name".to_string()).into());
        }
        
        // Validate namespace name format
        if !self.namespace.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            return Err(ConfigError::InvalidValue {
                field: "namespace.name".to_string(),
                value: self.namespace.name.clone(),
            }.into());
        }
        
        Ok(())
    }
}