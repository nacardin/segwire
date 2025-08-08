//! Logging demonstration for segwire daemon
//! 
//! This example shows how the structured logging system works with different
//! log levels, targets, and context information.

use segwire_common::{
    LogConfig, LogLevel, LogContext, 
    init_logging, log_info, log_warn, log_debug,
    SegwireError,
};
use segwire_common::error::ErrorContext;
use std::path::PathBuf;

#[monoio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize console logging with debug level
    let log_config = LogConfig {
        level: LogLevel::Debug,
        with_timestamps: true,
        with_thread_names: true,
        with_file_line: true,
        with_spans: true,
        component: "logging-demo".to_string(),
    };

    init_logging(log_config).map_err(|e| format!("Failed to initialize logging: {}", e))?;

    println!("=== Segwire Logging Demo ===\n");

    // Demonstrate basic structured logging
    let ctx = LogContext::new("demo_operation")
        .with_namespace("test-namespace")
        .with_config_path(PathBuf::from("/etc/segwire/test.toml"))
        .with_user_id(1000)
        .with_field("operation_id", "demo-001")
        .with_field("component", "logging-demo");

    log_info!(ctx, "Starting logging demonstration");
    log_debug!(ctx, "Debug information: system initialized");
    log_warn!(ctx, "Warning: this is a demonstration warning");

    // Demonstrate error logging with context
    let error = SegwireError::Network("Simulated network error".to_string());
    let error_ctx = ErrorContext::new("network_operation")
        .with_namespace("test-namespace")
        .with_field("interface", "eth0")
        .with_remediation("Check network interface availability")
        .with_remediation("Verify namespace configuration");

    error.log_with_context(&error_ctx);

    // Demonstrate different log contexts
    let config_ctx = LogContext::new("configuration_loading")
        .with_config_path(PathBuf::from("/etc/segwire/daemon.toml"))
        .with_field("config_version", "1.0");

    log_info!(config_ctx, "Configuration loaded successfully");

    let namespace_ctx = LogContext::new("namespace_creation")
        .with_namespace("production-app")
        .with_field("interfaces", "eth1,wlan0")
        .with_field("dns_servers", "8.8.8.8,8.8.4.4");

    log_info!(namespace_ctx, "Creating namespace with network interfaces");

    // Demonstrate D-Bus operation logging
    let dbus_ctx = LogContext::new("dbus_method_call")
        .with_field("method", "CreateNamespace")
        .with_field("sender", "segwire-cli")
        .with_user_id(1000);

    log_info!(dbus_ctx, "Processing D-Bus method call");

    println!("\n=== Logging Demo Complete ===");
    Ok(())
}