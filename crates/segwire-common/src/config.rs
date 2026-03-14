//! Configuration structures and parsing for segwire
//!
//! Defines TOML configuration structures for both daemon and namespace
//! configurations, with validation and environment variable substitution.

use crate::error::{ConfigError, SegwireResult};
use crate::utils::{
    validate_cidr, validate_domain_name, validate_interface_name, validate_ip_address,
    validate_namespace_name, validate_namespace_prefix,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Logging configuration settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Include timestamps in log output
    #[serde(default = "default_with_timestamps")]
    pub with_timestamps: bool,

    /// Include thread names in log output
    #[serde(default = "default_with_thread_names")]
    pub with_thread_names: bool,

    /// Include file and line information
    #[serde(default = "default_with_file_line")]
    pub with_file_line: bool,

    /// Include span information for tracing
    #[serde(default = "default_with_spans")]
    pub with_spans: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            with_timestamps: default_with_timestamps(),
            with_thread_names: default_with_thread_names(),
            with_file_line: default_with_file_line(),
            with_spans: default_with_spans(),
        }
    }
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
    /// IP addresses to assign in CIDR notation (e.g. "10.0.0.2/24")
    #[serde(default)]
    pub addresses: Vec<String>,
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

fn default_with_timestamps() -> bool {
    true
}

fn default_with_thread_names() -> bool {
    true
}

fn default_with_file_line() -> bool {
    false
}

fn default_with_spans() -> bool {
    false
}

fn default_service_name() -> String {
    "org.segwire.NamespaceManager".to_string()
}

fn default_object_path() -> String {
    "/org/segwire/NamespaceManager".to_string()
}

// Validation implementations for configuration components
impl InterfaceConfig {
    /// Validate interface configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate interface names to move
        for interface in &self.move_interfaces {
            validate_interface_name(interface).map_err(|_| ConfigError::InvalidValue {
                field: "interfaces.move_interfaces".to_string(),
                value: interface.clone(),
            })?;
        }

        // Validate virtual interfaces
        for vif in &self.virtual_interfaces {
            vif.validate()?;
        }

        // Check for duplicate interface names
        let mut all_interfaces = self.move_interfaces.clone();
        for vif in &self.virtual_interfaces {
            all_interfaces.push(vif.name.clone());
            if let Some(ref peer) = vif.peer {
                all_interfaces.push(peer.clone());
            }
        }

        let mut sorted_interfaces = all_interfaces.clone();
        sorted_interfaces.sort();
        sorted_interfaces.dedup();

        if sorted_interfaces.len() != all_interfaces.len() {
            return Err(ConfigError::InvalidValue {
                field: "interfaces".to_string(),
                value: "Duplicate interface names found".to_string(),
            }
            .into());
        }

        Ok(())
    }
}

impl VirtualInterface {
    /// Validate virtual interface configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate interface name
        validate_interface_name(&self.name).map_err(|_| ConfigError::InvalidValue {
            field: "interfaces.virtual.name".to_string(),
            value: self.name.clone(),
        })?;

        // Validate interface type
        let valid_types = ["veth", "bridge", "dummy", "macvlan", "ipvlan"];
        if !valid_types.contains(&self.interface_type.as_str()) {
            return Err(ConfigError::InvalidValue {
                field: "interfaces.virtual.type".to_string(),
                value: self.interface_type.clone(),
            }
            .into());
        }

        // Validate peer name if present
        if let Some(ref peer) = self.peer {
            validate_interface_name(peer).map_err(|_| ConfigError::InvalidValue {
                field: "interfaces.virtual.peer".to_string(),
                value: peer.clone(),
            })?;

            // Peer name should be different from interface name
            if peer == &self.name {
                return Err(ConfigError::InvalidValue {
                    field: "interfaces.virtual.peer".to_string(),
                    value: "Peer name cannot be the same as interface name".to_string(),
                }
                .into());
            }
        }

        // veth interfaces require a peer
        if self.interface_type == "veth" && self.peer.is_none() {
            return Err(ConfigError::MissingField("interfaces.virtual.peer".to_string()).into());
        }

        // Validate addresses (must be valid CIDR)
        for (i, addr) in self.addresses.iter().enumerate() {
            validate_cidr(addr).map_err(|_| ConfigError::InvalidValue {
                field: format!("interfaces.virtual.addresses[{}]", i),
                value: addr.clone(),
            })?;
        }

        Ok(())
    }
}

impl RoutingConfig {
    /// Validate routing configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate default gateway if present
        if let Some(ref gateway) = self.default_gateway {
            validate_ip_address(gateway).map_err(|_| ConfigError::InvalidValue {
                field: "routing.default_gateway".to_string(),
                value: gateway.clone(),
            })?;
        }

        // Validate all routes
        for (index, route) in self.routes.iter().enumerate() {
            route.validate().map_err(|e| match e {
                crate::error::SegwireError::Config(config_err) => {
                    // Enhance error message with route index
                    match config_err {
                        ConfigError::InvalidValue { field, value } => {
                            crate::error::SegwireError::Config(ConfigError::InvalidValue {
                                field: format!("routing.routes[{}].{}", index, field),
                                value,
                            })
                        }
                        other => crate::error::SegwireError::Config(other),
                    }
                }
                other => other,
            })?;
        }

        // Check for duplicate routes (same destination)
        let mut destinations: Vec<&String> = self.routes.iter().map(|r| &r.destination).collect();
        destinations.sort();
        for window in destinations.windows(2) {
            if window[0] == window[1] {
                return Err(ConfigError::InvalidValue {
                    field: "routing.routes".to_string(),
                    value: format!("Duplicate route destination: {}", window[0]),
                }
                .into());
            }
        }

        Ok(())
    }
}

impl Route {
    /// Validate individual route configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate destination CIDR
        validate_cidr(&self.destination).map_err(|_| ConfigError::InvalidValue {
            field: "destination".to_string(),
            value: self.destination.clone(),
        })?;

        // Validate gateway IP
        validate_ip_address(&self.gateway).map_err(|_| ConfigError::InvalidValue {
            field: "gateway".to_string(),
            value: self.gateway.clone(),
        })?;

        // Validate metric if present
        if let Some(metric) = self.metric {
            if metric == 0 {
                return Err(ConfigError::InvalidValue {
                    field: "metric".to_string(),
                    value: "Route metric cannot be zero".to_string(),
                }
                .into());
            }
        }

        Ok(())
    }
}

impl DnsConfig {
    /// Validate DNS configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate DNS servers
        for (index, server) in self.servers.iter().enumerate() {
            validate_ip_address(server).map_err(|_| ConfigError::InvalidValue {
                field: format!("dns.servers[{}]", index),
                value: server.clone(),
            })?;
        }

        // Validate search domains
        for (index, domain) in self.search.iter().enumerate() {
            validate_domain_name(domain).map_err(|_| ConfigError::InvalidValue {
                field: format!("dns.search[{}]", index),
                value: domain.clone(),
            })?;
        }

        // Check for duplicate DNS servers
        let mut sorted_servers = self.servers.clone();
        sorted_servers.sort();
        sorted_servers.dedup();
        if sorted_servers.len() != self.servers.len() {
            return Err(ConfigError::InvalidValue {
                field: "dns.servers".to_string(),
                value: "Duplicate DNS servers found".to_string(),
            }
            .into());
        }

        // Check for duplicate search domains
        let mut sorted_domains = self.search.clone();
        sorted_domains.sort();
        sorted_domains.dedup();
        if sorted_domains.len() != self.search.len() {
            return Err(ConfigError::InvalidValue {
                field: "dns.search".to_string(),
                value: "Duplicate search domains found".to_string(),
            }
            .into());
        }

        Ok(())
    }
}

impl LoggingConfig {
    /// Validate logging configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate log level
        let valid_log_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_log_levels.contains(&self.level.as_str()) {
            return Err(ConfigError::InvalidValue {
                field: "logging.level".to_string(),
                value: self.level.clone(),
            }
            .into());
        }

        Ok(())
    }
}

impl DaemonConfig {
    /// Load daemon configuration from TOML file
    pub fn from_file(path: &std::path::Path) -> SegwireResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::FileNotFound(path.display().to_string()))?;

        let config: DaemonConfig = toml::from_str(&content).map_err(ConfigError::InvalidToml)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate daemon configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate namespace prefix
        validate_namespace_prefix(&self.daemon.namespace_prefix)?;

        // Validate config directory exists
        if !self.daemon.config_dir.exists() {
            return Err(ConfigError::InvalidValue {
                field: "config_dir".to_string(),
                value: self.daemon.config_dir.display().to_string(),
            }
            .into());
        }

        // Validate logging configuration
        self.daemon.logging.validate()?;

        // Validate D-Bus service name format
        if !self.dbus.service_name.contains('.') || self.dbus.service_name.starts_with('.') {
            return Err(ConfigError::InvalidValue {
                field: "service_name".to_string(),
                value: self.dbus.service_name.clone(),
            }
            .into());
        }

        // Validate D-Bus object path format
        if !self.dbus.object_path.starts_with('/') {
            return Err(ConfigError::InvalidValue {
                field: "object_path".to_string(),
                value: self.dbus.object_path.clone(),
            }
            .into());
        }

        Ok(())
    }
}

impl NamespaceConfig {
    /// Load namespace configuration from TOML file
    pub fn from_file(path: &std::path::Path) -> SegwireResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::FileNotFound(path.display().to_string()))?;

        let mut config: NamespaceConfig =
            toml::from_str(&content).map_err(ConfigError::InvalidToml)?;
        config.substitute_environment_variables()?;
        config.validate()?;
        Ok(config)
    }

    /// Substitute environment variables in configuration values
    pub fn substitute_environment_variables(&mut self) -> SegwireResult<()> {
        use crate::utils::substitute_env_vars;

        // Substitute in namespace settings
        self.namespace.name = substitute_env_vars(&self.namespace.name, &self.environment)?;
        self.namespace.description =
            substitute_env_vars(&self.namespace.description, &self.environment)?;

        // Substitute in interface configuration
        for interface in &mut self.interfaces.move_interfaces {
            *interface = substitute_env_vars(interface, &self.environment)?;
        }

        for vif in &mut self.interfaces.virtual_interfaces {
            vif.name = substitute_env_vars(&vif.name, &self.environment)?;
            vif.interface_type = substitute_env_vars(&vif.interface_type, &self.environment)?;
            if let Some(ref mut peer) = vif.peer {
                *peer = substitute_env_vars(peer, &self.environment)?;
            }
            for addr in &mut vif.addresses {
                *addr = substitute_env_vars(addr, &self.environment)?;
            }
        }

        // Substitute in routing configuration
        if let Some(ref mut gateway) = self.routing.default_gateway {
            *gateway = substitute_env_vars(gateway, &self.environment)?;
        }

        for route in &mut self.routing.routes {
            route.destination = substitute_env_vars(&route.destination, &self.environment)?;
            route.gateway = substitute_env_vars(&route.gateway, &self.environment)?;
        }

        // Substitute in DNS configuration
        for server in &mut self.dns.servers {
            *server = substitute_env_vars(server, &self.environment)?;
        }

        for domain in &mut self.dns.search {
            *domain = substitute_env_vars(domain, &self.environment)?;
        }

        Ok(())
    }

    /// Validate namespace configuration
    pub fn validate(&self) -> SegwireResult<()> {
        // Validate namespace settings
        self.validate_namespace_settings()?;

        // Validate interface configuration
        self.interfaces.validate()?;

        // Validate routing configuration
        self.routing.validate()?;

        // Validate DNS configuration
        self.dns.validate()?;

        Ok(())
    }

    /// Validate namespace-specific settings
    fn validate_namespace_settings(&self) -> SegwireResult<()> {
        // Validate namespace name
        validate_namespace_name(&self.namespace.name)?;

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
                logging: LoggingConfig {
                    level: "debug".to_string(),
                    with_timestamps: true,
                    with_thread_names: true,
                    with_file_line: false,
                    with_spans: false,
                },
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
        let deserialized: DaemonConfig =
            toml::from_str(&toml_str).expect("Failed to deserialize config");
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
                    addresses: vec![],
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
        let deserialized: NamespaceConfig =
            toml::from_str(&toml_str).expect("Failed to deserialize config");
        assert_eq!(deserialized.namespace.name, "test-app");
        assert_eq!(deserialized.interfaces.move_interfaces, vec!["eth1"]);
        assert_eq!(
            deserialized.routing.default_gateway,
            Some("192.168.100.1".to_string())
        );
    }

    #[test]
    fn test_daemon_config_defaults() {
        let minimal_toml = r#"
[daemon]
namespace_prefix = "test"
config_dir = "/tmp"

[dbus]
"#;

        let config: DaemonConfig =
            toml::from_str(minimal_toml).expect("Failed to parse minimal config");

        // Check defaults are applied
        assert!(config.daemon.cleanup_on_shutdown);
        assert_eq!(config.daemon.logging.level, "info");
        assert_eq!(config.dbus.service_name, "org.segwire.NamespaceManager");
        assert_eq!(config.dbus.object_path, "/org/segwire/NamespaceManager");
    }

    #[test]
    fn test_daemon_config_validation() {
        let mut config = DaemonConfig {
            daemon: DaemonSettings {
                namespace_prefix: "".to_string(), // Invalid empty prefix
                config_dir: "/tmp".into(),
                cleanup_on_shutdown: true,
                logging: LoggingConfig::default(),
            },
            dbus: DBusSettings {
                service_name: "org.test.Service".to_string(),
                object_path: "/org/test/Service".to_string(),
            },
        };

        // Should fail validation with empty prefix
        assert!(config.validate().is_err());

        // Fix the prefix
        config.daemon.namespace_prefix = "test".to_string();
        assert!(config.validate().is_ok());

        // Test invalid log level
        config.daemon.logging.level = "invalid".to_string();
        assert!(config.validate().is_err());

        // Fix log level
        config.daemon.logging.level = "debug".to_string();
        assert!(config.validate().is_ok());

        // Test invalid service name
        config.dbus.service_name = "invalid-service".to_string();
        assert!(config.validate().is_err());
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
        config.namespace.name = "1invalid".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_interface_config_validation() {
        let mut config = InterfaceConfig {
            move_interfaces: vec!["eth0".to_string(), "invalid@interface".to_string()],
            virtual_interfaces: vec![],
        };

        // Should fail with invalid interface name
        assert!(config.validate().is_err());

        // Fix interface names
        config.move_interfaces = vec!["eth0".to_string(), "wlan0".to_string()];
        assert!(config.validate().is_ok());

        // Test duplicate interface names
        config.move_interfaces = vec!["eth0".to_string(), "eth0".to_string()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_virtual_interface_validation() {
        let mut vif = VirtualInterface {
            name: "veth0".to_string(),
            interface_type: "veth".to_string(),
            peer: None, // veth requires peer
            addresses: vec![],
        };

        // Should fail without peer for veth
        assert!(vif.validate().is_err());

        // Add peer
        vif.peer = Some("veth1".to_string());
        assert!(vif.validate().is_ok());

        // Test invalid interface type
        vif.interface_type = "invalid".to_string();
        assert!(vif.validate().is_err());

        // Test peer same as name
        vif.interface_type = "veth".to_string();
        vif.peer = Some("veth0".to_string());
        assert!(vif.validate().is_err());
    }

    #[test]
    fn test_routing_config_validation() {
        let mut config = RoutingConfig {
            default_gateway: Some("invalid-ip".to_string()),
            routes: vec![],
        };

        // Should fail with invalid gateway IP
        assert!(config.validate().is_err());

        // Fix gateway IP
        config.default_gateway = Some("192.168.1.1".to_string());
        assert!(config.validate().is_ok());

        // Test invalid route
        config.routes = vec![Route {
            destination: "invalid-cidr".to_string(),
            gateway: "192.168.1.1".to_string(),
            metric: Some(100),
        }];
        assert!(config.validate().is_err());

        // Fix route
        config.routes = vec![Route {
            destination: "10.0.0.0/8".to_string(),
            gateway: "192.168.1.1".to_string(),
            metric: Some(100),
        }];
        assert!(config.validate().is_ok());

        // Test duplicate routes
        config.routes = vec![
            Route {
                destination: "10.0.0.0/8".to_string(),
                gateway: "192.168.1.1".to_string(),
                metric: Some(100),
            },
            Route {
                destination: "10.0.0.0/8".to_string(),
                gateway: "192.168.1.2".to_string(),
                metric: Some(200),
            },
        ];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_dns_config_validation() {
        let mut config = DnsConfig {
            servers: vec!["invalid-ip".to_string()],
            search: vec!["example.com".to_string()],
        };

        // Should fail with invalid DNS server IP
        assert!(config.validate().is_err());

        // Fix DNS server
        config.servers = vec!["8.8.8.8".to_string()];
        assert!(config.validate().is_ok());

        // Test invalid search domain
        config.search = vec!["invalid..domain".to_string()];
        assert!(config.validate().is_err());

        // Fix search domain
        config.search = vec!["example.com".to_string()];
        assert!(config.validate().is_ok());

        // Test duplicate DNS servers
        config.servers = vec!["8.8.8.8".to_string(), "8.8.8.8".to_string()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_environment_variable_substitution() {
        let mut env_vars = HashMap::new();
        env_vars.insert("APP_NAME".to_string(), "test-app".to_string());
        env_vars.insert("NETWORK_PREFIX".to_string(), "192.168.100".to_string());
        env_vars.insert("DNS_SERVER".to_string(), "8.8.8.8".to_string());

        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "${APP_NAME}".to_string(),
                description: "Namespace for ${APP_NAME}".to_string(),
            },
            interfaces: InterfaceConfig {
                move_interfaces: vec!["${APP_NAME}-eth0".to_string()],
                virtual_interfaces: vec![VirtualInterface {
                    name: "veth-${APP_NAME}".to_string(),
                    interface_type: "veth".to_string(),
                    peer: Some("veth-${APP_NAME}-host".to_string()),
                    addresses: vec![],
                }],
            },
            routing: RoutingConfig {
                default_gateway: Some("${NETWORK_PREFIX}.1".to_string()),
                routes: vec![Route {
                    destination: "10.0.0.0/8".to_string(),
                    gateway: "${NETWORK_PREFIX}.1".to_string(),
                    metric: Some(100),
                }],
            },
            dns: DnsConfig {
                servers: vec!["${DNS_SERVER}".to_string()],
                search: vec!["${APP_NAME}.local".to_string()],
            },
            environment: env_vars,
        };

        // Perform substitution
        config.substitute_environment_variables().unwrap();

        // Verify substitutions
        assert_eq!(config.namespace.name, "test-app");
        assert_eq!(config.namespace.description, "Namespace for test-app");
        assert_eq!(config.interfaces.move_interfaces[0], "test-app-eth0");
        assert_eq!(
            config.interfaces.virtual_interfaces[0].name,
            "veth-test-app"
        );
        assert_eq!(
            config.interfaces.virtual_interfaces[0]
                .peer
                .as_ref()
                .unwrap(),
            "veth-test-app-host"
        );
        assert_eq!(
            config.routing.default_gateway.as_ref().unwrap(),
            "192.168.100.1"
        );
        assert_eq!(config.routing.routes[0].gateway, "192.168.100.1");
        assert_eq!(config.dns.servers[0], "8.8.8.8");
        assert_eq!(config.dns.search[0], "test-app.local");
    }

    #[test]
    fn test_environment_variable_substitution_with_defaults() {
        let mut env_vars = HashMap::new();
        env_vars.insert("SET_VAR".to_string(), "set_value".to_string());

        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "${SET_VAR}".to_string(),
                description: "${UNSET_VAR:-default_description}".to_string(),
            },
            interfaces: InterfaceConfig {
                move_interfaces: vec!["${UNSET_VAR:-eth0}".to_string()],
                virtual_interfaces: vec![],
            },
            routing: RoutingConfig {
                default_gateway: Some("${GATEWAY:-192.168.1.1}".to_string()),
                routes: vec![],
            },
            dns: DnsConfig {
                servers: vec!["${DNS:-8.8.8.8}".to_string()],
                search: vec![],
            },
            environment: env_vars,
        };

        // Perform substitution
        config.substitute_environment_variables().unwrap();

        // Verify substitutions with defaults
        assert_eq!(config.namespace.name, "set_value");
        assert_eq!(config.namespace.description, "default_description");
        assert_eq!(config.interfaces.move_interfaces[0], "eth0");
        assert_eq!(
            config.routing.default_gateway.as_ref().unwrap(),
            "192.168.1.1"
        );
        assert_eq!(config.dns.servers[0], "8.8.8.8");
    }

    #[test]
    fn test_environment_variable_substitution_errors() {
        let env_vars = HashMap::new();

        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "${MISSING_VAR}".to_string(), // No default provided
                description: "Test".to_string(),
            },
            interfaces: InterfaceConfig::default(),
            routing: RoutingConfig::default(),
            dns: DnsConfig::default(),
            environment: env_vars,
        };

        // Should fail with missing variable
        let result = config.substitute_environment_variables();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Environment variable 'MISSING_VAR' not found"));
    }

    #[test]
    fn test_environment_variable_substitution_system_env() {
        // Set a system environment variable
        std::env::set_var("SEGWIRE_TEST_SYSTEM_VAR", "system_value");

        let env_vars = HashMap::new();
        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "${SEGWIRE_TEST_SYSTEM_VAR}".to_string(),
                description: "Test".to_string(),
            },
            interfaces: InterfaceConfig::default(),
            routing: RoutingConfig::default(),
            dns: DnsConfig::default(),
            environment: env_vars,
        };

        // Should use system environment variable
        config.substitute_environment_variables().unwrap();
        assert_eq!(config.namespace.name, "system_value");

        // Clean up
        std::env::remove_var("SEGWIRE_TEST_SYSTEM_VAR");
    }

    #[test]
    fn test_environment_variable_substitution_precedence() {
        // Set a system environment variable
        std::env::set_var("SEGWIRE_TEST_PRECEDENCE_VAR", "system_value");

        // Config environment should take precedence
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "SEGWIRE_TEST_PRECEDENCE_VAR".to_string(),
            "config_value".to_string(),
        );

        let mut config = NamespaceConfig {
            namespace: NamespaceSettings {
                name: "${SEGWIRE_TEST_PRECEDENCE_VAR}".to_string(),
                description: "Test".to_string(),
            },
            interfaces: InterfaceConfig::default(),
            routing: RoutingConfig::default(),
            dns: DnsConfig::default(),
            environment: env_vars,
        };

        // Should use config environment variable over system
        config.substitute_environment_variables().unwrap();
        assert_eq!(config.namespace.name, "config_value");

        // Clean up
        std::env::remove_var("SEGWIRE_TEST_PRECEDENCE_VAR");
    }
}
