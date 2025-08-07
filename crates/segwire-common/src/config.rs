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
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_daemon_config_serialization() {
        let config = DaemonConfig {
            daemon: DaemonSettings {
                namespace_prefix: "test".to_string(),
                config_dir: "/tmp".into(),
                cleanup_on_shutdown: true,
                log_level: "debug".to_string(),
                log_target: "stdout".to_string(),
            },
            dbus: DBusSettings {
                service_name: "org.test.Service".to_string(),
                object_path: "/org/test/Service".to_string(),
            },
        };

        // Test serialization
        let toml_str = toml::to_string(&config).expect("Failed to serialize config");
        assert!(toml_str.contains("namespace_prefix = \"test\""));
        assert!(toml_str.contains("service_name = \"org.test.Service\""));

        // Test deserialization
        let deserialized: DaemonConfig = toml::from_str(&toml_str)
            .expect("Failed to deserialize config");
        assert_eq!(deserialized.daemon.namespace_prefix, "test");
        assert_eq!(deserialized.dbus.service_name, "org.test.Service");
    }

    #[test]
    fn test_namespace_config_serialization() {
        let mut env_vars = HashMap::new();
        env_vars.insert("APP_NETWORK".to_string(), "192.168.100.0/24".to_string());

        let config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "test-app".to_string(),
                description: "Test application namespace".to_string(),
            },
            interfaces: InterfaceConfig {
                move_interfaces: vec!["eth1".to_string()],
                virtual_interfaces: vec![VirtualInterface {
                    name: "veth-app".to_string(),
                    interface_type: "veth".to_string(),
                    peer: Some("veth-host".to_string()),
                }],
            },
            routing: RoutingConfig {
                default_gateway: Some("192.168.100.1".to_string()),
                routes: vec![Route {
                    destination: "10.0.0.0/8".to_string(),
                    gateway: "192.168.100.1".to_string(),
                    metric: Some(100),
                }],
            },
            dns: DnsConfig {
                servers: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
                search: vec!["example.com".to_string()],
            },
            environment: env_vars,
        };

        // Test serialization
        let toml_str = toml::to_string(&config).expect("Failed to serialize config");
        assert!(toml_str.contains("name = \"test-app\""));
        assert!(toml_str.contains("move_interfaces = [\"eth1\"]"));
        assert!(toml_str.contains("default_gateway = \"192.168.100.1\""));

        // Test deserialization
        let deserialized: NamespaceConfig = toml::from_str(&toml_str)
            .expect("Failed to deserialize config");
        assert_eq!(deserialized.namespace.name, "test-app");
        assert_eq!(deserialized.interfaces.move_interfaces, vec!["eth1"]);
        assert_eq!(deserialized.routing.default_gateway, Some("192.168.100.1".to_string()));
    }

    #[test]
    fn test_daemon_config_defaults() {
        let minimal_toml = r#"
[daemon]
namespace_prefix = "test"
config_dir = "/tmp"

[dbus]
"#;

        let config: DaemonConfig = toml::from_str(minimal_toml)
            .expect("Failed to parse minimal config");
        
        // Check defaults are applied
        assert_eq!(config.daemon.cleanup_on_shutdown, true);
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.daemon.log_target, "syslog");
        assert_eq!(config.dbus.service_name, "org.segwire.NamespaceManager");
        assert_eq!(config.dbus.object_path, "/org/segwire/NamespaceManager");
    }

    #[test]
    fn test_namespace_config_validation() {
        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "".to_string(), // Invalid empty name
                description: "Test".to_string(),
            },
            interfaces: InterfaceConfig::default(),
            routing: RoutingConfig::default(),
            dns: DnsConfig::default(),
            environment: HashMap::new(),
        };

        // Should fail validation with empty name
        assert!(config.validate().is_err());

        // Fix the name
        config.namespace.name = "valid-name".to_string();
        assert!(config.validate().is_ok());

        // Test invalid characters in name
        config.namespace.name = "invalid name!".to_string();
        assert!(config.validate().is_err());
    }
}