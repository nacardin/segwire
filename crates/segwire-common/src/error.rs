//! Error types for the segwire system
//! 
//! Provides comprehensive error handling with categorized error types
//! and utilities for error propagation and recovery.

use thiserror::Error;

/// Main error type for segwire operations
#[derive(Debug, Error)]
pub enum SegwireError {
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),
    
    #[error("Network operation failed: {0}")]
    Network(String),
    
    #[error("D-Bus error: {0}")]
    DBus(#[from] zbus::Error),
    
    #[error("Permission denied: {0}")]
    Permission(String),
    
    #[error("System error: {0}")]
    System(#[from] std::io::Error),
    
    #[error("Validation error: {0}")]
    Validation(String),
}

/// Configuration-related errors
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Invalid TOML syntax: {0}")]
    InvalidToml(#[from] toml::de::Error),
    
    #[error("Missing required field: {0}")]
    MissingField(String),
    
    #[error("Invalid value for field '{field}': {value}")]
    InvalidValue { field: String, value: String },
    
    #[error("Configuration file not found: {0}")]
    FileNotFound(String),
    
    #[error("Environment variable substitution failed: {0}")]
    EnvSubstitution(String),
}

/// Network operation errors
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("Network interface '{0}' not found")]
    InterfaceNotFound(String),
    
    #[error("Namespace '{0}' already exists")]
    NamespaceExists(String),
    
    #[error("Namespace '{0}' not found")]
    NamespaceNotFound(String),
    
    #[error("Failed to create namespace: {0}")]
    NamespaceCreationFailed(String),
    
    #[error("Invalid routing configuration: {0}")]
    InvalidRoute(String),
    
    #[error("DNS configuration error: {0}")]
    DnsError(String),
}

impl SegwireError {
    /// Check if the error is recoverable and operations can continue
    pub fn is_recoverable(&self) -> bool {
        match self {
            SegwireError::Config(_) => true,
            SegwireError::Network(_) => true,
            SegwireError::Permission(_) => false,
            SegwireError::System(_) => false,
            SegwireError::DBus(_) => true,
            SegwireError::Validation(_) => true,
        }
    }
    
    /// Get a user-friendly error message
    pub fn user_message(&self) -> String {
        match self {
            SegwireError::Config(e) => format!("Configuration problem: {}", e),
            SegwireError::Network(e) => format!("Network operation failed: {}", e),
            SegwireError::DBus(_) => "Communication error with daemon".to_string(),
            SegwireError::Permission(msg) => format!("Permission denied: {}", msg),
            SegwireError::System(e) => format!("System error: {}", e),
            SegwireError::Validation(msg) => format!("Validation failed: {}", msg),
        }
    }
}

impl From<crate::dbus::DbusError> for SegwireError {
    fn from(error: crate::dbus::DbusError) -> Self {
        match error {
            crate::dbus::DbusError::ConfigurationError(msg) => SegwireError::Validation(msg),
            crate::dbus::DbusError::NetworkError(msg) => SegwireError::Network(msg),
            crate::dbus::DbusError::PermissionDenied(msg) => SegwireError::Permission(msg),
            crate::dbus::DbusError::NamespaceNotFound(msg) => SegwireError::Network(msg),
            crate::dbus::DbusError::InvalidState(msg) => SegwireError::Validation(msg),
            crate::dbus::DbusError::SystemError(msg) => SegwireError::System(std::io::Error::new(std::io::ErrorKind::Other, msg)),
            crate::dbus::DbusError::InternalError(msg) => SegwireError::Network(msg),
        }
    }
}

/// Convenience type alias for Results with SegwireError
pub type SegwireResult<T> = Result<T, SegwireError>;