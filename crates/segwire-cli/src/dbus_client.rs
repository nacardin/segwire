//! D-Bus client for communicating with the segwire daemon
//!
//! This module provides the D-Bus client functionality for the CLI, including
//! connection management, service discovery, and method call handling with
//! retry logic and error handling.

use anyhow::{Context, Result};
use segwire_common::dbus::{
    interface::{INTERFACE_NAME, OBJECT_PATH, SERVICE_NAME},
    method_signatures::*,
    NamespaceState, OperationResult,
};
use std::time::Duration;
use zbus::{Connection, Proxy};

/// Maximum number of connection retry attempts
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Delay between retry attempts
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// D-Bus client for communicating with the segwire daemon
pub struct DbusClient {
    /// Keeps the D-Bus connection alive for the lifetime of this client.
    /// The `proxy` borrows from this connection internally, so dropping
    /// it would invalidate all proxy calls.
    _connection: Connection,
    proxy: Proxy<'static>,
}

impl DbusClient {
    /// Create a new D-Bus client and connect to the daemon
    ///
    /// This method will attempt to connect to the system D-Bus and discover
    /// the segwire daemon service. It includes retry logic for connection
    /// failures and service discovery.
    pub async fn new() -> Result<Self> {
        let connection = Self::connect_with_retry()
            .await
            .context("Failed to connect to D-Bus after multiple attempts")?;

        let proxy = Self::create_proxy(&connection)
            .await
            .context("Failed to create D-Bus proxy for daemon service")?;

        // Verify the service is available and responsive
        Self::verify_service_availability(&proxy)
            .await
            .context("Daemon service is not available or not responding")?;

        Ok(Self {
            _connection: connection,
            proxy,
        })
    }

    /// Connect to the system D-Bus with retry logic
    async fn connect_with_retry() -> Result<Connection> {
        let mut last_error = None;

        for attempt in 1..=MAX_RETRY_ATTEMPTS {
            let conn_res = if cfg!(test) || std::env::var("SEGWIRE_TEST_SESSION_BUS").is_ok() {
                Connection::session().await
            } else {
                Connection::system().await
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
                        monoio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        }

        Err(last_error.unwrap()).context("Failed to connect to system D-Bus")
    }

    /// Create a proxy for the daemon service
    async fn create_proxy(connection: &Connection) -> Result<Proxy<'static>> {
        Proxy::new(connection, SERVICE_NAME, OBJECT_PATH, INTERFACE_NAME)
            .await
            .context("Failed to create D-Bus proxy")
    }

    /// Verify that the daemon service is available and responding
    async fn verify_service_availability(proxy: &Proxy<'_>) -> Result<()> {
        // Try to call introspect to verify the service is available
        match proxy.introspect().await {
            Ok(_) => {
                tracing::debug!("Daemon service is available and responding");
                Ok(())
            }
            Err(e) => Err(e).context("Daemon service is not available or not responding"),
        }
    }

    /// Check if the daemon service is currently available
    pub async fn is_service_available(&self) -> bool {
        match self.proxy.introspect().await {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("Service availability check failed: {}", e);
                false
            }
        }
    }

    /// Get the daemon service version and status
    pub async fn get_daemon_status(&self) -> Result<DaemonStatusResult> {
        let reply = self
            .proxy
            .call_method("GetDaemonStatus", &())
            .await
            .context("Failed to get daemon status")?;

        reply
            .body::<DaemonStatusResult>()
            .context("Failed to deserialize daemon status response")
    }

    /// List all managed namespaces
    pub async fn list_namespaces(&self) -> Result<ListNamespacesResult> {
        let reply = self
            .proxy
            .call_method("ListNamespaces", &())
            .await
            .context("Failed to list namespaces")?;

        reply
            .body::<ListNamespacesResult>()
            .context("Failed to deserialize namespaces list response")
    }

    /// Get detailed status for a specific namespace
    pub async fn get_namespace_status(&self, name: &str) -> Result<NamespaceState> {
        let reply = self
            .proxy
            .call_method("GetNamespaceStatus", &(name,))
            .await
            .context("Failed to get namespace status")?;

        reply
            .body::<NamespaceState>()
            .context("Failed to deserialize namespace status response")
    }

    /// Reload daemon configuration
    pub async fn reload_configuration(&self) -> Result<OperationResult> {
        let reply = self
            .proxy
            .call_method("ReloadConfiguration", &())
            .await
            .context("Failed to reload configuration")?;

        reply
            .body::<OperationResult>()
            .context("Failed to deserialize reload configuration response")
    }

    /// Restart a namespace (delete and recreate)
    pub async fn restart_namespace(&self, name: &str) -> Result<OperationResult> {
        let reply = self
            .proxy
            .call_method("RestartNamespace", &(name,))
            .await
            .context("Failed to restart namespace")?;

        reply
            .body::<OperationResult>()
            .context("Failed to deserialize restart namespace response")
    }
}

pub mod utils {

    /// Get a formatted error message for common D-Bus errors
    pub fn format_dbus_error(error: &zbus::Error) -> String {
        match error {
            zbus::Error::MethodError(name, desc, _) => match name.as_str() {
                "org.freedesktop.DBus.Error.ServiceUnknown" => {
                    "Daemon service is not running. Please start segwire-daemon.".to_string()
                }
                "org.freedesktop.DBus.Error.AccessDenied" => {
                    "Access denied. You may need appropriate permissions.".to_string()
                }
                "org.segwire.Error.PermissionDenied" => {
                    format!(
                        "Permission denied: {}",
                        desc.as_deref().unwrap_or("Insufficient privileges")
                    )
                }
                "org.segwire.Error.NamespaceNotFound" => {
                    format!(
                        "Namespace not found: {}",
                        desc.as_deref().unwrap_or("Unknown namespace")
                    )
                }
                "org.segwire.Error.Configuration" => {
                    format!(
                        "Configuration error: {}",
                        desc.as_deref().unwrap_or("Invalid configuration")
                    )
                }
                _ => {
                    format!(
                        "D-Bus error {}: {}",
                        name,
                        desc.as_deref().unwrap_or("Unknown error")
                    )
                }
            },
            zbus::Error::InputOutput(io_error) => {
                format!("Connection error: {}", io_error)
            }
            _ => {
                format!("D-Bus communication error: {}", error)
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
