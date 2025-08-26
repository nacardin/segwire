//! Logging configuration and utilities for segwire
//!
//! Provides structured logging with tracing for console output.

use std::path::PathBuf;
use tracing::Level;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Registry,
};

/// Log level configuration
#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Level::TRACE,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Info => Level::INFO,
            LogLevel::Warn => Level::WARN,
            LogLevel::Error => Level::ERROR,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "trace"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "trace" => Ok(LogLevel::Trace),
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            _ => Err(format!("Invalid log level: {}", s)),
        }
    }
}

/// Logging configuration
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Log level
    pub level: LogLevel,
    /// Include timestamps
    pub with_timestamps: bool,
    /// Include thread names
    pub with_thread_names: bool,
    /// Include file and line information
    pub with_file_line: bool,
    /// Include span information
    pub with_spans: bool,
    /// Component name for structured logging
    pub component: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            with_timestamps: true,
            with_thread_names: true,
            with_file_line: false,
            with_spans: false,
            component: "segwire".to_string(),
        }
    }
}

/// Initialize console logging with the given configuration
pub fn init_logging(config: LogConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("{}={}", config.component, config.level)))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_thread_names(config.with_thread_names)
        .with_file(config.with_file_line)
        .with_line_number(config.with_file_line)
        .with_span_events(if config.with_spans {
            FmtSpan::ENTER | FmtSpan::EXIT
        } else {
            FmtSpan::NONE
        });

    Registry::default().with(filter).with(fmt_layer).init();

    Ok(())
}

/// Structured logging context for operations
#[derive(Debug, Clone)]
pub struct LogContext {
    /// Operation being performed
    pub operation: String,
    /// Namespace name (if applicable)
    pub namespace: Option<String>,
    /// Configuration file path (if applicable)
    pub config_path: Option<PathBuf>,
    /// User ID (if applicable)
    pub user_id: Option<u32>,
    /// Additional context fields
    pub fields: std::collections::HashMap<String, String>,
}

impl LogContext {
    /// Create a new log context for an operation
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            namespace: None,
            config_path: None,
            user_id: None,
            fields: std::collections::HashMap::new(),
        }
    }

    /// Add namespace context
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    /// Add configuration file context
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Add user ID context
    pub fn with_user_id(mut self, user_id: u32) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Add a custom field
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Log an info message with context
    pub fn info(&self, message: &str) {
        let span = tracing::info_span!(
            "operation",
            operation = %self.operation,
            namespace = self.namespace.as_deref(),
            config_path = self.config_path.as_ref().map(|p| p.display().to_string()).as_deref(),
            user_id = self.user_id,
        );
        let _enter = span.enter();

        tracing::info!("{}", message);

        for (key, value) in &self.fields {
            tracing::info!("{}: {}", key, value);
        }
    }

    /// Log a warning message with context
    pub fn warn(&self, message: &str) {
        let span = tracing::warn_span!(
            "operation",
            operation = %self.operation,
            namespace = self.namespace.as_deref(),
            config_path = self.config_path.as_ref().map(|p| p.display().to_string()).as_deref(),
            user_id = self.user_id,
        );
        let _enter = span.enter();

        tracing::warn!("{}", message);

        for (key, value) in &self.fields {
            tracing::warn!("{}: {}", key, value);
        }
    }

    /// Log an error message with context
    pub fn error(&self, message: &str) {
        let span = tracing::error_span!(
            "operation",
            operation = %self.operation,
            namespace = self.namespace.as_deref(),
            config_path = self.config_path.as_ref().map(|p| p.display().to_string()).as_deref(),
            user_id = self.user_id,
        );
        let _enter = span.enter();

        tracing::error!("{}", message);

        for (key, value) in &self.fields {
            tracing::error!("{}: {}", key, value);
        }
    }

    /// Log a debug message with context
    pub fn debug(&self, message: &str) {
        let span = tracing::debug_span!(
            "operation",
            operation = %self.operation,
            namespace = self.namespace.as_deref(),
            config_path = self.config_path.as_ref().map(|p| p.display().to_string()).as_deref(),
            user_id = self.user_id,
        );
        let _enter = span.enter();

        tracing::debug!("{}", message);

        for (key, value) in &self.fields {
            tracing::debug!("{}: {}", key, value);
        }
    }
}

/// Macro for creating structured error reports
#[macro_export]
macro_rules! log_error {
    ($ctx:expr, $err:expr, $msg:expr) => {
        $ctx.error(&format!("{}: {} ({})", $msg, $err, $err.user_message()));
    };
    ($ctx:expr, $err:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.error(&format!("{}: {} ({})", format!($msg, $($arg)*), $err, $err.user_message()));
    };
}

/// Macro for creating structured warning reports
#[macro_export]
macro_rules! log_warn {
    ($ctx:expr, $msg:expr) => {
        $ctx.warn($msg);
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.warn(&format!($msg, $($arg)*));
    };
}

/// Macro for creating structured info reports
#[macro_export]
macro_rules! log_info {
    ($ctx:expr, $msg:expr) => {
        $ctx.info($msg);
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.info(&format!($msg, $($arg)*));
    };
}

/// Macro for creating structured debug reports
#[macro_export]
macro_rules! log_debug {
    ($ctx:expr, $msg:expr) => {
        $ctx.debug($msg);
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.debug(&format!($msg, $($arg)*));
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_from_str() {
        assert_eq!("info".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("debug".parse::<LogLevel>().unwrap(), LogLevel::Debug);
        assert_eq!("error".parse::<LogLevel>().unwrap(), LogLevel::Error);
        assert!("invalid".parse::<LogLevel>().is_err());
    }

    #[test]
    fn test_log_context_creation() {
        let ctx = LogContext::new("test_operation")
            .with_namespace("test-ns")
            .with_user_id(1000)
            .with_field("custom", "value");

        assert_eq!(ctx.operation, "test_operation");
        assert_eq!(ctx.namespace, Some("test-ns".to_string()));
        assert_eq!(ctx.user_id, Some(1000));
        assert_eq!(ctx.fields.get("custom"), Some(&"value".to_string()));
    }
}
