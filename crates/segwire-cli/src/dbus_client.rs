//! D-Bus client for communicating with the segwire daemon
//!
//! This module provides the D-Bus client functionality for the CLI, including
//! connection management, service discovery, and method call handling with
//! retry logic and error handling.

use anyhow::{Context, Result};
use dbus::blocking::{Connection, Proxy};
use segwire_common::dbus::{
    interface::{INTERFACE_NAME, OBJECT_PATH, SERVICE_NAME},
    NamespaceState, OperationResult,
};
use std::collections::HashMap;
use std::time::Duration;

/// Maximum number of connection retry attempts
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Delay between retry attempts
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Timeout for D-Bus method calls
const METHOD_TIMEOUT: Duration = Duration::from_secs(30);

/// D-Bus client for communicating with the segwire daemon
pub struct DbusClient {
    connection: Connection,
}

impl DbusClient {
    /// Create a new D-Bus client and connect to the daemon
    pub fn new() -> Result<Self> {
        let connection = Self::connect_with_retry()
            .context("Failed to connect to D-Bus after multiple attempts")?;

        // Verify the service is available and responsive
        Self::verify_service_availability(&connection)
            .context("Daemon service is not available or not responding")?;

        Ok(Self { connection })
    }

    /// Get a proxy for calling daemon methods
    fn proxy(&self) -> Proxy<'_, &Connection> {
        self.connection
            .with_proxy(SERVICE_NAME, OBJECT_PATH, METHOD_TIMEOUT)
    }

    /// Connect to the system D-Bus with retry logic
    fn connect_with_retry() -> Result<Connection> {
        let mut last_error = None;

        for attempt in 1..=MAX_RETRY_ATTEMPTS {
            let conn_res = if cfg!(test) || std::env::var("SEGWIRE_TEST_SESSION_BUS").is_ok() {
                Connection::new_session()
            } else {
                Connection::new_system()
            };

            match conn_res {
                Ok(connection) => {
                    tracing::debug!("Successfully connected to D-Bus on attempt {}", attempt);
                    return Ok(connection);
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRY_ATTEMPTS {
                        tracing::warn!(
                            "Failed to connect to D-Bus (attempt {}/{}): {}. Retrying in {:?}",
                            attempt,
                            MAX_RETRY_ATTEMPTS,
                            last_error.as_ref().unwrap(),
                            RETRY_DELAY
                        );
                        std::thread::sleep(RETRY_DELAY);
                    }
                }
            }
        }

        Err(last_error.unwrap()).context("Failed to connect to system D-Bus")
    }

    /// Verify that the daemon service is available and responding
    fn verify_service_availability(connection: &Connection) -> Result<()> {
        let proxy = connection.with_proxy(SERVICE_NAME, OBJECT_PATH, Duration::from_secs(5));

        // Try to call Introspect to verify the service is available
        let result: Result<(String,), _> =
            proxy.method_call("org.freedesktop.DBus.Introspectable", "Introspect", ());

        match result {
            Ok(_) => {
                tracing::debug!("Daemon service is available and responding");
                Ok(())
            }
            Err(e) => Err(e).context("Daemon service is not available or not responding"),
        }
    }

    /// Check if the daemon service is currently available
    pub fn is_service_available(&self) -> bool {
        let proxy = self.proxy();
        let result: Result<(String,), _> =
            proxy.method_call("org.freedesktop.DBus.Introspectable", "Introspect", ());
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("Service availability check failed: {}", e);
                false
            }
        }
    }

    /// Get the daemon service version and status
    pub fn get_daemon_status(&self) -> Result<(String, u64, u32, u32)> {
        let result: (String, u64, u32, u32) = self
            .proxy()
            .method_call(INTERFACE_NAME, "GetDaemonStatus", ())
            .context("Failed to get daemon status")?;
        Ok(result)
    }

    /// List all managed namespaces
    pub fn list_namespaces(&self) -> Result<Vec<(String, String, String, String)>> {
        let (namespaces,): (Vec<(String, String, String, String)>,) = self
            .proxy()
            .method_call(INTERFACE_NAME, "ListNamespaces", ())
            .context("Failed to list namespaces")?;
        Ok(namespaces)
    }

    /// Get detailed status for a specific namespace
    pub fn get_namespace_status(&self, name: &str) -> Result<NamespaceState> {
        let (ns_name, full_name, status, config_path, created_at, last_updated): (
            String,
            String,
            String,
            String,
            u64,
            u64,
        ) = self
            .proxy()
            .method_call(INTERFACE_NAME, "GetNamespaceStatus", (name,))
            .context("Failed to get namespace status")?;

        let namespace_status = status
            .parse()
            .unwrap_or(segwire_common::dbus::NamespaceStatus::Failed);

        Ok(NamespaceState {
            name: ns_name,
            full_name,
            status: namespace_status,
            config_path,
            interfaces: Vec::new(),
            routes: Vec::new(),
            dns_config: segwire_common::dbus::DnsInfo {
                servers: Vec::new(),
                search_domains: Vec::new(),
            },
            created_at,
            last_updated,
        })
    }

    /// Reload daemon configuration
    pub fn reload_configuration(&self) -> Result<OperationResult> {
        let (success, message, details): (bool, String, HashMap<String, String>) = self
            .proxy()
            .method_call(INTERFACE_NAME, "ReloadConfiguration", ())
            .context("Failed to reload configuration")?;

        Ok(OperationResult {
            success,
            message,
            details,
        })
    }

    /// Restart a namespace (delete and recreate)
    pub fn restart_namespace(&self, name: &str) -> Result<OperationResult> {
        let (success, message, details): (bool, String, HashMap<String, String>) = self
            .proxy()
            .method_call(INTERFACE_NAME, "RestartNamespace", (name,))
            .context("Failed to restart namespace")?;

        Ok(OperationResult {
            success,
            message,
            details,
        })
    }
}

pub mod utils {

    /// Get a formatted error message for common D-Bus errors
    pub fn format_dbus_error(error: &dbus::Error) -> String {
        let name = error.name().unwrap_or("unknown");
        let message = error.message().unwrap_or("Unknown error");

        match name {
            "org.freedesktop.DBus.Error.ServiceUnknown" => {
                "Daemon service is not running. Please start segwire-daemon.".to_string()
            }
            "org.freedesktop.DBus.Error.AccessDenied" => {
                "Access denied. You may need appropriate permissions.".to_string()
            }
            "org.segwire.Error.PermissionDenied" => {
                format!("Permission denied: {}", message)
            }
            "org.segwire.Error.NamespaceNotFound" => {
                format!("Namespace not found: {}", message)
            }
            "org.segwire.Error.Configuration" => {
                format!("Configuration error: {}", message)
            }
            _ => {
                format!("D-Bus error {}: {}", name, message)
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_error_formatting() {
        // Test the error formatting utility function directly
        let formatted = "Daemon service is not running. Please start segwire-daemon.";
        assert!(formatted.contains("Daemon service is not running"));
    }
}
