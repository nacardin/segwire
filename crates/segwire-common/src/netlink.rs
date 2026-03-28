//! Shared netlink types used across crates.
//!
//! Implementation (`NetlinkManager`, `netlink_raw`, `netns_raw`) lives in
//! `segwire-daemon`.  This module provides only the type definitions and
//! error types so they can be referenced from config, D-Bus, and error
//! handling code without pulling in the full implementation.

use crate::error::{SegwireError, SegwireResult};
use std::path::PathBuf;
use thiserror::Error;

/// Errors specific to netlink operations
#[derive(Debug, Error)]
pub enum NetlinkError {
    #[error("Namespace '{0}' not found")]
    NamespaceNotFound(String),

    #[error("Namespace '{0}' already exists")]
    NamespaceExists(String),

    #[error("Failed to create namespace '{0}': {1}")]
    CreateFailed(String, String),

    #[error("Failed to delete namespace '{0}': {1}")]
    DeleteFailed(String, String),

    #[error("Insufficient privileges for namespace operations")]
    InsufficientPrivileges,

    #[error("Invalid namespace name: {0}")]
    InvalidName(String),

    #[error("Network interface '{0}' not found")]
    InterfaceNotFound(String),

    #[error("Failed to move interface '{0}' to namespace '{1}': {2}")]
    InterfaceMoveFailed(String, String, String),

    #[error("Failed to create virtual interface '{0}': {1}")]
    VirtualInterfaceCreateFailed(String, String),

    #[error("Interface '{0}' is not available for namespace assignment")]
    InterfaceNotAvailable(String),

    #[error("Failed to configure route in namespace '{0}': {1}")]
    RouteConfigFailed(String, String),

    #[error("Failed to configure DNS in namespace '{0}': {1}")]
    DnsConfigFailed(String, String),

    #[error("Invalid route configuration: {0}")]
    InvalidRoute(String),

    #[error("Invalid DNS configuration: {0}")]
    InvalidDns(String),

    #[error("Netlink socket error: {0}")]
    SocketError(String),

    #[error("Netlink protocol error: {0}")]
    ProtocolError(String),
}

impl From<NetlinkError> for SegwireError {
    fn from(err: NetlinkError) -> Self {
        SegwireError::Network(err.to_string())
    }
}

/// Information about a network namespace
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    /// The namespace name
    pub name: String,
    /// The namespace ID (inode number)
    pub id: u32,
    /// Path to the namespace bind-mount in /var/run/netns/
    pub path: PathBuf,
    /// Whether the namespace is currently active
    pub active: bool,
}

/// Route configuration for a namespace
#[derive(Debug, Clone)]
pub struct RouteConfig {
    /// Destination network (e.g., "192.168.1.0/24" or "default")
    pub destination: String,
    /// Gateway IP address
    pub gateway: String,
    /// Network interface to use for this route
    pub interface: Option<String>,
    /// Route metric (priority)
    pub metric: Option<u32>,
}

/// DNS configuration for a namespace
#[derive(Debug, Clone)]
pub struct DnsConfig {
    /// DNS server IP addresses
    pub servers: Vec<String>,
    /// Search domains
    pub search_domains: Vec<String>,
    /// Additional options for resolv.conf
    pub options: Vec<String>,
}

impl RouteConfig {
    pub fn validate(&self) -> SegwireResult<()> {
        if self.destination.is_empty() {
            return Err(
                NetlinkError::InvalidRoute("destination cannot be empty".to_string()).into(),
            );
        }
        if self.destination != "default" {
            crate::utils::validate_cidr(&self.destination)?;
        }
        crate::utils::validate_ip_address(&self.gateway)?;
        if let Some(iface) = &self.interface {
            crate::utils::validate_interface_name(iface)?;
        }
        Ok(())
    }
}

impl DnsConfig {
    pub fn validate(&self) -> SegwireResult<()> {
        if self.servers.is_empty() {
            return Err(
                NetlinkError::InvalidDns("at least one DNS server required".to_string()).into(),
            );
        }
        for server in &self.servers {
            crate::utils::validate_ip_address(server)?;
        }
        for domain in &self.search_domains {
            crate::utils::validate_domain_name(domain)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_namespace_name() {
        // Basic validation via the common utils
        assert!(crate::utils::validate_namespace_name("valid-ns").is_ok());
        assert!(crate::utils::validate_namespace_name("").is_err());
    }

    #[test]
    fn test_dns_config_rendering() {
        let dns = DnsConfig {
            servers: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
            search_domains: vec!["example.com".to_string()],
            options: vec!["ndots:5".to_string()],
        };
        assert!(dns.validate().is_ok());
    }

    #[test]
    fn test_format_route() {
        let route = RouteConfig {
            destination: "192.168.1.0/24".to_string(),
            gateway: "10.0.0.1".to_string(),
            interface: None,
            metric: Some(100),
        };
        assert!(route.validate().is_ok());
    }
}
