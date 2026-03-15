//! Netlink interface wrapper for network namespace operations.
//!
//! Provides a high-level `NetlinkManager` for managing Linux network namespaces.
//! All kernel interactions are delegated to [`crate::netlink_raw`] and
//! [`crate::netns_raw`].
//!
//! Type definitions (`NetlinkError`, `NamespaceInfo`, `RouteConfig`, `DnsConfig`)
//! live in `segwire-common::netlink` and are re-used here.

use crate::netlink_raw::{self, RawRouteParams};
use crate::netns_raw;
use segwire_common::error::{SegwireError, SegwireResult};
use segwire_common::netlink::{DnsConfig, NamespaceInfo, NetlinkError, RouteConfig};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Directory where named network namespaces are persisted.
const NETNS_RUN_DIR: &str = "/var/run/netns";

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
    /// Real backend for production use.
    ///
    /// No socket is stored — a fresh netlink socket is created on demand
    /// inside each method.  This keeps `NetlinkManager: Send`.
    Real,
    /// In-memory simulation for testing.
    Simulated(RefCell<SimulatedState>),
}

/// High-level interface for network namespace operations.
///
/// Uses [`crate::netlink_raw`] and [`crate::netns_raw`] for all kernel
/// interactions.  Link operations that need a netlink socket create one
/// on demand (no `!Send` state is stored).
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
        if !netns_raw::is_root() {
            return Err(NetlinkError::InsufficientPrivileges.into());
        }

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
            backend: NetlinkBackend::Real,
        })
    }

    /// Create a simulated NetlinkManager for testing.
    ///
    /// All namespace operations act on an in-memory map — no root required,
    /// no real kernel namespaces are created.
    pub fn new_simulated() -> SegwireResult<Self> {
        info!("Creating simulated NetlinkManager (no real namespace operations)");
        Ok(Self {
            backend: NetlinkBackend::Simulated(RefCell::new(SimulatedState::new())),
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

    // -----------------------------------------------------------------------
    // Namespace lifecycle
    // -----------------------------------------------------------------------

    /// Create a new network namespace.
    pub fn create_namespace(&self, name: &str) -> SegwireResult<NamespaceInfo> {
        segwire_common::utils::validate_namespace_name(name)?;

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
        if let Err(msg) = netns_raw::create_netns(&ns_path) {
            let _ = fs::remove_file(&ns_path);
            return Err(NetlinkError::CreateFailed(name.to_string(), msg).into());
        }

        let id = netns_raw::ns_inode(&ns_path);

        info!("Created namespace '{}'", name);
        Ok(NamespaceInfo {
            name: name.to_string(),
            id,
            path: ns_path,
            active: true,
        })
    }

    /// Delete a network namespace.
    pub fn delete_namespace(&self, name: &str) -> SegwireResult<()> {
        segwire_common::utils::validate_namespace_name(name)?;

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

        netns_raw::delete_netns(&ns_path)
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
            let id = netns_raw::ns_inode(&path);

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

        let names = netlink_raw::dump_interface_names_fresh().map_err(NetlinkError::SocketError)?;
        Ok(names)
    }

    /// Check if a network interface exists.
    pub fn interface_exists(&self, interface_name: &str) -> SegwireResult<bool> {
        let interfaces = self.list_interfaces()?;
        Ok(interfaces.iter().any(|n| n == interface_name))
    }

    /// Check if a network interface is available for namespace assignment.
    pub fn interface_available(&self, interface_name: &str) -> SegwireResult<bool> {
        if interface_name == "lo" {
            return Ok(false);
        }
        self.interface_exists(interface_name)
    }

    /// List network interfaces inside a specific namespace.
    pub fn list_namespace_interfaces(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(vec!["lo".to_string()]);
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let result = netns_raw::run_in_namespace(&ns_path, netlink_raw::dump_interface_names_fresh)
            .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    /// Move a network interface to a namespace.
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
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        segwire_common::utils::validate_interface_name(interface_name)?;

        if !self.interface_exists(interface_name)? {
            return Err(NetlinkError::InterfaceNotFound(interface_name.to_string()).into());
        }
        if !self.interface_available(interface_name)? {
            return Err(NetlinkError::InterfaceNotAvailable(interface_name.to_string()).into());
        }
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ifindex = netlink_raw::get_interface_index_fresh(interface_name)
            .map_err(NetlinkError::SocketError)?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        netlink_raw::move_interface_to_ns_sync(ifindex, &ns_path).map_err(|e| {
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
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        segwire_common::utils::validate_interface_name(interface_name)?;

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

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            netlink_raw::move_interface_to_default_ns(&iface_name)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(|e| {
            NetlinkError::InterfaceMoveFailed(iface_name_for_err, "default".to_string(), e).into()
        })
    }

    /// Create a generic virtual interface (dummy, bridge, macvlan, ipvlan) inside a namespace.
    pub fn create_virtual_interface(
        &self,
        namespace_name: &str,
        vif_name: &str,
        vif_type: &str,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Created virtual interface '{}' of type '{}' in namespace '{}'",
                vif_name, vif_type, namespace_name
            );
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        segwire_common::utils::validate_interface_name(vif_name)?;

        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_interfaces = self.list_namespace_interfaces(namespace_name)?;
        if ns_interfaces.iter().any(|n| n == vif_name) {
            return Err(NetlinkError::VirtualInterfaceCreateFailed(
                vif_name.to_string(),
                "Interface already exists in namespace".to_string(),
            )
            .into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let vif_name_clone = vif_name.to_string();
        let vif_type_clone = vif_type.to_string();

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            netlink_raw::create_virtual_interface_sync(&vif_name_clone, &vif_type_clone)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(|e| e.into())
    }

    /// Create a WireGuard interface inside a namespace.
    pub fn create_wireguard_interface(
        &self,
        namespace_name: &str,
        wg_name: &str,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Created WireGuard interface '{}' in namespace '{}'",
                wg_name, namespace_name
            );
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        segwire_common::utils::validate_interface_name(wg_name)?;

        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let wg_name_clone = wg_name.to_string();

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            crate::wireguard_raw::create_wireguard_interface_sync(&wg_name_clone)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(|e| e.into())
    }

    /// Configure a WireGuard device (keys, peers, endpoints) inside a namespace.
    pub fn configure_wireguard(
        &self,
        namespace_name: &str,
        wg_name: &str,
        config: &segwire_common::config::WireguardConfig,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Configured WireGuard device '{}' in namespace '{}' ({} peers)",
                wg_name,
                namespace_name,
                config.peers.len()
            );
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let wg_name_clone = wg_name.to_string();
        let config_clone = config.clone();

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            crate::wireguard_raw::configure_wireguard_device(&wg_name_clone, &config_clone)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    /// Create a virtual ethernet (veth) pair.
    pub fn create_veth_pair(&self, veth_name: &str, peer_name: &str) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Created veth pair '{}'<->'{}'", veth_name, peer_name);
            return Ok(());
        }
        segwire_common::utils::validate_interface_name(veth_name)?;
        segwire_common::utils::validate_interface_name(peer_name)?;

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

        netlink_raw::create_veth_pair_sync(veth_name, peer_name).map_err(|e| {
            NetlinkError::VirtualInterfaceCreateFailed(veth_name.to_string(), e.to_string())
        })?;

        info!("Created veth pair '{}'<->'{}'", veth_name, peer_name);
        Ok(())
    }

    /// Add an IP address to an interface inside a namespace.
    pub fn add_address_in_namespace(
        &self,
        namespace_name: &str,
        ifname: &str,
        addr: std::net::IpAddr,
        prefix_len: u8,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Added address {}/{} to interface '{}' in namespace '{}'",
                addr, prefix_len, ifname, namespace_name
            );
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let ifname = ifname.to_string();

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            netlink_raw::add_address_fresh(&ifname, addr, prefix_len)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    /// Bring a network interface UP inside a namespace.
    pub fn set_link_up_in_namespace(
        &self,
        namespace_name: &str,
        ifname: &str,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!(
                "[SIM] Set interface '{}' UP in namespace '{}'",
                ifname, namespace_name
            );
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let ifname = ifname.to_string();

        let result = netns_raw::run_in_namespace(&ns_path, move || {
            netlink_raw::set_link_up_fresh(&ifname)
        })
        .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    /// Bring a network interface UP in the host (default) namespace.
    pub fn set_link_up(&self, ifname: &str) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Set interface '{}' UP", ifname);
            return Ok(());
        }

        netlink_raw::set_link_up_fresh(ifname)
            .map_err(SegwireError::Network)
    }

    /// Add an IP address to an interface in the host (default) namespace.
    pub fn add_address(&self, ifname: &str, addr: std::net::IpAddr, prefix_len: u8) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Added address {}/{} to interface '{}'", addr, prefix_len, ifname);
            return Ok(());
        }

        netlink_raw::add_address_fresh(ifname, addr, prefix_len)
            .map_err(SegwireError::Network)
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
        segwire_common::utils::validate_namespace_name(namespace_name)?;

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
            netns_raw::run_in_namespace(&ns_path, move || netlink_raw::add_route_fresh(params))
                .map_err(SegwireError::Network)?;

        result.map_err(|e| NetlinkError::RouteConfigFailed(namespace_name.to_string(), e).into())
    }

    /// List routes inside a namespace.
    pub fn list_namespace_routes(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(Vec::new());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let result = netns_raw::run_in_namespace(&ns_path, netlink_raw::dump_routes_fresh)
            .map_err(SegwireError::Network)?;

        result.map_err(SegwireError::Network)
    }

    // -----------------------------------------------------------------------
    // DNS configuration (file I/O only)
    // -----------------------------------------------------------------------

    /// Configure DNS resolution in a namespace.
    pub fn configure_namespace_dns(
        &self,
        namespace_name: &str,
        dns_config: &DnsConfig,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Configured DNS for namespace '{}'", namespace_name);
            return Ok(());
        }
        segwire_common::utils::validate_namespace_name(namespace_name)?;
        dns_config.validate()?;

        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

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
        segwire_common::utils::validate_namespace_name(namespace_name)?;
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
        segwire_common::utils::validate_namespace_name(name)?;

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

        let id = netns_raw::ns_inode(&ns_path);

        Ok(NamespaceInfo {
            name: name.to_string(),
            id,
            path: ns_path,
            active: true,
        })
    }
}
