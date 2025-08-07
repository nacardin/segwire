//! D-Bus interface definitions and types
//! 
//! Defines all D-Bus interface structures, method signatures, and data types
//! used for communication between the daemon and CLI components.

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
    pub status: String, // Status as string for D-Bus compatibility
    pub config_path: String,
    pub interfaces: Vec<InterfaceInfo>,
    pub routes: Vec<RouteInfo>,
    pub dns_config: DnsInfo,
    pub created_at: u64, // Unix timestamp
    pub last_updated: u64, // Unix timestamp
}

/// Namespace status enumeration
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
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
    pub status: String, // up, down, etc.
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
        self.status == "active"
    }
    
    /// Check if the namespace is in a failed state
    pub fn is_failed(&self) -> bool {
        self.status.starts_with("failed")
    }
    
    /// Set the status with a NamespaceStatus enum
    pub fn set_status(&mut self, status: NamespaceStatus) {
        self.status = status.as_str().to_string();
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