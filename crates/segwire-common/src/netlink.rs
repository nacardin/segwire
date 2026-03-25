//! Netlink interface wrapper for network namespace operations
//!
//! Provides a high-level interface for managing Linux network namespaces.
//! All kernel interactions (raw netlink sockets, nix syscalls, unsafe code)
//! are delegated to the [`crate::netlink_raw`] module.
//!
//! This module contains **zero** `unsafe` blocks.
//!
//! All operations are synchronous and runtime-agnostic — no tokio dependency.

use crate::error::{SegwireError, SegwireResult};
use crate::netlink_raw::{self, RawNetlinkSocket, RawRouteParams};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{info, warn};

/// Directory where named network namespaces are persisted.
const NETNS_RUN_DIR: &str = "/var/run/netns";

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

// ---------------------------------------------------------------------------
// NetlinkManager
// ---------------------------------------------------------------------------

/// In-memory state for simulation mode.
pub struct SimulatedState {
    namespaces: HashMap<String, NamespaceInfo>,
    next_id: u32,
}

impl SimulatedState {
    fn new() -> Self {
        Self {
            namespaces: HashMap::new(),
            next_id: 1000,
        }
    }
}

/// Backend selection for NetlinkManager.
enum NetlinkBackend {
    /// Real netlink socket for production use.
    Real(RawNetlinkSocket),
    /// In-memory simulation for testing.
    Simulated(std::cell::RefCell<SimulatedState>),
}

/// High-level interface for network namespace operations.
///
/// Uses [`crate::netlink_raw`] for all kernel interactions (raw netlink sockets,
/// nix syscalls).  All methods are synchronous.
///
/// In simulation mode (created via `new_simulated()` or `new_auto()` with
/// `SEGWIRE_SIMULATION=1`), all operations act on an in-memory map instead
/// of real kernel namespaces.
pub struct NetlinkManager {
    backend: NetlinkBackend,
}

impl NetlinkManager {
    /// Create a new NetlinkManager instance.
    ///
    /// Requires CAP_SYS_ADMIN capability (currently checks effective UID == 0).
    pub fn new() -> SegwireResult<Self> {
        if !netlink_raw::is_root() {
            return Err(NetlinkError::InsufficientPrivileges.into());
        }

        let socket = RawNetlinkSocket::open()?;

        // Ensure /var/run/netns exists
        if !Path::new(NETNS_RUN_DIR).exists() {
            fs::create_dir_all(NETNS_RUN_DIR).map_err(|e| {
                NetlinkError::CreateFailed(
                    "netns directory".to_string(),
                    format!("failed to create {}: {}", NETNS_RUN_DIR, e),
                )
            })?;
        }

        Ok(Self {
            backend: NetlinkBackend::Real(socket),
        })
    }

    /// Create a simulated NetlinkManager for testing.
    ///
    /// All namespace operations act on an in-memory map — no root required,
    /// no real kernel namespaces are created.
    pub fn new_simulated() -> SegwireResult<Self> {
        info!("Creating simulated NetlinkManager (no real namespace operations)");
        Ok(Self {
            backend: NetlinkBackend::Simulated(std::cell::RefCell::new(SimulatedState::new())),
        })
    }

    /// Auto-select real or simulated mode based on `SEGWIRE_SIMULATION` env var.
    pub fn new_auto() -> SegwireResult<Self> {
        if std::env::var("SEGWIRE_SIMULATION").is_ok() {
            Self::new_simulated()
        } else {
            Self::new()
        }
    }

    /// Returns true if running in simulation mode.
    pub fn is_simulated(&self) -> bool {
        matches!(self.backend, NetlinkBackend::Simulated(_))
    }

    /// Get reference to the real netlink socket (panics in simulation mode).
    fn real_socket(&self) -> &RawNetlinkSocket {
        match &self.backend {
            NetlinkBackend::Real(s) => s,
            NetlinkBackend::Simulated(_) => panic!("BUG: real_socket() called in simulation mode"),
        }
    }

    // -----------------------------------------------------------------------
    // Namespace lifecycle
    // -----------------------------------------------------------------------

    /// Create a new network namespace.
    ///
    /// This mirrors the behaviour of `ip netns add`:
    ///   1. Create a placeholder file under /var/run/netns/
    ///   2. In a new thread: `unshare(CLONE_NEWNET)`
    ///   3. Bind-mount the thread's `/proc/self/ns/net` onto the placeholder
    ///
    /// In simulation mode, adds an entry to the in-memory map.
    pub fn create_namespace(&self, name: &str) -> SegwireResult<NamespaceInfo> {
        crate::utils::validate_namespace_name(name)?;

        if let NetlinkBackend::Simulated(state) = &self.backend {
            let mut state = state.borrow_mut();
            if state.namespaces.contains_key(name) {
                return Err(NetlinkError::NamespaceExists(name.to_string()).into());
            }
            let id = state.next_id;
            state.next_id += 1;
            let info = NamespaceInfo {
                name: name.to_string(),
                id,
                path: PathBuf::from(format!("/sim/netns/{}", name)),
                active: true,
            };
            state.namespaces.insert(name.to_string(), info.clone());
            info!("[SIM] Created namespace '{}'", name);
            return Ok(info);
        }

        let ns_path = PathBuf::from(format!("{}/{}", NETNS_RUN_DIR, name));
        if ns_path.exists() {
            return Err(NetlinkError::NamespaceExists(name.to_string()).into());
        }

        // Create placeholder file
        fs::File::create(&ns_path).map_err(|e| {
            NetlinkError::CreateFailed(name.to_string(), format!("create file: {}", e))
        })?;

        // Do the unshare + bind-mount in a dedicated thread
        if let Err(msg) = netlink_raw::create_netns(&ns_path) {
            let _ = fs::remove_file(&ns_path);
            return Err(NetlinkError::CreateFailed(name.to_string(), msg).into());
        }

        // Read the inode number as an ID
        let id = netlink_raw::ns_inode(&ns_path);

        info!("Created namespace '{}'", name);
        Ok(NamespaceInfo {
            name: name.to_string(),
            id,
            path: ns_path,
            active: true,
        })
    }

    /// Delete a network namespace.
    ///
    /// Mirrors `ip netns delete`: unmount the bind-mount, remove the file.
    /// In simulation mode, removes from the in-memory map.
    pub fn delete_namespace(&self, name: &str) -> SegwireResult<()> {
        crate::utils::validate_namespace_name(name)?;

        if let NetlinkBackend::Simulated(state) = &self.backend {
            let mut state = state.borrow_mut();
            if state.namespaces.remove(name).is_none() {
                return Err(NetlinkError::NamespaceNotFound(name.to_string()).into());
            }
            info!("[SIM] Deleted namespace '{}'", name);
            return Ok(());
        }

        let ns_path = PathBuf::from(format!("{}/{}", NETNS_RUN_DIR, name));
        if !ns_path.exists() {
            return Err(NetlinkError::NamespaceNotFound(name.to_string()).into());
        }

        netlink_raw::delete_netns(&ns_path)
            .map_err(|e| NetlinkError::DeleteFailed(name.to_string(), e))?;

        info!("Deleted namespace '{}'", name);
        Ok(())
    }

    /// Check if a namespace exists.
    pub fn namespace_exists(&self, name: &str) -> SegwireResult<bool> {
        if let NetlinkBackend::Simulated(state) = &self.backend {
            return Ok(state.borrow().namespaces.contains_key(name));
        }
        Ok(PathBuf::from(format!("{}/{}", NETNS_RUN_DIR, name)).exists())
    }

    /// List all named network namespaces.
    ///
    /// Reads `/var/run/netns/` directory entries.
    /// In simulation mode, returns the in-memory map.
    pub fn list_namespaces(&self) -> SegwireResult<HashMap<String, NamespaceInfo>> {
        if let NetlinkBackend::Simulated(state) = &self.backend {
            return Ok(state.borrow().namespaces.clone());
        }

        let mut map = HashMap::new();
        let netns_dir = Path::new(NETNS_RUN_DIR);
        if !netns_dir.exists() {
            return Ok(map);
        }

        let entries = fs::read_dir(netns_dir).map_err(|e| {
            NetlinkError::CreateFailed("list".to_string(), format!("read_dir failed: {}", e))
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let id = netlink_raw::ns_inode(&path);

            map.insert(
                name.clone(),
                NamespaceInfo {
                    name,
                    id,
                    path,
                    active: true,
                },
            );
        }

        Ok(map)
    }

    // -----------------------------------------------------------------------
    // Link operations
    // -----------------------------------------------------------------------

    /// List all network interfaces in the default namespace.
    pub fn list_interfaces(&self) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(vec!["lo".to_string(), "eth0".to_string()]);
        }

        let names = netlink_raw::dump_interface_names(self.real_socket())?;
        Ok(names)
    }

    /// Check if a network interface exists.
    pub fn interface_exists(&self, interface_name: &str) -> SegwireResult<bool> {
        let interfaces = self.list_interfaces()?;
        Ok(interfaces.iter().any(|n| n == interface_name))
    }

    /// Check if a network interface is available for namespace assignment.
    ///
    /// An interface is available if it exists and is not the loopback.
    pub fn interface_available(&self, interface_name: &str) -> SegwireResult<bool> {
        if interface_name == "lo" {
            return Ok(false);
        }
        self.interface_exists(interface_name)
    }

    /// Get the kernel interface index for a given name.
    fn get_interface_index(&self, interface_name: &str) -> SegwireResult<u32> {
        if self.is_simulated() {
            return Ok(1); // simulated index
        }

        let idx = netlink_raw::get_interface_index(self.real_socket(), interface_name)?;
        Ok(idx)
    }

    /// List network interfaces inside a specific namespace.
    pub fn list_namespace_interfaces(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(vec!["lo".to_string()]);
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let result =
            netlink_raw::run_in_namespace(&ns_path, || netlink_raw::dump_interface_names_fresh())
                .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    /// Move a network interface to a namespace.
    ///
    /// Uses `RTM_SETLINK` with `IFLA_NET_NS_FD`.
    pub fn move_interface_to_namespace(
        &self,
        interface_name: &str,
        namespace_name: &str,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Moved interface '{}' to namespace '{}'",
                interface_name, namespace_name
            );
            return Ok(());
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        crate::utils::validate_interface_name(interface_name)?;

        if !self.interface_exists(interface_name)? {
            return Err(NetlinkError::InterfaceNotFound(interface_name.to_string()).into());
        }
        if !self.interface_available(interface_name)? {
            return Err(NetlinkError::InterfaceNotAvailable(interface_name.to_string()).into());
        }
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ifindex = self.get_interface_index(interface_name)?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        netlink_raw::move_interface_to_ns(self.real_socket(), ifindex, &ns_path).map_err(|e| {
            NetlinkError::InterfaceMoveFailed(
                interface_name.to_string(),
                namespace_name.to_string(),
                e,
            )
        })?;

        info!(
            "Moved interface '{}' to namespace '{}'",
            interface_name, namespace_name
        );
        Ok(())
    }

    /// Move a network interface from a namespace back to the default namespace (PID 1).
    pub fn move_interface_from_namespace_to_default(
        &self,
        namespace_name: &str,
        interface_name: &str,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Moved interface '{}' from namespace '{}' to default",
                interface_name, namespace_name
            );
            return Ok(());
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        crate::utils::validate_interface_name(interface_name)?;

        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Check if interface exists in the namespace
        let ns_interfaces = self.list_namespace_interfaces(namespace_name)?;
        if !ns_interfaces.iter().any(|name| name == interface_name) {
            warn!(
                "Interface '{}' not found in namespace '{}', skipping move",
                interface_name, namespace_name
            );
            return Ok(());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let iface_name = interface_name.to_string();
        let iface_name_for_err = iface_name.clone();

        let result = netlink_raw::run_in_namespace(&ns_path, move || {
            netlink_raw::move_interface_to_default_ns(&iface_name)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(|e| {
            NetlinkError::InterfaceMoveFailed(iface_name_for_err, "default".to_string(), e).into()
        })
    }

    /// Create a virtual ethernet (veth) pair.
    ///
    /// Uses `RTM_NEWLINK` with `IFLA_LINKINFO` kind="veth".
    pub fn create_veth_pair(&self, veth_name: &str, peer_name: &str) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Created veth pair '{}'<->'{}'", veth_name, peer_name);
            return Ok(());
        }
        crate::utils::validate_interface_name(veth_name)?;
        crate::utils::validate_interface_name(peer_name)?;

        if self.interface_exists(veth_name)? {
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                veth_name.to_string(),
                "Interface already exists".to_string(),
            )
            .into());
        }
        if self.interface_exists(peer_name)? {
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                peer_name.to_string(),
                "Peer interface already exists".to_string(),
            )
            .into());
        }

        netlink_raw::create_veth_pair(self.real_socket(), veth_name, peer_name).map_err(|e| {
            NetlinkError::VirtualInterfaceCreateFailed(veth_name.to_string(), e.to_string())
        })?;

        info!("Created veth pair '{}'<->'{}'", veth_name, peer_name);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Route operations
    // -----------------------------------------------------------------------

    /// Configure routing inside a namespace.
    pub fn configure_namespace_routes(
        &self,
        namespace_name: &str,
        routes: &[RouteConfig],
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Configured routes for namespace '{}'", namespace_name);
            return Ok(());
        }
        crate::utils::validate_namespace_name(namespace_name)?;

        for route in routes {
            self.add_route_to_namespace(namespace_name, route)?;
        }
        Ok(())
    }

    /// Add a single route inside a namespace using netlink.
    pub fn add_route_to_namespace(
        &self,
        namespace_name: &str,
        route: &RouteConfig,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            return Ok(());
        }
        route.validate()?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let params = RawRouteParams {
            destination: route.destination.clone(),
            gateway: route.gateway.clone(),
            interface: route.interface.clone(),
            metric: route.metric,
        };

        let result =
            netlink_raw::run_in_namespace(&ns_path, move || netlink_raw::add_route_fresh(params))
                .map_err(SegwireError::Network)?;

        result.map_err(|e| NetlinkError::RouteConfigFailed(namespace_name.to_string(), e).into())
    }

    /// List routes inside a namespace.
    pub fn list_namespace_routes(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(Vec::new());
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let result = netlink_raw::run_in_namespace(&ns_path, || netlink_raw::dump_routes_fresh())
            .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    // -----------------------------------------------------------------------
    // DNS configuration (file I/O only)
    // -----------------------------------------------------------------------

    /// Configure DNS resolution in a namespace.
    ///
    /// Writes a `/etc/resolv.conf` file inside the namespace's mount namespace
    /// via `/etc/netns/<name>/resolv.conf` which iproute2 bind-mounts into the
    /// namespace when using `ip netns exec`.
    pub fn configure_namespace_dns(
        &self,
        namespace_name: &str,
        dns_config: &DnsConfig,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Configured DNS for namespace '{}'", namespace_name);
            return Ok(());
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        dns_config.validate()?;

        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        // Build resolv.conf content
        let mut content = String::new();
        for server in &dns_config.servers {
            content.push_str(&format!("nameserver {}\n", server));
        }
        if !dns_config.search_domains.is_empty() {
            content.push_str(&format!("search {}\n", dns_config.search_domains.join(" ")));
        }
        for opt in &dns_config.options {
            content.push_str(&format!("options {}\n", opt));
        }

        let netns_etc = format!("/etc/netns/{}", namespace_name);
        fs::create_dir_all(&netns_etc).map_err(|e| {
            NetlinkError::DnsConfigFailed(
                namespace_name.to_string(),
                format!("mkdir {}: {}", netns_etc, e),
            )
        })?;

        let resolv_path = format!("{}/resolv.conf", netns_etc);
        let mut f = fs::File::create(&resolv_path).map_err(|e| {
            NetlinkError::DnsConfigFailed(
                namespace_name.to_string(),
                format!("create resolv.conf: {}", e),
            )
        })?;
        f.write_all(content.as_bytes()).map_err(|e| {
            NetlinkError::DnsConfigFailed(
                namespace_name.to_string(),
                format!("write resolv.conf: {}", e),
            )
        })?;

        info!("Configured DNS for namespace '{}'", namespace_name);
        Ok(())
    }

    /// Get DNS configuration from a namespace.
    pub fn get_namespace_dns_config(&self, namespace_name: &str) -> SegwireResult<DnsConfig> {
        if self.is_simulated() {
            return Ok(DnsConfig {
                servers: Vec::new(),
                search_domains: Vec::new(),
                options: Vec::new(),
            });
        }
        crate::utils::validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let resolv_path = format!("/etc/netns/{}/resolv.conf", namespace_name);
        let mut dns = DnsConfig {
            servers: Vec::new(),
            search_domains: Vec::new(),
            options: Vec::new(),
        };

        if let Ok(content) = fs::read_to_string(&resolv_path) {
            for line in content.lines() {
                let line = line.trim();
                if let Some(server) = line.strip_prefix("nameserver ") {
                    dns.servers.push(server.trim().to_string());
                } else if let Some(domains) = line.strip_prefix("search ") {
                    dns.search_domains =
                        domains.split_whitespace().map(|s| s.to_string()).collect();
                } else if let Some(opt) = line.strip_prefix("options ") {
                    dns.options.push(opt.trim().to_string());
                }
            }
        }

        Ok(dns)
    }

    /// Get information about a specific namespace.
    pub fn get_namespace_info(&self, name: &str) -> SegwireResult<NamespaceInfo> {
        crate::utils::validate_namespace_name(name)?;

        if let NetlinkBackend::Simulated(state) = &self.backend {
            let state = state.borrow();
            return state
                .namespaces
                .get(name)
                .cloned()
                .ok_or_else(|| NetlinkError::NamespaceNotFound(name.to_string()).into());
        }

        let ns_path = PathBuf::from(format!("{}/{}", NETNS_RUN_DIR, name));
        if !ns_path.exists() {
            return Err(NetlinkError::NamespaceNotFound(name.to_string()).into());
        }

        let id = netlink_raw::ns_inode(&ns_path);

        Ok(NamespaceInfo {
            name: name.to_string(),
            id,
            path: ns_path,
            active: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_namespace_name() {
        let _mgr_result = NetlinkManager::new_simulated().unwrap();
        // Only run validation tests that don't need a real socket
    }

    #[test]
    fn test_format_route() {
        // format_route is in netlink_raw — tested there.
    }

    #[test]
    fn test_dns_config_rendering() {
        let dns = DnsConfig {
            servers: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
            search_domains: vec!["example.com".to_string()],
            options: vec!["ndots:2".to_string()],
        };

        // Build the resolv.conf content the same way configure_namespace_dns does
        let mut content = String::new();
        for server in &dns.servers {
            content.push_str(&format!("nameserver {}\n", server));
        }
        if !dns.search_domains.is_empty() {
            content.push_str(&format!("search {}\n", dns.search_domains.join(" ")));
        }
        for opt in &dns.options {
            content.push_str(&format!("options {}\n", opt));
        }

        assert!(content.contains("nameserver 8.8.8.8"));
        assert!(content.contains("nameserver 1.1.1.1"));
        assert!(content.contains("search example.com"));
        assert!(content.contains("options ndots:2"));
    }
}
