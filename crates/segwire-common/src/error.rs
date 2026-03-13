//! Error types for the segwire system
//!
//! Provides comprehensive error handling with categorized error types
//! and utilities for error propagation and recovery.

use std::collections::HashMap;
use thiserror::Error;
use tracing::error;

/// Main error type for segwire operations
#[derive(Debug, Error)]
pub enum SegwireError {
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Network operation failed: {0}")]
    Network(String),

    #[error("D-Bus error: {0}")]
    DBus(String),

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

/// Error context for detailed error reporting
#[derive(Debug, Clone)]
pub struct ErrorContext {
    /// Operation that was being performed
    pub operation: String,
    /// Namespace involved (if applicable)
    pub namespace: Option<String>,
    /// Configuration file path (if applicable)
    pub config_path: Option<std::path::PathBuf>,
    /// Additional context fields
    pub fields: HashMap<String, String>,
    /// Suggested remediation steps
    pub remediation: Vec<String>,
}

impl ErrorContext {
    /// Create a new error context
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            namespace: None,
            config_path: None,
            fields: HashMap::new(),
            remediation: Vec::new(),
        }
    }

    /// Add namespace context
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    /// Add configuration file context
    pub fn with_config_path(mut self, path: std::path::PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Add a custom field
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Add a remediation step
    pub fn with_remediation(mut self, step: impl Into<String>) -> Self {
        self.remediation.push(step.into());
        self
    }
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

    /// Get error category for structured logging
    pub fn category(&self) -> &'static str {
        match self {
            SegwireError::Config(_) => "configuration",
            SegwireError::Network(_) => "network",
            SegwireError::DBus(_) => "dbus",
            SegwireError::Permission(_) => "permission",
            SegwireError::System(_) => "system",
            SegwireError::Validation(_) => "validation",
        }
    }

    /// Get suggested remediation steps
    pub fn remediation_steps(&self) -> Vec<String> {
        match self {
            SegwireError::Config(ConfigError::InvalidToml(_)) => vec![
                "Check TOML syntax using a validator".to_string(),
                "Ensure all required fields are present".to_string(),
                "Verify file encoding is UTF-8".to_string(),
            ],
            SegwireError::Config(ConfigError::MissingField(field)) => vec![
                format!("Add the required field '{}' to your configuration", field),
                "Refer to the configuration documentation for field requirements".to_string(),
            ],
            SegwireError::Network(_) => vec![
                "Check network interface availability".to_string(),
                "Verify namespace doesn't already exist".to_string(),
                "Ensure sufficient network privileges".to_string(),
            ],
            SegwireError::Permission(_) => vec![
                "Run with appropriate privileges (CAP_SYS_ADMIN)".to_string(),
                "Check PolicyKit configuration".to_string(),
                "Verify user is in required groups".to_string(),
            ],
            SegwireError::System(_) => vec![
                "Check system resources and limits".to_string(),
                "Verify kernel support for network namespaces".to_string(),
                "Check system logs for additional details".to_string(),
            ],
            _ => vec!["Check logs for additional details".to_string()],
        }
    }

    /// Log this error with full context
    pub fn log_with_context(&self, context: &ErrorContext) {
        let span = tracing::error_span!(
            "error_report",
            operation = %context.operation,
            error_category = self.category(),
            namespace = context.namespace.as_deref(),
            config_path = context.config_path.as_ref().map(|p| p.display().to_string()).as_deref(),
            recoverable = self.is_recoverable(),
        );
        let _enter = span.enter();

        error!("Operation failed: {}", self);
        error!("User message: {}", self.user_message());

        // Log context fields
        for (key, value) in &context.fields {
            error!("Context {}: {}", key, value);
        }

        // Log remediation steps
        if !context.remediation.is_empty() {
            error!("Custom remediation steps:");
            for (i, step) in context.remediation.iter().enumerate() {
                error!("  {}. {}", i + 1, step);
            }
        } else {
            let steps = self.remediation_steps();
            if !steps.is_empty() {
                error!("Suggested remediation steps:");
                for (i, step) in steps.iter().enumerate() {
                    error!("  {}. {}", i + 1, step);
                }
            }
        }
    }

    /// Create an error with context
    pub fn with_context(self, context: ErrorContext) -> ContextualError {
        ContextualError {
            error: self,
            context,
        }
    }
}

/// Error with additional context information
#[derive(Debug)]
pub struct ContextualError {
    pub error: SegwireError,
    pub context: ErrorContext,
}

impl std::fmt::Display for ContextualError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (operation: {})", self.error, self.context.operation)
    }
}

impl std::error::Error for ContextualError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl ContextualError {
    /// Log this contextual error
    pub fn log(&self) {
        self.error.log_with_context(&self.context);
    }

    /// Log and return the inner error
    pub fn log_and_return(self) -> SegwireError {
        self.log();
        self.error
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
            crate::dbus::DbusError::SystemError(msg) => {
                SegwireError::System(std::io::Error::other(msg))
            }
            crate::dbus::DbusError::InternalError(msg) => SegwireError::Network(msg),
        }
    }
}

/// Convenience type alias for Results with SegwireError
pub type SegwireResult<T> = Result<T, SegwireError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_context_creation() {
        let ctx = ErrorContext::new("test_op")
            .with_namespace("test_ns")
            .with_config_path(std::path::PathBuf::from("/etc/test.toml"))
            .with_field("key", "value")
            .with_remediation("fix it");

        assert_eq!(ctx.operation, "test_op");
        assert_eq!(ctx.namespace.as_deref(), Some("test_ns"));
        assert_eq!(
            ctx.config_path.as_deref(),
            Some(std::path::Path::new("/etc/test.toml"))
        );
        assert_eq!(ctx.fields.get("key").map(|s| s.as_str()), Some("value"));
        assert_eq!(ctx.remediation, vec!["fix it"]);
    }

    #[test]
    fn test_segwire_error_recoverable() {
        assert!(SegwireError::Config(ConfigError::FileNotFound("".into())).is_recoverable());
        assert!(SegwireError::Network("".into()).is_recoverable());
        assert!(SegwireError::Validation("".into()).is_recoverable());
        assert!(!SegwireError::Permission("".into()).is_recoverable());
        assert!(!SegwireError::System(std::io::Error::from_raw_os_error(1)).is_recoverable());
    }

    #[test]
    fn test_segwire_error_category() {
        assert_eq!(
            SegwireError::Config(ConfigError::FileNotFound("".into())).category(),
            "configuration"
        );
        assert_eq!(SegwireError::Network("".into()).category(), "network");
        assert_eq!(SegwireError::Permission("".into()).category(), "permission");
        assert_eq!(
            SegwireError::System(std::io::Error::from_raw_os_error(1)).category(),
            "system"
        );
        assert_eq!(SegwireError::Validation("".into()).category(), "validation");
    }

    #[test]
    fn test_segwire_error_remediation() {
        let config_err = SegwireError::Config(ConfigError::MissingField("test".into()));
        assert!(config_err
            .remediation_steps()
            .iter()
            .any(|s| s.contains("test")));

        let perm_err = SegwireError::Permission("".into());
        assert!(perm_err
            .remediation_steps()
            .iter()
            .any(|s| s.contains("CAP_SYS_ADMIN")));
    }

    #[test]
    fn test_contextual_error() {
        let err = SegwireError::Network("test".into());
        let ctx_err = err.with_context(ErrorContext::new("op"));
        assert_eq!(
            ctx_err.to_string(),
            "Network operation failed: test (operation: op)"
        );

        let returned_err = ctx_err.log_and_return();
        assert!(matches!(returned_err, SegwireError::Network(_)));
    }
}
