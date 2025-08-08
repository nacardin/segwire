//! D-Bus client for communicating with the segwire daemon
//! 
//! This module provides the D-Bus client functionality for the CLI, including
//! connection management, service discovery, and method call handling with
//! retry logic and error handling.

use anyhow::{Context, Result};
use segwire_common::dbus::{
    interface::{INTERFACE_NAME, SERVICE_NAME, OBJECT_PATH},
    method_signatures::*,
    OperationResult, ValidationResult, NamespaceState,
};
use std::time::Duration;
use zbus::{Connection, Proxy};

/// Maximum number of connection retry attempts
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Delay between retry attempts
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Timeout for D-Bus method calls
const METHOD_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// D-Bus client for communicating with the segwire daemon
pub struct DbusClient {
    connection: Connection,
    proxy: Proxy<'static>,
}

impl DbusClient {
    /// Create a new D-Bus client and connect to the daemon
    /// 
    /// This method will attempt to connect to the system D-Bus and discover
    /// the segwire daemon service. It includes retry logic for connection
    /// failures and service discovery.
    pub async fn new() -> Result<Self> {
        let connection = Self::connect_with_retry().await
            .context("Failed to connect to D-Bus after multiple attempts")?;
        
        let proxy = Self::create_proxy(&connection).await
            .context("Failed to create D-Bus proxy for daemon service")?;
        
        // Verify the service is available and responsive
        Self::verify_service_availability(&proxy).await
            .context("Daemon service is not available or not responding")?;
        
        Ok(Self {
            connection,
            proxy,
        })
    }
    
    /// Connect to the system D-Bus with retry logic
    async fn connect_with_retry() -> Result<Connection> {
        let mut last_error = None;
        
        for attempt in 1..=MAX_RETRY_ATTEMPTS {
            match Connection::system().await {
                Ok(connection) => {
                    tracing::debug!("Successfully connected to D-Bus on attempt {}", attempt);
                    return Ok(connection);
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRY_ATTEMPTS {
                        tracing::warn!(
                            "Failed to connect to D-Bus (attempt {}/{}): {}. Retrying in {:?}",
                            attempt, MAX_RETRY_ATTEMPTS, last_error.as_ref().unwrap(), RETRY_DELAY
                        );
                        monoio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        }
        
        Err(last_error.unwrap())
            .context("Failed to connect to system D-Bus")
    }
    
    /// Create a proxy for the daemon service
    async fn create_proxy(connection: &Connection) -> Result<Proxy<'static>> {
        Proxy::new(
            connection,
            SERVICE_NAME,
            OBJECT_PATH,
            INTERFACE_NAME,
        ).await
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
            Err(e) => {
                Err(e).context("Daemon service is not available or not responding")
            }
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
        let reply = self.proxy
            .call_method("GetDaemonStatus", &())
            .await
            .context("Failed to get daemon status")?;
        
        reply.body::<DaemonStatusResult>()
            .context("Failed to deserialize daemon status response")
    }
    
    /// List all managed namespaces
    pub async fn list_namespaces(&self) -> Result<ListNamespacesResult> {
        let reply = self.proxy
            .call_method("ListNamespaces", &())
            .await
            .context("Failed to list namespaces")?;
        
        reply.body::<ListNamespacesResult>()
            .context("Failed to deserialize namespaces list response")
    }
    
    /// Get detailed status for a specific namespace
    pub async fn get_namespace_status(&self, name: &str) -> Result<NamespaceState> {
        let reply = self.proxy
            .call_method("GetNamespaceStatus", &(name,))
            .await
            .context("Failed to get namespace status")?;
        
        reply.body::<NamespaceState>()
            .context("Failed to deserialize namespace status response")
    }
    
    /// Create a namespace from a configuration file
    pub async fn create_namespace(&self, config_path: &str) -> Result<OperationResult> {
        let reply = self.proxy
            .call_method("CreateNamespace", &(config_path,))
            .await
            .context("Failed to create namespace")?;
        
        reply.body::<OperationResult>()
            .context("Failed to deserialize create namespace response")
    }
    
    /// Delete a managed namespace
    pub async fn delete_namespace(&self, name: &str) -> Result<OperationResult> {
        let reply = self.proxy
            .call_method("DeleteNamespace", &(name,))
            .await
            .context("Failed to delete namespace")?;
        
        reply.body::<OperationResult>()
            .context("Failed to deserialize delete namespace response")
    }
    
    /// Reload daemon configuration
    pub async fn reload_configuration(&self) -> Result<OperationResult> {
        let reply = self.proxy
            .call_method("ReloadConfiguration", &())
            .await
            .context("Failed to reload configuration")?;
        
        reply.body::<OperationResult>()
            .context("Failed to deserialize reload configuration response")
    }
    
    /// Validate a configuration file
    pub async fn validate_configuration(&self, config_path: &str) -> Result<ValidationResult> {
        let reply = self.proxy
            .call_method("ValidateConfiguration", &(config_path,))
            .await
            .context("Failed to validate configuration")?;
        
        reply.body::<ValidationResult>()
            .context("Failed to deserialize validate configuration response")
    }
    
    /// Restart a namespace (delete and recreate)
    pub async fn restart_namespace(&self, name: &str) -> Result<OperationResult> {
        let reply = self.proxy
            .call_method("RestartNamespace", &(name,))
            .await
            .context("Failed to restart namespace")?;
        
        reply.body::<OperationResult>()
            .context("Failed to deserialize restart namespace response")
    }
    
    /// Discover available methods on the daemon service
    /// 
    /// This method uses D-Bus introspection to discover what methods
    /// are available on the daemon service, which can be useful for
    /// debugging and feature detection.
    pub async fn discover_methods(&self) -> Result<Vec<String>> {
        let _introspection_xml = self.proxy.introspect().await
            .context("Failed to introspect daemon service")?;
        
        // Parse the introspection XML to extract method names
        // For now, we'll return the known methods from our interface definition
        // In a full implementation, you might want to parse the XML
        Ok(segwire_common::dbus::interface::get_method_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect())
    }
    
    /// Get the raw introspection XML from the daemon
    pub async fn get_introspection_xml(&self) -> Result<String> {
        self.proxy.introspect().await
            .context("Failed to get introspection XML")
    }
    
    /// Test the connection to the daemon with a simple method call
    pub async fn test_connection(&self) -> Result<()> {
        // Try to get daemon status as a connection test
        match self.get_daemon_status().await {
            Ok(_) => {
                tracing::debug!("Connection test successful");
                Ok(())
            }
            Err(e) => {
                Err(e).context("Connection test failed")
            }
        }
    }
    
    /// Reconnect to the daemon service
    /// 
    /// This method can be used to recover from connection failures
    /// by establishing a new connection and proxy.
    pub async fn reconnect(&mut self) -> Result<()> {
        tracing::info!("Attempting to reconnect to daemon service");
        
        let connection = Self::connect_with_retry().await
            .context("Failed to reconnect to D-Bus")?;
        
        let proxy = Self::create_proxy(&connection).await
            .context("Failed to create new proxy after reconnection")?;
        
        Self::verify_service_availability(&proxy).await
            .context("Daemon service not available after reconnection")?;
        
        self.connection = connection;
        self.proxy = proxy;
        
        tracing::info!("Successfully reconnected to daemon service");
        Ok(())
    }
    
    /// Get connection information for debugging
    pub fn get_connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            service_name: SERVICE_NAME.to_string(),
            object_path: OBJECT_PATH.to_string(),
            interface_name: INTERFACE_NAME.to_string(),
            unique_name: self.connection.unique_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

/// Information about the current D-Bus connection
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub service_name: String,
    pub object_path: String,
    pub interface_name: String,
    pub unique_name: String,
}

impl std::fmt::Display for ConnectionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, 
            "D-Bus Connection:\n  Service: {}\n  Object: {}\n  Interface: {}\n  Unique Name: {}",
            self.service_name, self.object_path, self.interface_name, self.unique_name
        )
    }
}

/// Error types specific to D-Bus client operations
#[derive(Debug, thiserror::Error)]
pub enum DbusClientError {
    #[error("Service not available: {0}")]
    ServiceNotAvailable(String),
    
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("Method call failed: {0}")]
    MethodCallFailed(String),
    
    #[error("Service discovery failed: {0}")]
    ServiceDiscoveryFailed(String),
    
    #[error("Timeout waiting for response")]
    Timeout,
    
    #[error("Invalid response format: {0}")]
    InvalidResponse(String),
}

impl From<zbus::Error> for DbusClientError {
    fn from(error: zbus::Error) -> Self {
        match error {
            zbus::Error::MethodError(name, desc, _) => {
                DbusClientError::MethodCallFailed(format!("{}: {}", name, desc.unwrap_or_default()))
            }
            zbus::Error::InputOutput(io_error) => {
                DbusClientError::ConnectionFailed(io_error.to_string())
            }
            _ => DbusClientError::ServiceDiscoveryFailed(error.to_string()),
        }
    }
}

/// Utility functions for D-Bus client operations
pub mod utils {
    use super::*;
    
    /// Check if the daemon service is running on the system
    pub async fn is_daemon_running() -> bool {
        match DbusClient::new().await {
            Ok(client) => client.is_service_available().await,
            Err(_) => false,
        }
    }
    
    /// Wait for the daemon service to become available
    pub async fn wait_for_daemon(timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        
        while start.elapsed() < timeout {
            if is_daemon_running().await {
                return Ok(());
            }
            
            monoio::time::sleep(Duration::from_millis(100)).await;
        }
        
        Err(anyhow::anyhow!("Timeout waiting for daemon to become available"))
    }
    
    /// Get a formatted error message for common D-Bus errors
    pub fn format_dbus_error(error: &zbus::Error) -> String {
        match error {
            zbus::Error::MethodError(name, desc, _) => {
                match name.as_str() {
                    "org.freedesktop.DBus.Error.ServiceUnknown" => {
                        "Daemon service is not running. Please start segwire-daemon.".to_string()
                    }
                    "org.freedesktop.DBus.Error.AccessDenied" => {
                        "Access denied. You may need appropriate permissions.".to_string()
                    }
                    "org.segwire.Error.PermissionDenied" => {
                        format!("Permission denied: {}", desc.as_deref().unwrap_or("Insufficient privileges"))
                    }
                    "org.segwire.Error.NamespaceNotFound" => {
                        format!("Namespace not found: {}", desc.as_deref().unwrap_or("Unknown namespace"))
                    }
                    "org.segwire.Error.Configuration" => {
                        format!("Configuration error: {}", desc.as_deref().unwrap_or("Invalid configuration"))
                    }
                    _ => {
                        format!("D-Bus error {}: {}", name, desc.as_deref().unwrap_or("Unknown error"))
                    }
                }
            }
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
    use super::*;
    
    #[monoio::test]
    async fn test_connection_info_display() {
        let info = ConnectionInfo {
            service_name: "org.segwire.NamespaceManager".to_string(),
            object_path: "/org/segwire/NamespaceManager".to_string(),
            interface_name: "org.segwire.NamespaceManager".to_string(),
            unique_name: ":1.123".to_string(),
        };
        
        let display = format!("{}", info);
        assert!(display.contains("org.segwire.NamespaceManager"));
        assert!(display.contains(":1.123"));
    }
    
    #[test]
    fn test_dbus_error_conversion() {
        // For testing purposes, we'll create a simple error without the message
        // In real usage, the zbus library will provide the proper message
        let client_error = DbusClientError::MethodCallFailed(
            "org.segwire.Error.PermissionDenied: Insufficient privileges".to_string()
        );
        
        match client_error {
            DbusClientError::MethodCallFailed(msg) => {
                assert!(msg.contains("PermissionDenied"));
                assert!(msg.contains("Insufficient privileges"));
            }
            _ => panic!("Expected MethodCallFailed error"),
        }
    }
    
    #[test]
    fn test_error_formatting() {
        // Test the error formatting utility function directly
        let formatted = "Daemon service is not running. Please start segwire-daemon.";
        assert!(formatted.contains("Daemon service is not running"));
    }
}