//! Netlink interface wrapper for network namespace operations
//! 
//! This module provides a high-level interface for managing Linux network namespaces
//! using netlink sockets. It handles namespace creation, deletion, and provides
//! error handling for netlink operations.

use crate::error::{SegwireError, SegwireResult};
use nix::unistd::Uid;
use rtnetlink::{new_connection, Handle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

/// Errors specific to netlink operations
#[derive(Debug, Error)]
pub enum NetlinkError {
    #[error("Failed to create netlink connection: {0}")]
    ConnectionFailed(#[from] rtnetlink::Error),
    
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
    /// The namespace ID
    pub id: u32,
    /// Path to the namespace file in /proc/self/ns/net
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

/// High-level interface for netlink namespace operations
pub struct NetlinkManager {
    handle: Handle,
}

impl NetlinkManager {
    /// Create a new NetlinkManager instance
    /// 
    /// This establishes a connection to the netlink socket for namespace operations.
    /// Requires CAP_SYS_ADMIN capability.
    pub async fn new() -> SegwireResult<Self> {
        // Check if we have the necessary privileges
        if !Uid::effective().is_root() {
            return Err(NetlinkError::InsufficientPrivileges.into());
        }

        let (connection, handle, _) = new_connection()
            .map_err(|e| NetlinkError::CreateFailed("connection".to_string(), e.to_string()))?;
        
        // Spawn the connection to handle netlink messages
        monoio::spawn(connection);
        
        Ok(Self { handle })
    }

    /// Create a new network namespace
    /// 
    /// # Arguments
    /// * `name` - The name of the namespace to create
    /// 
    /// # Returns
    /// * `Ok(NamespaceInfo)` - Information about the created namespace
    /// * `Err(SegwireError)` - If the namespace creation fails
    pub async fn create_namespace(&self, name: &str) -> SegwireResult<NamespaceInfo> {
        // Validate namespace name
        self.validate_namespace_name(name)?;

        // Check if namespace already exists
        if self.namespace_exists(name).await? {
            return Err(NetlinkError::NamespaceExists(name.to_string()).into());
        }

        // Create the namespace using ip netns command
        let output = Command::new("ip")
            .args(&["netns", "add", name])
            .output()
            .map_err(|e| NetlinkError::CreateFailed(name.to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::CreateFailed(name.to_string(), error_msg.to_string()).into());
        }

        // Get namespace information
        let info = NamespaceInfo {
            name: name.to_string(),
            id: 0, // We'll get the actual ID later if needed
            path: PathBuf::from(format!("/var/run/netns/{}", name)),
            active: true,
        };

        Ok(info)
    }

    /// Delete a network namespace
    /// 
    /// # Arguments
    /// * `name` - The name of the namespace to delete
    /// 
    /// # Returns
    /// * `Ok(())` - If the namespace was successfully deleted
    /// * `Err(SegwireError)` - If the namespace deletion fails
    pub async fn delete_namespace(&self, name: &str) -> SegwireResult<()> {
        // Validate namespace name
        self.validate_namespace_name(name)?;

        // Check if namespace exists
        if !self.namespace_exists(name).await? {
            return Err(NetlinkError::NamespaceNotFound(name.to_string()).into());
        }

        // Delete the namespace using ip netns command
        let output = Command::new("ip")
            .args(&["netns", "delete", name])
            .output()
            .map_err(|e| NetlinkError::DeleteFailed(name.to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::DeleteFailed(name.to_string(), error_msg.to_string()).into());
        }

        Ok(())
    }

    /// Move a network interface to a namespace
    /// 
    /// # Arguments
    /// * `interface_name` - The name of the interface to move
    /// * `namespace_name` - The name of the target namespace
    /// 
    /// # Returns
    /// * `Ok(())` - If the interface was successfully moved
    /// * `Err(SegwireError)` - If the interface move fails
    pub async fn move_interface_to_namespace(
        &self,
        interface_name: &str,
        namespace_name: &str,
    ) -> SegwireResult<()> {
        // Validate inputs
        self.validate_namespace_name(namespace_name)?;
        self.validate_interface_name(interface_name)?;

        // Check if interface exists and is available
        if !self.interface_exists(interface_name).await? {
            return Err(NetlinkError::InterfaceNotFound(interface_name.to_string()).into());
        }

        if !self.interface_available(interface_name).await? {
            return Err(NetlinkError::InterfaceNotAvailable(interface_name.to_string()).into());
        }

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Move interface to namespace using ip link command
        let output = Command::new("ip")
            .args(&["link", "set", interface_name, "netns", namespace_name])
            .output()
            .map_err(|e| {
                NetlinkError::InterfaceMoveFailed(
                    interface_name.to_string(),
                    namespace_name.to_string(),
                    e.to_string(),
                )
            })?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::InterfaceMoveFailed(
                interface_name.to_string(),
                namespace_name.to_string(),
                error_msg.to_string(),
            )
            .into());
        }

        Ok(())
    }

    /// Create a virtual ethernet (veth) pair
    /// 
    /// # Arguments
    /// * `veth_name` - The name of the first veth interface
    /// * `peer_name` - The name of the peer veth interface
    /// 
    /// # Returns
    /// * `Ok(())` - If the veth pair was successfully created
    /// * `Err(SegwireError)` - If the veth pair creation fails
    pub async fn create_veth_pair(&self, veth_name: &str, peer_name: &str) -> SegwireResult<()> {
        // Validate interface names
        self.validate_interface_name(veth_name)?;
        self.validate_interface_name(peer_name)?;

        // Check if interfaces already exist
        if self.interface_exists(veth_name).await? {
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                veth_name.to_string(),
                "Interface already exists".to_string(),
            )
            .into());
        }

        if self.interface_exists(peer_name).await? {
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                peer_name.to_string(),
                "Peer interface already exists".to_string(),
            )
            .into());
        }

        // Create veth pair using ip link command
        let output = Command::new("ip")
            .args(&[
                "link",
                "add",
                veth_name,
                "type",
                "veth",
                "peer",
                "name",
                peer_name,
            ])
            .output()
            .map_err(|e| {
                NetlinkError::VirtualInterfaceCreateFailed(veth_name.to_string(), e.to_string())
            })?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                veth_name.to_string(),
                error_msg.to_string(),
            )
            .into());
        }

        Ok(())
    }

    /// Check if a network interface exists
    /// 
    /// # Arguments
    /// * `interface_name` - The name of the interface to check
    /// 
    /// # Returns
    /// * `Ok(bool)` - True if the interface exists, false otherwise
    /// * `Err(SegwireError)` - If the check fails
    pub async fn interface_exists(&self, interface_name: &str) -> SegwireResult<bool> {
        let output = Command::new("ip")
            .args(&["link", "show", interface_name])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("interface_check".to_string(), e.to_string()))?;

        Ok(output.status.success())
    }

    /// Check if a network interface is available for namespace assignment
    /// 
    /// An interface is considered available if:
    /// - It exists
    /// - It's not a loopback interface
    /// - It's not already assigned to a namespace (other than the default)
    /// 
    /// # Arguments
    /// * `interface_name` - The name of the interface to check
    /// 
    /// # Returns
    /// * `Ok(bool)` - True if the interface is available, false otherwise
    /// * `Err(SegwireError)` - If the check fails
    pub async fn interface_available(&self, interface_name: &str) -> SegwireResult<bool> {
        // Check if interface exists
        if !self.interface_exists(interface_name).await? {
            return Ok(false);
        }

        // Skip loopback interface
        if interface_name == "lo" {
            return Ok(false);
        }

        // Get interface details to check if it's in the default namespace
        let output = Command::new("ip")
            .args(&["link", "show", interface_name])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("interface_check".to_string(), e.to_string()))?;

        if !output.status.success() {
            return Ok(false);
        }

        // If we can see the interface with ip link show, it's in the default namespace
        // and available for assignment
        Ok(true)
    }

    /// List all network interfaces in the default namespace
    /// 
    /// # Returns
    /// * `Ok(Vec<String>)` - List of interface names
    /// * `Err(SegwireError)` - If listing fails
    pub async fn list_interfaces(&self) -> SegwireResult<Vec<String>> {
        let output = Command::new("ip")
            .args(&["link", "show"])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("interface_list".to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::CreateFailed("interface_list".to_string(), error_msg.to_string()).into());
        }

        let mut interfaces = Vec::new();
        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            // Parse lines like: "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc pfifo_fast state UP mode DEFAULT group default qlen 1000"
            if let Some(interface_part) = line.split(':').nth(1) {
                let interface_name = interface_part.trim().split_whitespace().next();
                if let Some(name) = interface_name {
                    if !name.is_empty() && name != "lo" {
                        interfaces.push(name.to_string());
                    }
                }
            }
        }

        Ok(interfaces)
    }

    /// List network interfaces in a specific namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// 
    /// # Returns
    /// * `Ok(Vec<String>)` - List of interface names in the namespace
    /// * `Err(SegwireError)` - If listing fails
    pub async fn list_namespace_interfaces(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        // Validate namespace name
        self.validate_namespace_name(namespace_name)?;

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // List interfaces in the namespace using ip netns exec
        let output = Command::new("ip")
            .args(&["netns", "exec", namespace_name, "ip", "link", "show"])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("namespace_interface_list".to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::CreateFailed("namespace_interface_list".to_string(), error_msg.to_string()).into());
        }

        let mut interfaces = Vec::new();
        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            // Parse lines like: "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc pfifo_fast state UP mode DEFAULT group default qlen 1000"
            if let Some(interface_part) = line.split(':').nth(1) {
                let interface_name = interface_part.trim().split_whitespace().next();
                if let Some(name) = interface_name {
                    if !name.is_empty() {
                        interfaces.push(name.to_string());
                    }
                }
            }
        }

        Ok(interfaces)
    }

    /// Configure routing in a namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// * `routes` - Vector of route configurations to apply
    /// 
    /// # Returns
    /// * `Ok(())` - If routes were successfully configured
    /// * `Err(SegwireError)` - If route configuration fails
    pub async fn configure_namespace_routes(
        &self,
        namespace_name: &str,
        routes: &[RouteConfig],
    ) -> SegwireResult<()> {
        // Validate namespace name
        self.validate_namespace_name(namespace_name)?;

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Configure each route
        for route in routes {
            self.add_route_to_namespace(namespace_name, route).await?;
        }

        Ok(())
    }

    /// Add a single route to a namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// * `route` - The route configuration to add
    /// 
    /// # Returns
    /// * `Ok(())` - If the route was successfully added
    /// * `Err(SegwireError)` - If route addition fails
    pub async fn add_route_to_namespace(
        &self,
        namespace_name: &str,
        route: &RouteConfig,
    ) -> SegwireResult<()> {
        // Validate route configuration
        self.validate_route_config(route)?;

        // Build ip route command
        let mut args = vec!["netns", "exec", namespace_name, "ip", "route", "add"];
        
        // Add destination
        args.push(&route.destination);
        
        // Add gateway if specified
        if !route.gateway.is_empty() {
            args.push("via");
            args.push(&route.gateway);
        }
        
        // Add interface if specified
        if let Some(ref interface) = route.interface {
            args.push("dev");
            args.push(interface);
        }
        
        // Add metric if specified
        let metric_str;
        if let Some(metric) = route.metric {
            args.push("metric");
            metric_str = metric.to_string();
            args.push(&metric_str);
        }

        // Execute the command
        let output = Command::new("ip")
            .args(&args)
            .output()
            .map_err(|e| {
                NetlinkError::RouteConfigFailed(namespace_name.to_string(), e.to_string())
            })?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::RouteConfigFailed(
                namespace_name.to_string(),
                error_msg.to_string(),
            )
            .into());
        }

        Ok(())
    }

    /// Configure DNS resolution in a namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// * `dns_config` - The DNS configuration to apply
    /// 
    /// # Returns
    /// * `Ok(())` - If DNS was successfully configured
    /// * `Err(SegwireError)` - If DNS configuration fails
    pub async fn configure_namespace_dns(
        &self,
        namespace_name: &str,
        dns_config: &DnsConfig,
    ) -> SegwireResult<()> {
        // Validate namespace name and DNS config
        self.validate_namespace_name(namespace_name)?;
        self.validate_dns_config(dns_config)?;

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Create resolv.conf content
        let mut resolv_content = String::new();

        // Add nameservers
        for server in &dns_config.servers {
            resolv_content.push_str(&format!("nameserver {}\n", server));
        }

        // Add search domains
        if !dns_config.search_domains.is_empty() {
            resolv_content.push_str(&format!("search {}\n", dns_config.search_domains.join(" ")));
        }

        // Add options
        for option in &dns_config.options {
            resolv_content.push_str(&format!("options {}\n", option));
        }

        // Write resolv.conf to the namespace
        // First, create the directory structure in the namespace
        let mkdir_output = Command::new("ip")
            .args(&[
                "netns",
                "exec",
                namespace_name,
                "mkdir",
                "-p",
                "/etc",
            ])
            .output()
            .map_err(|e| {
                NetlinkError::DnsConfigFailed(namespace_name.to_string(), e.to_string())
            })?;

        if !mkdir_output.status.success() {
            let error_msg = String::from_utf8_lossy(&mkdir_output.stderr);
            return Err(NetlinkError::DnsConfigFailed(
                namespace_name.to_string(),
                format!("Failed to create /etc directory: {}", error_msg),
            )
            .into());
        }

        // Write the resolv.conf file
        let echo_output = Command::new("ip")
            .args(&[
                "netns",
                "exec",
                namespace_name,
                "sh",
                "-c",
                &format!("echo '{}' > /etc/resolv.conf", resolv_content.trim()),
            ])
            .output()
            .map_err(|e| {
                NetlinkError::DnsConfigFailed(namespace_name.to_string(), e.to_string())
            })?;

        if !echo_output.status.success() {
            let error_msg = String::from_utf8_lossy(&echo_output.stderr);
            return Err(NetlinkError::DnsConfigFailed(
                namespace_name.to_string(),
                format!("Failed to write resolv.conf: {}", error_msg),
            )
            .into());
        }

        Ok(())
    }

    /// List routes in a namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// 
    /// # Returns
    /// * `Ok(Vec<String>)` - List of route entries
    /// * `Err(SegwireError)` - If listing fails
    pub async fn list_namespace_routes(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        // Validate namespace name
        self.validate_namespace_name(namespace_name)?;

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // List routes in the namespace
        let output = Command::new("ip")
            .args(&["netns", "exec", namespace_name, "ip", "route", "show"])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("route_list".to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::CreateFailed("route_list".to_string(), error_msg.to_string()).into());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let routes: Vec<String> = output_str
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .collect();

        Ok(routes)
    }

    /// Get DNS configuration from a namespace
    /// 
    /// # Arguments
    /// * `namespace_name` - The name of the namespace
    /// 
    /// # Returns
    /// * `Ok(DnsConfig)` - The current DNS configuration
    /// * `Err(SegwireError)` - If reading DNS config fails
    pub async fn get_namespace_dns_config(&self, namespace_name: &str) -> SegwireResult<DnsConfig> {
        // Validate namespace name
        self.validate_namespace_name(namespace_name)?;

        // Check if namespace exists
        if !self.namespace_exists(namespace_name).await? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Read resolv.conf from the namespace
        let output = Command::new("ip")
            .args(&[
                "netns",
                "exec",
                namespace_name,
                "cat",
                "/etc/resolv.conf",
            ])
            .output()
            .map_err(|e| {
                NetlinkError::DnsConfigFailed(namespace_name.to_string(), e.to_string())
            })?;

        let mut dns_config = DnsConfig {
            servers: Vec::new(),
            search_domains: Vec::new(),
            options: Vec::new(),
        };

        if output.status.success() {
            let content = String::from_utf8_lossy(&output.stdout);
            
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("nameserver ") {
                    if let Some(server) = line.strip_prefix("nameserver ") {
                        dns_config.servers.push(server.trim().to_string());
                    }
                } else if line.starts_with("search ") {
                    if let Some(domains) = line.strip_prefix("search ") {
                        dns_config.search_domains = domains
                            .split_whitespace()
                            .map(|s| s.to_string())
                            .collect();
                    }
                } else if line.starts_with("options ") {
                    if let Some(options) = line.strip_prefix("options ") {
                        dns_config.options.push(options.trim().to_string());
                    }
                }
            }
        }

        Ok(dns_config)
    }

    /// Check if a namespace exists
    /// 
    /// # Arguments
    /// * `name` - The name of the namespace to check
    /// 
    /// # Returns
    /// * `Ok(bool)` - True if the namespace exists, false otherwise
    /// * `Err(SegwireError)` - If the check fails
    pub async fn namespace_exists(&self, name: &str) -> SegwireResult<bool> {
        let namespaces = self.list_namespaces().await?;
        Ok(namespaces.contains_key(name))
    }

    /// List all network namespaces
    /// 
    /// # Returns
    /// * `Ok(HashMap<String, NamespaceInfo>)` - Map of namespace names to their information
    /// * `Err(SegwireError)` - If listing fails
    pub async fn list_namespaces(&self) -> SegwireResult<HashMap<String, NamespaceInfo>> {
        let mut namespaces = HashMap::new();

        // List namespaces using ip netns command
        let output = Command::new("ip")
            .args(&["netns", "list"])
            .output()
            .map_err(|e| NetlinkError::CreateFailed("list".to_string(), e.to_string()))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(NetlinkError::CreateFailed("list".to_string(), error_msg.to_string()).into());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        for line in output_str.lines() {
            let name = line.trim().split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                let info = NamespaceInfo {
                    name: name.clone(),
                    id: 0, // We'll get the actual ID later if needed
                    path: PathBuf::from(format!("/var/run/netns/{}", name)),
                    active: true,
                };
                namespaces.insert(name, info);
            }
        }

        Ok(namespaces)
    }

    /// Get information about a specific namespace
    /// 
    /// # Arguments
    /// * `name` - The name of the namespace
    /// 
    /// # Returns
    /// * `Ok(NamespaceInfo)` - Information about the namespace
    /// * `Err(SegwireError)` - If the namespace is not found or query fails
    pub async fn get_namespace_info(&self, name: &str) -> SegwireResult<NamespaceInfo> {
        let namespaces = self.list_namespaces().await?;
        namespaces
            .get(name)
            .cloned()
            .ok_or_else(|| NetlinkError::NamespaceNotFound(name.to_string()).into())
    }

    /// Validate a namespace name
    /// 
    /// Namespace names must be valid Linux network namespace names:
    /// - 1-15 characters long
    /// - Alphanumeric characters, hyphens, and underscores only
    /// - Must start with a letter or underscore
    fn validate_namespace_name(&self, name: &str) -> SegwireResult<()> {
        if name.is_empty() || name.len() > 15 {
            return Err(NetlinkError::InvalidName(
                format!("Namespace name must be 1-15 characters long, got: '{}'", name)
            ).into());
        }

        if !name.chars().next().unwrap().is_ascii_alphabetic() && !name.starts_with('_') {
            return Err(NetlinkError::InvalidName(
                format!("Namespace name must start with a letter or underscore, got: '{}'", name)
            ).into());
        }

        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(NetlinkError::InvalidName(
                format!("Namespace name can only contain alphanumeric characters, hyphens, and underscores, got: '{}'", name)
            ).into());
        }

        Ok(())
    }

    /// Validate a network interface name
    /// 
    /// Interface names must be valid Linux network interface names:
    /// - 1-15 characters long
    /// - Alphanumeric characters, hyphens, underscores, and dots only
    /// - Must start with a letter or underscore
    fn validate_interface_name(&self, name: &str) -> SegwireResult<()> {
        if name.is_empty() || name.len() > 15 {
            return Err(NetlinkError::InvalidName(
                format!("Interface name must be 1-15 characters long, got: '{}'", name)
            ).into());
        }

        if !name.chars().next().unwrap().is_ascii_alphabetic() && !name.starts_with('_') {
            return Err(NetlinkError::InvalidName(
                format!("Interface name must start with a letter or underscore, got: '{}'", name)
            ).into());
        }

        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return Err(NetlinkError::InvalidName(
                format!("Interface name can only contain alphanumeric characters, hyphens, underscores, and dots, got: '{}'", name)
            ).into());
        }

        Ok(())
    }

    /// Validate route configuration
    /// 
    /// # Arguments
    /// * `route` - The route configuration to validate
    /// 
    /// # Returns
    /// * `Ok(())` - If the route configuration is valid
    /// * `Err(SegwireError)` - If the route configuration is invalid
    fn validate_route_config(&self, route: &RouteConfig) -> SegwireResult<()> {
        // Validate destination
        if route.destination.is_empty() {
            return Err(NetlinkError::InvalidRoute("Destination cannot be empty".to_string()).into());
        }

        // Validate gateway if specified
        if !route.gateway.is_empty() && !self.is_valid_ip(&route.gateway) {
            return Err(NetlinkError::InvalidRoute(
                format!("Invalid gateway IP address: {}", route.gateway)
            ).into());
        }

        // Validate interface name if specified
        if let Some(ref interface) = route.interface {
            self.validate_interface_name(interface)?;
        }

        Ok(())
    }

    /// Validate DNS configuration
    /// 
    /// # Arguments
    /// * `dns_config` - The DNS configuration to validate
    /// 
    /// # Returns
    /// * `Ok(())` - If the DNS configuration is valid
    /// * `Err(SegwireError)` - If the DNS configuration is invalid
    fn validate_dns_config(&self, dns_config: &DnsConfig) -> SegwireResult<()> {
        // Validate DNS servers
        if dns_config.servers.is_empty() {
            return Err(NetlinkError::InvalidDns("At least one DNS server must be specified".to_string()).into());
        }

        for server in &dns_config.servers {
            if !self.is_valid_ip(server) {
                return Err(NetlinkError::InvalidDns(
                    format!("Invalid DNS server IP address: {}", server)
                ).into());
            }
        }

        // Validate search domains
        for domain in &dns_config.search_domains {
            if !self.is_valid_domain(domain) {
                return Err(NetlinkError::InvalidDns(
                    format!("Invalid search domain: {}", domain)
                ).into());
            }
        }

        Ok(())
    }

    /// Check if a string is a valid IP address
    /// 
    /// # Arguments
    /// * `ip` - The IP address string to validate
    /// 
    /// # Returns
    /// * `bool` - True if the IP address is valid, false otherwise
    fn is_valid_ip(&self, ip: &str) -> bool {
        use std::net::IpAddr;
        ip.parse::<IpAddr>().is_ok()
    }

    /// Check if a string is a valid domain name
    /// 
    /// # Arguments
    /// * `domain` - The domain name string to validate
    /// 
    /// # Returns
    /// * `bool` - True if the domain name is valid, false otherwise
    fn is_valid_domain(&self, domain: &str) -> bool {
        // Basic domain validation
        if domain.is_empty() || domain.len() > 253 {
            return false;
        }

        // Check for valid characters and structure
        domain
            .split('.')
            .all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_namespace_name() {
        // Create a mock manager for testing validation
        let (_, handle, _) = rtnetlink::new_connection().unwrap();
        let manager = NetlinkManager { handle };

        // Valid names
        assert!(manager.validate_namespace_name("test").is_ok());
        assert!(manager.validate_namespace_name("test-ns").is_ok());
        assert!(manager.validate_namespace_name("test_ns").is_ok());
        assert!(manager.validate_namespace_name("_private").is_ok());
        assert!(manager.validate_namespace_name("ns123").is_ok());

        // Invalid names
        assert!(manager.validate_namespace_name("").is_err());
        assert!(manager.validate_namespace_name("1invalid").is_err());
        assert!(manager.validate_namespace_name("-invalid").is_err());
        assert!(manager.validate_namespace_name("invalid.name").is_err());
        assert!(manager.validate_namespace_name("toolongnamespace").is_err());
        assert!(manager.validate_namespace_name("invalid space").is_err());
    }
}