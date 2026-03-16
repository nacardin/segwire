//! D-Bus interface definitions and types
//!
//! Defines all D-Bus interface structures, method signatures, and data types
//! used for communication between the daemon and CLI components.

use crate::error::SegwireError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;
use zvariant::Type;

/// D-Bus interface name for the namespace manager
pub const DBUS_INTERFACE_NAME: &str = "org.segwire.NamespaceManager";

/// D-Bus service name
pub const DBUS_SERVICE_NAME: &str = "org.segwire.NamespaceManager";

/// D-Bus object path
pub const DBUS_OBJECT_PATH: &str = "/org/segwire/NamespaceManager";

/// Namespace state information for D-Bus communication
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct NamespaceState {
    pub name: String,
    pub full_name: String, // prefixed name
    pub status: String,    // Status as string for D-Bus compatibility
    pub config_path: String,
    pub interfaces: Vec<InterfaceInfo>,
    pub routes: Vec<RouteInfo>,
    pub dns_config: DnsInfo,
    pub created_at: u64,   // Unix timestamp
    pub last_updated: u64, // Unix timestamp
}

/// Namespace status enumeration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum NamespaceStatus {
    Creating,
    Active,
    Failed,
    Deleting,
}

/// Network interface information
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct InterfaceInfo {
    pub name: String,
    pub interface_type: String, // physical, virtual, etc.
    pub status: String,         // up, down, etc.
    pub addresses: Vec<String>,
}

/// Route information
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RouteInfo {
    pub destination: String,
    pub gateway: String,
    pub metric: u32,
    pub interface: String,
}

/// DNS configuration information
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DnsInfo {
    pub servers: Vec<String>,
    pub search_domains: Vec<String>,
}

/// D-Bus method call results
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct OperationResult {
    pub success: bool,
    pub message: String,
    pub details: HashMap<String, String>,
}

/// Configuration validation result
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Progress information for long-running operations
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct OperationProgress {
    pub operation: String,
    pub progress: f64, // 0.0 to 1.0
    pub message: String,
    pub current_step: String,
}

impl NamespaceState {
    /// Create a new namespace state
    pub fn new(name: String, full_name: String, config_path: PathBuf) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            name,
            full_name,
            status: "creating".to_string(),
            config_path: config_path.display().to_string(),
            interfaces: Vec::new(),
            routes: Vec::new(),
            dns_config: DnsInfo {
                servers: Vec::new(),
                search_domains: Vec::new(),
            },
            created_at: now,
            last_updated: now,
        }
    }

    /// Update the last modified timestamp
    pub fn touch(&mut self) {
        self.last_updated = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }

    /// Check if the namespace is in an active state
    pub fn is_active(&self) -> bool {
        self.parsed_status() == NamespaceStatus::Active
    }

    /// Check if the namespace is in a failed state
    pub fn is_failed(&self) -> bool {
        self.parsed_status() == NamespaceStatus::Failed
    }

    /// Parse the status string into a NamespaceStatus enum.
    /// Returns `Failed` for unrecognised strings (including "failed: ...").
    pub fn parsed_status(&self) -> NamespaceStatus {
        self.status.parse().unwrap_or(NamespaceStatus::Failed)
    }

    /// Set the status with a NamespaceStatus enum
    pub fn set_status(&mut self, status: NamespaceStatus) {
        self.status = status.to_string();
        self.touch();
    }
}

impl NamespaceStatus {
    /// Get a human-readable status string
    pub fn as_str(&self) -> &str {
        match self {
            NamespaceStatus::Creating => "creating",
            NamespaceStatus::Active => "active",
            NamespaceStatus::Failed => "failed",
            NamespaceStatus::Deleting => "deleting",
        }
    }

    /// Check if the status indicates an error condition
    pub fn is_error(&self) -> bool {
        matches!(self, NamespaceStatus::Failed)
    }
}

impl std::fmt::Display for NamespaceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for NamespaceStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "creating" => Ok(NamespaceStatus::Creating),
            "active" => Ok(NamespaceStatus::Active),
            "failed" => Ok(NamespaceStatus::Failed),
            "deleting" => Ok(NamespaceStatus::Deleting),
            _ => Err(format!("unknown namespace status: {}", s)),
        }
    }
}

impl OperationResult {
    /// Create a successful operation result
    pub fn success(message: String) -> Self {
        Self {
            success: true,
            message,
            details: HashMap::new(),
        }
    }

    /// Create a failed operation result
    pub fn failure(message: String) -> Self {
        Self {
            success: false,
            message,
            details: HashMap::new(),
        }
    }

    /// Add detail information to the result
    pub fn with_detail(mut self, key: String, value: String) -> Self {
        self.details.insert(key, value);
        self
    }
}

/// D-Bus error types with descriptive messages
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub enum DbusError {
    /// Configuration-related errors
    ConfigurationError(String),
    /// Network operation errors
    NetworkError(String),
    /// Permission/authorization errors
    PermissionDenied(String),
    /// Namespace not found
    NamespaceNotFound(String),
    /// Invalid operation state
    InvalidState(String),
    /// System resource errors
    SystemError(String),
    /// Internal daemon errors
    InternalError(String),
}

impl std::fmt::Display for DbusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl DbusError {
    /// Get the D-Bus error name for this error type
    pub fn error_name(&self) -> &'static str {
        match self {
            DbusError::ConfigurationError(_) => "org.segwire.Error.Configuration",
            DbusError::NetworkError(_) => "org.segwire.Error.Network",
            DbusError::PermissionDenied(_) => "org.segwire.Error.PermissionDenied",
            DbusError::NamespaceNotFound(_) => "org.segwire.Error.NamespaceNotFound",
            DbusError::InvalidState(_) => "org.segwire.Error.InvalidState",
            DbusError::SystemError(_) => "org.segwire.Error.System",
            DbusError::InternalError(_) => "org.segwire.Error.Internal",
        }
    }

    /// Get the error message
    pub fn message(&self) -> &str {
        match self {
            DbusError::ConfigurationError(msg) => msg,
            DbusError::NetworkError(msg) => msg,
            DbusError::PermissionDenied(msg) => msg,
            DbusError::NamespaceNotFound(msg) => msg,
            DbusError::InvalidState(msg) => msg,
            DbusError::SystemError(msg) => msg,
            DbusError::InternalError(msg) => msg,
        }
    }
}

impl From<SegwireError> for DbusError {
    fn from(error: SegwireError) -> Self {
        match error {
            SegwireError::Config(msg) => DbusError::ConfigurationError(msg.to_string()),
            SegwireError::Network(msg) => DbusError::NetworkError(msg.to_string()),
            SegwireError::Permission(msg) => DbusError::PermissionDenied(msg),
            SegwireError::System(msg) => DbusError::SystemError(msg.to_string()),
            SegwireError::DBus(msg) => DbusError::InternalError(msg.to_string()),
            SegwireError::Validation(msg) => DbusError::ConfigurationError(msg),
        }
    }
}

/// Method signatures for the D-Bus interface
/// These are the method definitions that both the daemon and CLI need to know about
pub mod method_signatures {
    use super::*;

    /// List all managed namespaces with basic information
    /// Returns: Array of (name, status, config_path, description)
    pub type ListNamespacesResult = Vec<(String, String, String, String)>;

    /// Get detailed status information for a specific namespace
    pub type GetNamespaceStatusResult = NamespaceState;

    /// Standard operation result type
    pub type StandardOperationResult = OperationResult;

    /// Configuration validation result
    pub type ConfigValidationResult = ValidationResult;

    /// Daemon status information
    /// Returns: (version, uptime, managed_count, active_count)
    pub type DaemonStatusResult = (String, u64, u32, u32);
}

/// Signal definitions for D-Bus communication
pub mod signals {
    /// Signal emitted when a namespace is created
    /// Args: (name, config_path)
    pub type NamespaceCreated = (String, String);

    /// Signal emitted when a namespace is deleted
    /// Args: (name, reason)
    pub type NamespaceDeleted = (String, String);

    /// Signal emitted when configuration is reloaded
    /// Args: (count, errors)
    pub type ConfigurationReloaded = (u32, u32);

    /// Signal emitted for operation progress updates
    /// Args: (operation, progress, message)
    pub type ProgressUpdate = (String, f64, String);

    /// Signal emitted when a namespace status changes
    /// Args: (name, old_status, new_status)
    pub type NamespaceStatusChanged = (String, String, String);

    /// Signal emitted when an error occurs
    /// Args: (error_type, message, namespace)
    pub type ErrorOccurred = (String, String, String);
}

/// D-Bus interface constants and method signatures
/// These define the interface contract that both daemon and CLI must follow
pub mod interface {

    /// D-Bus interface name
    pub const INTERFACE_NAME: &str = "org.segwire.NamespaceManager";

    /// D-Bus service name
    pub const SERVICE_NAME: &str = "org.segwire.NamespaceManager";

    /// D-Bus object path
    pub const OBJECT_PATH: &str = "/org/segwire/NamespaceManager";

    /// Method names as constants
    pub const METHOD_LIST_NAMESPACES: &str = "ListNamespaces";
    pub const METHOD_GET_NAMESPACE_STATUS: &str = "GetNamespaceStatus";
    pub const METHOD_DELETE_NAMESPACE: &str = "DeleteNamespace";
    pub const METHOD_RELOAD_CONFIGURATION: &str = "ReloadConfiguration";
    pub const METHOD_VALIDATE_CONFIGURATION: &str = "ValidateConfiguration";
    pub const METHOD_GET_DAEMON_STATUS: &str = "GetDaemonStatus";
    pub const METHOD_RESTART_NAMESPACE: &str = "RestartNamespace";

    /// Signal names as constants
    pub const SIGNAL_NAMESPACE_CREATED: &str = "NamespaceCreated";
    pub const SIGNAL_NAMESPACE_DELETED: &str = "NamespaceDeleted";
    pub const SIGNAL_CONFIGURATION_RELOADED: &str = "ConfigurationReloaded";
    pub const SIGNAL_OPERATION_PROGRESS: &str = "OperationProgress";
    pub const SIGNAL_NAMESPACE_STATUS_CHANGED: &str = "NamespaceStatusChanged";
    pub const SIGNAL_ERROR_OCCURRED: &str = "ErrorOccurred";

    /// Method information for introspection
    #[derive(Debug, Clone)]
    pub struct MethodInfo {
        pub name: &'static str,
        pub description: &'static str,
        pub input_args: Vec<ArgInfo>,
        pub output_args: Vec<ArgInfo>,
    }

    /// Argument information for methods and signals
    #[derive(Debug, Clone)]
    pub struct ArgInfo {
        pub name: &'static str,
        pub type_signature: &'static str,
        pub description: &'static str,
    }

    /// Signal information for introspection
    #[derive(Debug, Clone)]
    pub struct SignalInfo {
        pub name: &'static str,
        pub description: &'static str,
        pub args: Vec<ArgInfo>,
    }

    /// Get all available methods with their signatures and descriptions
    pub fn get_methods() -> Vec<MethodInfo> {
        vec![
            MethodInfo {
                name: METHOD_LIST_NAMESPACES,
                description: "List all managed namespaces with basic information",
                input_args: vec![],
                output_args: vec![ArgInfo {
                    name: "namespaces",
                    type_signature: "a(ssss)",
                    description: "Array of (name, status, config_path, description) tuples",
                }],
            },
            MethodInfo {
                name: METHOD_GET_NAMESPACE_STATUS,
                description: "Get detailed status information for a specific namespace",
                input_args: vec![ArgInfo {
                    name: "name",
                    type_signature: "s",
                    description: "Namespace name to query",
                }],
                output_args: vec![ArgInfo {
                    name: "status",
                    type_signature: "(sssasasas)",
                    description: "Namespace state information",
                }],
            },
            MethodInfo {
                name: METHOD_DELETE_NAMESPACE,
                description: "Delete a managed namespace",
                input_args: vec![ArgInfo {
                    name: "name",
                    type_signature: "s",
                    description: "Namespace name to delete",
                }],
                output_args: vec![ArgInfo {
                    name: "result",
                    type_signature: "(bsa{ss})",
                    description: "Operation result with success flag, message, and details",
                }],
            },
            MethodInfo {
                name: METHOD_RELOAD_CONFIGURATION,
                description: "Reload all configuration files and update namespaces",
                input_args: vec![],
                output_args: vec![ArgInfo {
                    name: "result",
                    type_signature: "(bsa{ss})",
                    description: "Operation result with success flag, message, and details",
                }],
            },
            MethodInfo {
                name: METHOD_VALIDATE_CONFIGURATION,
                description: "Validate a configuration file without applying it",
                input_args: vec![ArgInfo {
                    name: "config_path",
                    type_signature: "s",
                    description: "Path to configuration file to validate",
                }],
                output_args: vec![ArgInfo {
                    name: "validation",
                    type_signature: "(basas)",
                    description: "Validation result with valid flag, errors, and warnings",
                }],
            },
            MethodInfo {
                name: METHOD_GET_DAEMON_STATUS,
                description: "Get daemon status and statistics",
                input_args: vec![],
                output_args: vec![ArgInfo {
                    name: "status",
                    type_signature: "(stuu)",
                    description: "Daemon status: (version, uptime, managed_count, active_count)",
                }],
            },
            MethodInfo {
                name: METHOD_RESTART_NAMESPACE,
                description: "Restart a specific namespace (delete and recreate)",
                input_args: vec![ArgInfo {
                    name: "name",
                    type_signature: "s",
                    description: "Namespace name to restart",
                }],
                output_args: vec![ArgInfo {
                    name: "result",
                    type_signature: "(bsa{ss})",
                    description: "Operation result with success flag, message, and details",
                }],
            },
        ]
    }

    /// Get all available signals with their signatures and descriptions
    pub fn get_signals() -> Vec<SignalInfo> {
        vec![
            SignalInfo {
                name: SIGNAL_NAMESPACE_CREATED,
                description: "Signal emitted when a namespace is created",
                args: vec![
                    ArgInfo {
                        name: "name",
                        type_signature: "s",
                        description: "Name of the created namespace",
                    },
                    ArgInfo {
                        name: "config_path",
                        type_signature: "s",
                        description: "Path to the configuration file used",
                    },
                ],
            },
            SignalInfo {
                name: SIGNAL_NAMESPACE_DELETED,
                description: "Signal emitted when a namespace is deleted",
                args: vec![
                    ArgInfo {
                        name: "name",
                        type_signature: "s",
                        description: "Name of the deleted namespace",
                    },
                    ArgInfo {
                        name: "reason",
                        type_signature: "s",
                        description: "Reason for deletion",
                    },
                ],
            },
            SignalInfo {
                name: SIGNAL_CONFIGURATION_RELOADED,
                description: "Signal emitted when configuration is reloaded",
                args: vec![
                    ArgInfo {
                        name: "count",
                        type_signature: "u",
                        description: "Number of configurations processed",
                    },
                    ArgInfo {
                        name: "errors",
                        type_signature: "u",
                        description: "Number of errors encountered",
                    },
                ],
            },
            SignalInfo {
                name: SIGNAL_OPERATION_PROGRESS,
                description: "Signal emitted for operation progress updates",
                args: vec![
                    ArgInfo {
                        name: "operation",
                        type_signature: "s",
                        description: "Name of the operation in progress",
                    },
                    ArgInfo {
                        name: "progress",
                        type_signature: "d",
                        description: "Progress as a value between 0.0 and 1.0",
                    },
                    ArgInfo {
                        name: "message",
                        type_signature: "s",
                        description: "Human-readable progress message",
                    },
                ],
            },
            SignalInfo {
                name: SIGNAL_NAMESPACE_STATUS_CHANGED,
                description: "Signal emitted when a namespace status changes",
                args: vec![
                    ArgInfo {
                        name: "name",
                        type_signature: "s",
                        description: "Name of the namespace",
                    },
                    ArgInfo {
                        name: "old_status",
                        type_signature: "s",
                        description: "Previous status",
                    },
                    ArgInfo {
                        name: "new_status",
                        type_signature: "s",
                        description: "New status",
                    },
                ],
            },
            SignalInfo {
                name: SIGNAL_ERROR_OCCURRED,
                description: "Signal emitted when an error occurs",
                args: vec![
                    ArgInfo {
                        name: "error_type",
                        type_signature: "s",
                        description: "Type of error that occurred",
                    },
                    ArgInfo {
                        name: "message",
                        type_signature: "s",
                        description: "Error message",
                    },
                    ArgInfo {
                        name: "namespace",
                        type_signature: "s",
                        description: "Namespace associated with the error (if any)",
                    },
                ],
            },
        ]
    }

    /// Find a method by name
    pub fn find_method(name: &str) -> Option<MethodInfo> {
        get_methods().into_iter().find(|m| m.name == name)
    }

    /// Find a signal by name
    pub fn find_signal(name: &str) -> Option<SignalInfo> {
        get_signals().into_iter().find(|s| s.name == name)
    }

    /// Get method names as a list
    pub fn get_method_names() -> Vec<&'static str> {
        get_methods().into_iter().map(|m| m.name).collect()
    }

    /// Get signal names as a list
    pub fn get_signal_names() -> Vec<&'static str> {
        get_signals().into_iter().map(|s| s.name).collect()
    }

    /// Generate D-Bus introspection XML for the interface
    pub fn introspection_xml() -> &'static str {
        r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.segwire.NamespaceManager">
    <!-- Methods -->
    <method name="ListNamespaces">
      <arg direction="out" name="namespaces" type="a(ssss)"/>
    </method>
    
    <method name="GetNamespaceStatus">
      <arg direction="in" name="name" type="s"/>
      <arg direction="out" name="status" type="(sssasasas)"/>
    </method>
    
    <method name="DeleteNamespace">
      <arg direction="in" name="name" type="s"/>
      <arg direction="out" name="result" type="(bsa{ss})"/>
    </method>
    
    <method name="ReloadConfiguration">
      <arg direction="out" name="result" type="(bsa{ss})"/>
    </method>
    
    <method name="ValidateConfiguration">
      <arg direction="in" name="config_path" type="s"/>
      <arg direction="out" name="validation" type="(basas)"/>
    </method>
    
    <method name="GetDaemonStatus">
      <arg direction="out" name="status" type="(stuu)"/>
    </method>
    
    <method name="RestartNamespace">
      <arg direction="in" name="name" type="s"/>
      <arg direction="out" name="result" type="(bsa{ss})"/>
    </method>
    
    <!-- Signals -->
    <signal name="NamespaceCreated">
      <arg name="name" type="s"/>
      <arg name="config_path" type="s"/>
    </signal>
    
    <signal name="NamespaceDeleted">
      <arg name="name" type="s"/>
      <arg name="reason" type="s"/>
    </signal>
    
    <signal name="ConfigurationReloaded">
      <arg name="count" type="u"/>
      <arg name="errors" type="u"/>
    </signal>
    
    <signal name="OperationProgress">
      <arg name="operation" type="s"/>
      <arg name="progress" type="d"/>
      <arg name="message" type="s"/>
    </signal>
    
    <signal name="NamespaceStatusChanged">
      <arg name="name" type="s"/>
      <arg name="old_status" type="s"/>
      <arg name="new_status" type="s"/>
    </signal>
    
    <signal name="ErrorOccurred">
      <arg name="error_type" type="s"/>
      <arg name="message" type="s"/>
      <arg name="namespace" type="s"/>
    </signal>
  </interface>
</node>"#
    }
}

/// Helper functions for D-Bus interface management
pub mod interface_helpers {
    use super::*;

    /// Create a D-Bus error from a SegwireError
    pub fn create_dbus_error(error: SegwireError) -> zbus::fdo::Error {
        let dbus_error = DbusError::from(error);
        zbus::fdo::Error::Failed(dbus_error.message().to_string())
    }

    /// Convert a namespace list to D-Bus tuple format
    pub fn namespaces_to_tuples(
        namespaces: Vec<NamespaceState>,
    ) -> Vec<(String, String, String, String)> {
        namespaces
            .into_iter()
            .map(|ns| (ns.name, ns.status, ns.config_path, "".to_string())) // description placeholder
            .collect()
    }

    /// Create daemon status tuple
    pub fn create_daemon_status(
        version: String,
        uptime: u64,
        managed: u32,
        active: u32,
    ) -> (String, u64, u32, u32) {
        (version, uptime, managed, active)
    }

    /// Validate D-Bus method parameters
    pub fn validate_namespace_name(name: &str) -> Result<(), DbusError> {
        if name.is_empty() {
            return Err(DbusError::ConfigurationError(
                "Namespace name cannot be empty".to_string(),
            ));
        }

        if name.len() > 255 {
            return Err(DbusError::ConfigurationError(
                "Namespace name too long".to_string(),
            ));
        }

        // Check for valid characters (alphanumeric, hyphens, underscores)
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(DbusError::ConfigurationError(
                "Namespace name contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate configuration file path
    pub fn validate_config_path(path: &str) -> Result<(), DbusError> {
        if path.is_empty() {
            return Err(DbusError::ConfigurationError(
                "Configuration path cannot be empty".to_string(),
            ));
        }

        if !path.ends_with(".toml") {
            return Err(DbusError::ConfigurationError(
                "Configuration file must have .toml extension".to_string(),
            ));
        }

        // Basic path traversal protection
        if path.contains("..") {
            return Err(DbusError::ConfigurationError(
                "Path traversal not allowed".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interface_constants() {
        assert_eq!(interface::INTERFACE_NAME, "org.segwire.NamespaceManager");
        assert_eq!(interface::SERVICE_NAME, "org.segwire.NamespaceManager");
        assert_eq!(interface::OBJECT_PATH, "/org/segwire/NamespaceManager");
    }

    #[test]
    fn test_method_discovery() {
        let methods = interface::get_methods();
        assert!(!methods.is_empty());

        // Check that all expected methods are present
        let method_names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(method_names.contains(&interface::METHOD_LIST_NAMESPACES));
        assert!(method_names.contains(&interface::METHOD_GET_NAMESPACE_STATUS));
        assert!(method_names.contains(&interface::METHOD_DELETE_NAMESPACE));
        assert!(method_names.contains(&interface::METHOD_RELOAD_CONFIGURATION));
        assert!(method_names.contains(&interface::METHOD_VALIDATE_CONFIGURATION));
        assert!(method_names.contains(&interface::METHOD_GET_DAEMON_STATUS));
        assert!(method_names.contains(&interface::METHOD_RESTART_NAMESPACE));
    }

    #[test]
    fn test_signal_discovery() {
        let signals = interface::get_signals();
        assert!(!signals.is_empty());

        // Check that all expected signals are present
        let signal_names: Vec<&str> = signals.iter().map(|s| s.name).collect();
        assert!(signal_names.contains(&interface::SIGNAL_NAMESPACE_CREATED));
        assert!(signal_names.contains(&interface::SIGNAL_NAMESPACE_DELETED));
        assert!(signal_names.contains(&interface::SIGNAL_CONFIGURATION_RELOADED));
        assert!(signal_names.contains(&interface::SIGNAL_OPERATION_PROGRESS));
        assert!(signal_names.contains(&interface::SIGNAL_NAMESPACE_STATUS_CHANGED));
        assert!(signal_names.contains(&interface::SIGNAL_ERROR_OCCURRED));
    }

    #[test]
    fn test_method_lookup() {
        // Test finding existing method
        let method = interface::find_method(interface::METHOD_LIST_NAMESPACES);
        assert!(method.is_some());
        let method = method.unwrap();
        assert_eq!(method.name, interface::METHOD_LIST_NAMESPACES);
        assert!(!method.description.is_empty());

        // Test finding non-existent method
        let method = interface::find_method("NonExistentMethod");
        assert!(method.is_none());
    }

    #[test]
    fn test_signal_lookup() {
        // Test finding existing signal
        let signal = interface::find_signal(interface::SIGNAL_NAMESPACE_CREATED);
        assert!(signal.is_some());
        let signal = signal.unwrap();
        assert_eq!(signal.name, interface::SIGNAL_NAMESPACE_CREATED);
        assert!(!signal.description.is_empty());
        assert_eq!(signal.args.len(), 2); // name and config_path

        // Test finding non-existent signal
        let signal = interface::find_signal("NonExistentSignal");
        assert!(signal.is_none());
    }

    #[test]
    fn test_method_signatures() {
        let methods = interface::get_methods();

        // Test ListNamespaces method
        let list_method = methods
            .iter()
            .find(|m| m.name == interface::METHOD_LIST_NAMESPACES)
            .expect("ListNamespaces method should exist");
        assert!(list_method.input_args.is_empty());
        assert_eq!(list_method.output_args.len(), 1);
        assert_eq!(list_method.output_args[0].type_signature, "a(ssss)");

        // Test GetNamespaceStatus method
        let status_method = methods
            .iter()
            .find(|m| m.name == interface::METHOD_GET_NAMESPACE_STATUS)
            .expect("GetNamespaceStatus method should exist");
        assert_eq!(status_method.input_args.len(), 1);
        assert_eq!(status_method.input_args[0].type_signature, "s");
        assert_eq!(status_method.output_args.len(), 1);
    }

    #[test]
    fn test_signal_signatures() {
        let signals = interface::get_signals();

        // Test NamespaceCreated signal
        let created_signal = signals
            .iter()
            .find(|s| s.name == interface::SIGNAL_NAMESPACE_CREATED)
            .expect("NamespaceCreated signal should exist");
        assert_eq!(created_signal.args.len(), 2);
        assert_eq!(created_signal.args[0].type_signature, "s");
        assert_eq!(created_signal.args[1].type_signature, "s");

        // Test OperationProgress signal
        let progress_signal = signals
            .iter()
            .find(|s| s.name == interface::SIGNAL_OPERATION_PROGRESS)
            .expect("OperationProgress signal should exist");
        assert_eq!(progress_signal.args.len(), 3);
        assert_eq!(progress_signal.args[0].type_signature, "s"); // operation
        assert_eq!(progress_signal.args[1].type_signature, "d"); // progress (double)
        assert_eq!(progress_signal.args[2].type_signature, "s"); // message
    }

    #[test]
    fn test_introspection_xml() {
        let xml = interface::introspection_xml();
        assert!(!xml.is_empty());

        // Check that XML contains expected elements
        assert!(xml.contains("org.segwire.NamespaceManager"));
        assert!(xml.contains("ListNamespaces"));
        assert!(xml.contains("GetNamespaceStatus"));
        assert!(xml.contains("NamespaceCreated"));
        assert!(xml.contains("OperationProgress"));

        // Check that it's valid XML structure
        assert!(xml.starts_with("<!DOCTYPE"));
        assert!(xml.contains("<node>"));
        assert!(xml.contains("</node>"));
        assert!(xml.contains("<interface"));
        assert!(xml.contains("</interface>"));
    }

    #[test]
    fn test_method_enumeration() {
        let method_names = interface::get_method_names();
        assert_eq!(method_names.len(), 7); // We should have 7 methods

        let signal_names = interface::get_signal_names();
        assert_eq!(signal_names.len(), 6); // We should have 6 signals
    }

    #[test]
    fn test_dbus_error_types() {
        let config_error = DbusError::ConfigurationError("test config error".to_string());
        assert_eq!(config_error.error_name(), "org.segwire.Error.Configuration");
        assert_eq!(config_error.message(), "test config error");

        let network_error = DbusError::NetworkError("test network error".to_string());
        assert_eq!(network_error.error_name(), "org.segwire.Error.Network");
        assert_eq!(network_error.message(), "test network error");

        let permission_error = DbusError::PermissionDenied("test permission error".to_string());
        assert_eq!(
            permission_error.error_name(),
            "org.segwire.Error.PermissionDenied"
        );
        assert_eq!(permission_error.message(), "test permission error");
    }

    #[test]
    fn test_namespace_state_creation() {
        let state = NamespaceState::new(
            "test-ns".to_string(),
            "segwire-test-ns".to_string(),
            PathBuf::from("/etc/segwire/test.toml"),
        );

        assert_eq!(state.name, "test-ns");
        assert_eq!(state.full_name, "segwire-test-ns");
        assert_eq!(state.status, "creating");
        assert_eq!(state.parsed_status(), NamespaceStatus::Creating);
        assert_eq!(state.config_path, "/etc/segwire/test.toml");
        assert!(state.interfaces.is_empty());
        assert!(state.routes.is_empty());
        assert!(state.dns_config.servers.is_empty());
        assert!(state.created_at > 0);
        assert!(state.last_updated > 0);
    }

    #[test]
    fn test_namespace_status_enum() {
        assert_eq!(NamespaceStatus::Creating.as_str(), "creating");
        assert_eq!(NamespaceStatus::Active.as_str(), "active");
        assert_eq!(NamespaceStatus::Failed.as_str(), "failed");
        assert_eq!(NamespaceStatus::Deleting.as_str(), "deleting");

        assert!(!NamespaceStatus::Creating.is_error());
        assert!(!NamespaceStatus::Active.is_error());
        assert!(NamespaceStatus::Failed.is_error());
        assert!(!NamespaceStatus::Deleting.is_error());
    }

    #[test]
    fn test_operation_result() {
        let success = OperationResult::success("Operation completed".to_string());
        assert!(success.success);
        assert_eq!(success.message, "Operation completed");
        assert!(success.details.is_empty());

        let failure = OperationResult::failure("Operation failed".to_string());
        assert!(!failure.success);
        assert_eq!(failure.message, "Operation failed");
        assert!(failure.details.is_empty());

        let with_details = success.with_detail("key".to_string(), "value".to_string());
        assert_eq!(with_details.details.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_validation_helpers() {
        use interface_helpers::*;

        // Test valid namespace name
        assert!(validate_namespace_name("valid-namespace").is_ok());
        assert!(validate_namespace_name("valid_namespace").is_ok());
        assert!(validate_namespace_name("namespace123").is_ok());

        // Test invalid namespace names
        assert!(validate_namespace_name("").is_err());
        assert!(validate_namespace_name("invalid namespace").is_err()); // space
        assert!(validate_namespace_name("invalid@namespace").is_err()); // special char

        // Test valid config path
        assert!(validate_config_path("/etc/segwire/test.toml").is_ok());
        assert!(validate_config_path("config.toml").is_ok());

        // Test invalid config paths
        assert!(validate_config_path("").is_err());
        assert!(validate_config_path("config.txt").is_err()); // wrong extension
        assert!(validate_config_path("../config.toml").is_err()); // path traversal
    }
}
