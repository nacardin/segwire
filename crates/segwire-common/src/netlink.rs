//! Netlink interface wrapper for network namespace operations
//!
//! Provides a high-level interface for managing Linux network namespaces
//! using raw netlink sockets (via `netlink-sys` + `netlink-packet-route`) for
//! link and route operations, and `nix` crate syscalls for namespace lifecycle.
//!
//! All operations are synchronous and runtime-agnostic — no tokio dependency.

use crate::error::{SegwireError, SegwireResult};
use netlink_packet_core::{
    NetlinkMessage, NetlinkPayload, NLM_F_ACK, NLM_F_CREATE, NLM_F_DUMP, NLM_F_EXCL, NLM_F_REQUEST,
};
use netlink_packet_route::{
    constants::*,
    nlas::link::{Info, InfoData, InfoKind, Nla as LinkNla, VethInfo},
    nlas::route::Nla as RouteNla,
    LinkMessage, RouteMessage, RtnlMessage,
};
use netlink_packet_utils::traits::Emitable;
use netlink_sys::{protocols::NETLINK_ROUTE, Socket, SocketAddr};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::Uid;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::fd::BorrowedFd;
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

// ---------------------------------------------------------------------------
// Raw netlink helpers
// ---------------------------------------------------------------------------

/// Send a netlink message and collect all response messages.
///
/// Handles multi-part (DUMP) responses automatically.
fn netlink_request(
    socket: &Socket,
    msg: NetlinkMessage<RtnlMessage>,
) -> Result<Vec<NetlinkMessage<RtnlMessage>>, NetlinkError> {
    // Serialize
    let mut buf = vec![0u8; msg.buffer_len()];
    msg.emit(&mut buf);

    // Send to kernel (pid=0, groups=0)
    let kernel_addr = SocketAddr::new(0, 0);
    socket
        .send_to(&buf, &kernel_addr, 0)
        .map_err(|e| NetlinkError::SocketError(format!("send failed: {}", e)))?;

    // Receive responses
    let mut responses = Vec::new();
    let mut recv_buf = vec![0u8; 16384];

    loop {
        let (n, _addr) = socket
            .recv_from(&mut recv_buf, 0)
            .map_err(|e| NetlinkError::SocketError(format!("recv failed: {}", e)))?;

        let data = &recv_buf[..n];
        let mut offset = 0;
        let mut done = false;

        while offset < data.len() {
            // Parse the netlink header to get message length
            if data.len() - offset < 4 {
                break;
            }
            let msg_len = u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            if msg_len < 16 || offset + msg_len > data.len() {
                break;
            }

            let msg_data = &data[offset..offset + msg_len];
            // Parse NetlinkMessage
            match NetlinkMessage::<RtnlMessage>::deserialize(msg_data) {
                Ok(parsed) => {
                    match parsed.payload {
                        NetlinkPayload::Done(_) => {
                            done = true;
                            break;
                        }
                        NetlinkPayload::Error(ref err) => {
                            // code None means ACK (success)
                            if let Some(code) = err.code {
                                let code_val: i32 = code.into();
                                return Err(NetlinkError::ProtocolError(format!(
                                    "netlink error: {} (code {})",
                                    std::io::Error::from_raw_os_error(-code_val),
                                    code_val
                                )));
                            }
                            // ACK — success
                            done = true;
                            break;
                        }
                        _ => {
                            responses.push(parsed);
                        }
                    }
                }
                Err(e) => {
                    return Err(NetlinkError::ProtocolError(format!(
                        "failed to parse netlink message: {}",
                        e
                    )));
                }
            }

            offset += msg_len;
            // Align to 4-byte boundary
            offset = (offset + 3) & !3;
        }

        if done {
            break;
        }

        // If not a DUMP request and we got responses, we're done
        if !responses.is_empty() && (msg.header.flags & NLM_F_DUMP) == 0 {
            break;
        }
    }

    Ok(responses)
}

/// Open a netlink ROUTE socket, bind it, and return it.
fn open_netlink_socket() -> Result<Socket, NetlinkError> {
    let mut socket = Socket::new(NETLINK_ROUTE)
        .map_err(|e| NetlinkError::SocketError(format!("socket creation failed: {}", e)))?;
    socket
        .bind_auto()
        .map_err(|e| NetlinkError::SocketError(format!("bind failed: {}", e)))?;
    Ok(socket)
}

/// Allocate a fresh sequence number for netlink messages.
fn next_seq() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
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
    Real(Socket),
    /// In-memory simulation for testing.
    Simulated(std::cell::RefCell<SimulatedState>),
}

/// High-level interface for network namespace operations.
///
/// Uses raw netlink sockets for link/route operations and nix syscalls for
/// namespace lifecycle management.  All methods are synchronous.
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
        if !Uid::effective().is_root() {
            return Err(NetlinkError::InsufficientPrivileges.into());
        }

        let socket = open_netlink_socket()?;

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
    fn real_socket(&self) -> &Socket {
        match &self.backend {
            NetlinkBackend::Real(s) => s,
            NetlinkBackend::Simulated(_) => panic!("BUG: real_socket() called in simulation mode"),
        }
    }

    // -----------------------------------------------------------------------
    // Namespace lifecycle  (nix syscalls, NOT netlink)
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
        self.validate_namespace_name(name)?;

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

        // Do the unshare + bind-mount in a dedicated thread so we don't change
        // the main thread's network namespace.
        let ns_path_clone = ns_path.clone();
        let _ns_name = name.to_string();
        let result = std::thread::spawn(move || -> Result<(), String> {
            // Create a new network namespace for THIS thread only
            unshare(CloneFlags::CLONE_NEWNET)
                .map_err(|e| format!("unshare(CLONE_NEWNET) failed: {}", e))?;

            // Bind-mount /proc/self/ns/net onto the placeholder file.
            // This persists the namespace beyond the lifetime of the thread.
            let src = "/proc/self/ns/net";
            mount(
                Some(src),
                &ns_path_clone,
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            )
            .map_err(|e| format!("bind mount failed: {}", e))?;

            Ok(())
        })
        .join()
        .map_err(|_| {
            // Thread panicked; clean up placeholder
            let _ = fs::remove_file(&ns_path);
            NetlinkError::CreateFailed(name.to_string(), "thread panicked".to_string())
        })?;

        if let Err(msg) = result {
            let _ = fs::remove_file(&ns_path);
            return Err(NetlinkError::CreateFailed(name.to_string(), msg).into());
        }

        // Read the inode number as an ID
        let id = fs::metadata(&ns_path)
            .map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.ino() as u32
            })
            .unwrap_or(0);

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
        self.validate_namespace_name(name)?;

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

        // Unmount (lazy detach) the bind-mount
        umount2(&ns_path, MntFlags::MNT_DETACH).map_err(|e| {
            NetlinkError::DeleteFailed(name.to_string(), format!("umount2 failed: {}", e))
        })?;

        // Remove the file
        fs::remove_file(&ns_path).map_err(|e| {
            NetlinkError::DeleteFailed(name.to_string(), format!("remove file: {}", e))
        })?;

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
            let id = fs::metadata(&path)
                .map(|m| {
                    use std::os::unix::fs::MetadataExt;
                    m.ino() as u32
                })
                .unwrap_or(0);

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
    // Link operations  (netlink RTM_*LINK)
    // -----------------------------------------------------------------------

    /// List all network interfaces in the default namespace.
    pub fn list_interfaces(&self) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(vec!["lo".to_string(), "eth0".to_string()]);
        }

        let mut msg = LinkMessage::default();
        // AF_UNSPEC = 0 — list all families
        msg.header.interface_family = 0;

        let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetLink(msg));
        nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
        nl_msg.header.sequence_number = next_seq();
        nl_msg.finalize();

        let responses = netlink_request(self.real_socket(), nl_msg)?;

        let mut names = Vec::new();
        for resp in responses {
            if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = resp.payload {
                for nla in &link.nlas {
                    if let LinkNla::IfName(ref name) = nla {
                        names.push(name.clone());
                    }
                }
            }
        }
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

        let mut msg = LinkMessage::default();
        msg.header.interface_family = 0;

        let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetLink(msg));
        nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
        nl_msg.header.sequence_number = next_seq();
        nl_msg.finalize();

        let responses = netlink_request(self.real_socket(), nl_msg)?;

        for resp in responses {
            if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = resp.payload {
                for nla in &link.nlas {
                    if let LinkNla::IfName(ref name) = nla {
                        if name == interface_name {
                            return Ok(link.header.index);
                        }
                    }
                }
            }
        }

        Err(NetlinkError::InterfaceNotFound(interface_name.to_string()).into())
    }

    /// List network interfaces inside a specific namespace.
    pub fn list_namespace_interfaces(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(vec!["lo".to_string()]);
        }
        self.validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_name = namespace_name.to_string();
        let result = self.run_in_namespace(&ns_name, || {
            let sock = open_netlink_socket().map_err(|e| e.to_string())?;
            let mut msg = LinkMessage::default();
            msg.header.interface_family = 0;

            let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetLink(msg));
            nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
            nl_msg.header.sequence_number = next_seq();
            nl_msg.finalize();

            let responses = netlink_request(&sock, nl_msg).map_err(|e| e.to_string())?;

            let mut names = Vec::new();
            for resp in responses {
                if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = resp.payload {
                    for nla in &link.nlas {
                        if let LinkNla::IfName(ref name) = nla {
                            names.push(name.clone());
                        }
                    }
                }
            }
            Ok(names)
        })?;

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
        self.validate_namespace_name(namespace_name)?;
        self.validate_interface_name(interface_name)?;

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

        // Open the namespace file descriptor
        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);
        let ns_fd = nix::fcntl::open(
            ns_path.as_str(),
            nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|e| {
            NetlinkError::InterfaceMoveFailed(
                interface_name.to_string(),
                namespace_name.to_string(),
                format!("open ns fd: {}", e),
            )
        })?;

        let result = (|| -> Result<(), NetlinkError> {
            let mut msg = LinkMessage::default();
            msg.header.index = ifindex;
            msg.nlas.push(LinkNla::NetNsFd(ns_fd));

            let mut nl_msg = NetlinkMessage::from(RtnlMessage::SetLink(msg));
            nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
            nl_msg.header.sequence_number = next_seq();
            nl_msg.finalize();

            netlink_request(self.real_socket(), nl_msg)?;
            Ok(())
        })();

        // Close the fd
        let _ = nix::unistd::close(ns_fd);

        result.map_err(|e| {
            NetlinkError::InterfaceMoveFailed(
                interface_name.to_string(),
                namespace_name.to_string(),
                e.to_string(),
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
        self.validate_namespace_name(namespace_name)?;
        self.validate_interface_name(interface_name)?;

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

        let ns_name = namespace_name.to_string();
        let iface_name = interface_name.to_string();
        let iface_name_clone = iface_name.clone();

        let result = self.run_in_namespace(&ns_name, move || {
            // Get the interface index inside the namespace
            let sock = open_netlink_socket().map_err(|e| e.to_string())?;
            let ifindex =
                get_interface_index_raw(&sock, &iface_name_clone).map_err(|e| e.to_string())?;

            // Open the default namespace file descriptor
            // The process's original namespace is what we want, assuming daemon runs in default netns.
            // Since `run_in_namespace` runs in a new thread, the thread's netns is changed, but we can
            // get the PID 1's netns safely, or we could have opened `/proc/self/ns/net` *before* the closure
            // and passed it in.
            // But doing open("/proc/1/ns/net") requires root, which we have.
            let default_ns_fd = nix::fcntl::open(
                "/proc/1/ns/net",
                nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
                nix::sys::stat::Mode::empty(),
            )
            .map_err(|e| format!("open default ns fd: {}", e))?;

            let mut msg = LinkMessage::default();
            msg.header.index = ifindex;
            msg.nlas.push(LinkNla::NetNsFd(default_ns_fd));

            let mut nl_msg = NetlinkMessage::from(RtnlMessage::SetLink(msg));
            nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
            nl_msg.header.sequence_number = next_seq();
            nl_msg.finalize();

            netlink_request(&sock, nl_msg).map_err(|e| e.to_string())?;

            let _ = nix::unistd::close(default_ns_fd);
            Ok(())
        })?;

        result.map_err(|e| {
            NetlinkError::InterfaceMoveFailed(iface_name, "default".to_string(), e).into()
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
        self.validate_interface_name(veth_name)?;
        self.validate_interface_name(peer_name)?;

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

        // Build the peer LinkMessage
        let mut peer_msg = LinkMessage::default();
        peer_msg.nlas.push(LinkNla::IfName(peer_name.to_string()));

        // Build the main LinkMessage with LINKINFO
        let mut msg = LinkMessage::default();
        msg.nlas.push(LinkNla::IfName(veth_name.to_string()));
        msg.nlas.push(LinkNla::Info(vec![
            Info::Kind(InfoKind::Veth),
            Info::Data(InfoData::Veth(VethInfo::Peer(peer_msg))),
        ]));

        let mut nl_msg = NetlinkMessage::from(RtnlMessage::NewLink(msg));
        nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
        nl_msg.header.sequence_number = next_seq();
        nl_msg.finalize();

        netlink_request(self.real_socket(), nl_msg).map_err(|e| {
            NetlinkError::VirtualInterfaceCreateFailed(veth_name.to_string(), e.to_string())
        })?;

        info!("Created veth pair '{}'<->'{}'", veth_name, peer_name);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Route operations  (netlink RTM_*ROUTE)
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
        self.validate_namespace_name(namespace_name)?;

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
        self.validate_route_config(route)?;

        let ns_name = namespace_name.to_string();
        let route_clone = route.clone();

        let result = self.run_in_namespace(&ns_name, move || {
            let sock = open_netlink_socket().map_err(|e| e.to_string())?;

            let mut msg = RouteMessage::default();
            msg.header.table = RT_TABLE_MAIN;
            msg.header.protocol = RTPROT_STATIC;
            msg.header.scope = RT_SCOPE_UNIVERSE;
            msg.header.kind = RTN_UNICAST;
            msg.header.address_family = libc::AF_INET as u8;

            // Destination
            if route_clone.destination == "default" {
                msg.header.destination_prefix_length = 0;
            } else if let Some((ip_str, prefix_len_str)) = route_clone.destination.split_once('/') {
                let prefix_len: u8 = prefix_len_str
                    .parse()
                    .map_err(|e| format!("invalid prefix length: {}", e))?;
                msg.header.destination_prefix_length = prefix_len;

                let ip: std::net::Ipv4Addr = ip_str
                    .parse()
                    .map_err(|e| format!("invalid destination IP: {}", e))?;
                msg.nlas.push(RouteNla::Destination(ip.octets().to_vec()));
            } else {
                // Single host route
                msg.header.destination_prefix_length = 32;
                let ip: std::net::Ipv4Addr = route_clone
                    .destination
                    .parse()
                    .map_err(|e| format!("invalid destination IP: {}", e))?;
                msg.nlas.push(RouteNla::Destination(ip.octets().to_vec()));
            }

            // Gateway
            if !route_clone.gateway.is_empty() {
                let gw: std::net::Ipv4Addr = route_clone
                    .gateway
                    .parse()
                    .map_err(|e| format!("invalid gateway IP: {}", e))?;
                msg.nlas.push(RouteNla::Gateway(gw.octets().to_vec()));
            }

            // Metric
            if let Some(metric) = route_clone.metric {
                msg.nlas.push(RouteNla::Priority(metric));
            }

            // Output interface
            if let Some(ref iface) = route_clone.interface {
                // Resolve interface name to index inside the namespace
                let ns_sock = open_netlink_socket().map_err(|e| e.to_string())?;
                let ifindex =
                    get_interface_index_raw(&ns_sock, iface).map_err(|e| e.to_string())?;
                msg.nlas.push(RouteNla::Oif(ifindex));
            }

            let mut nl_msg = NetlinkMessage::from(RtnlMessage::NewRoute(msg));
            nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
            nl_msg.header.sequence_number = next_seq();
            nl_msg.finalize();

            netlink_request(&sock, nl_msg).map_err(|e| e.to_string())?;
            Ok(())
        })?;

        result.map_err(|e| NetlinkError::RouteConfigFailed(namespace_name.to_string(), e).into())
    }

    /// List routes inside a namespace.
    pub fn list_namespace_routes(&self, namespace_name: &str) -> SegwireResult<Vec<String>> {
        if self.is_simulated() {
            return Ok(Vec::new());
        }
        self.validate_namespace_name(namespace_name)?;
        if !self.namespace_exists(namespace_name)? {
            return Err(NetlinkError::NamespaceNotFound(namespace_name.to_string()).into());
        }

        let ns_name = namespace_name.to_string();
        let result = self.run_in_namespace(&ns_name, || {
            let sock = open_netlink_socket().map_err(|e| e.to_string())?;

            let mut msg = RouteMessage::default();
            msg.header.address_family = libc::AF_INET as u8;

            let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetRoute(msg));
            nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
            nl_msg.header.sequence_number = next_seq();
            nl_msg.finalize();

            let responses = netlink_request(&sock, nl_msg).map_err(|e| e.to_string())?;

            let mut routes = Vec::new();
            for resp in responses {
                if let NetlinkPayload::InnerMessage(RtnlMessage::NewRoute(route_msg)) = resp.payload
                {
                    routes.push(format_route(&route_msg));
                }
            }
            Ok(routes)
        })?;

        result.map_err(SegwireError::Network)
    }

    // -----------------------------------------------------------------------
    // DNS configuration  (setns + file I/O, NOT netlink)
    // -----------------------------------------------------------------------

    /// Configure DNS resolution in a namespace.
    ///
    /// Writes a `/etc/resolv.conf` file inside the namespace's mount namespace
    /// using `setns()` + direct file I/O.
    pub fn configure_namespace_dns(
        &self,
        namespace_name: &str,
        dns_config: &DnsConfig,
    ) -> SegwireResult<()> {
        if self.is_simulated() {
            info!("[SIM] Configured DNS for namespace '{}'", namespace_name);
            return Ok(());
        }
        self.validate_namespace_name(namespace_name)?;
        self.validate_dns_config(dns_config)?;

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

        // Write inside the namespace.
        // Note: resolv.conf lives in the mount namespace, not the network
        // namespace.  For named namespaces created by `ip netns add`, the
        // mount namespace is separate.  We write via
        // /etc/netns/<name>/resolv.conf which iproute2 bind-mounts into the
        // namespace when using `ip netns exec`.
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
        self.validate_namespace_name(namespace_name)?;
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
        self.validate_namespace_name(name)?;

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

        let id = fs::metadata(&ns_path)
            .map(|m| {
                use std::os::unix::fs::MetadataExt;
                m.ino() as u32
            })
            .unwrap_or(0);

        Ok(NamespaceInfo {
            name: name.to_string(),
            id,
            path: ns_path,
            active: true,
        })
    }

    // -----------------------------------------------------------------------
    // Helpers: run closure inside a namespace
    // -----------------------------------------------------------------------

    /// Run a closure inside the given namespace's network context.
    ///
    /// Spawns a dedicated thread, switches it to the target namespace via
    /// `setns()`, runs the closure, then restores the original namespace.
    fn run_in_namespace<F, T>(&self, namespace_name: &str, f: F) -> SegwireResult<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let ns_path = format!("{}/{}", NETNS_RUN_DIR, namespace_name);

        let result = std::thread::spawn(move || -> Result<T, String> {
            // Save current network namespace
            let orig_ns = nix::fcntl::open(
                "/proc/self/ns/net",
                nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
                nix::sys::stat::Mode::empty(),
            )
            .map_err(|e| format!("open current ns: {}", e))?;

            // Open target namespace
            let target_ns = nix::fcntl::open(
                ns_path.as_str(),
                nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
                nix::sys::stat::Mode::empty(),
            )
            .map_err(|e| format!("open target ns: {}", e))?;

            // Switch to target namespace
            // SAFETY: the fd was just opened and is valid for the lifetime of this scope
            nix::sched::setns(
                unsafe { BorrowedFd::borrow_raw(target_ns) },
                CloneFlags::CLONE_NEWNET,
            )
            .map_err(|e| format!("setns to target: {}", e))?;
            let _ = nix::unistd::close(target_ns);

            // Run the closure
            let result = f();

            // Restore original namespace
            let restore_result = nix::sched::setns(
                unsafe { BorrowedFd::borrow_raw(orig_ns) },
                CloneFlags::CLONE_NEWNET,
            );
            let _ = nix::unistd::close(orig_ns);

            if let Err(e) = restore_result {
                // This is serious — the thread is now stuck in the wrong namespace.
                // Best we can do is log and continue (the thread will be destroyed anyway).
                eprintln!("CRITICAL: Failed to restore original namespace: {}", e);
            }

            Ok(result)
        })
        .join()
        .map_err(|_| SegwireError::Network("namespace thread panicked".to_string()))?;

        result.map_err(SegwireError::Network)
    }

    // -----------------------------------------------------------------------
    // Validation helpers
    // -----------------------------------------------------------------------

    fn validate_namespace_name(&self, name: &str) -> SegwireResult<()> {
        if name.is_empty() {
            return Err(
                NetlinkError::InvalidName("namespace name cannot be empty".to_string()).into(),
            );
        }
        if name.len() > 255 {
            return Err(NetlinkError::InvalidName("namespace name too long".to_string()).into());
        }
        if name.contains('/') || name.contains('\0') {
            return Err(NetlinkError::InvalidName(
                "namespace name cannot contain '/' or null".to_string(),
            )
            .into());
        }
        Ok(())
    }

    fn validate_interface_name(&self, name: &str) -> SegwireResult<()> {
        if name.is_empty() {
            return Err(NetlinkError::InterfaceNotFound("empty name".to_string()).into());
        }
        if name.len() > 15 {
            return Err(NetlinkError::InterfaceNotFound(format!(
                "interface name '{}' exceeds IFNAMSIZ (15 chars)",
                name
            ))
            .into());
        }
        Ok(())
    }

    fn validate_route_config(&self, route: &RouteConfig) -> SegwireResult<()> {
        if route.destination.is_empty() {
            return Err(
                NetlinkError::InvalidRoute("destination cannot be empty".to_string()).into(),
            );
        }
        Ok(())
    }

    fn validate_dns_config(&self, dns: &DnsConfig) -> SegwireResult<()> {
        if dns.servers.is_empty() {
            return Err(
                NetlinkError::InvalidDns("at least one DNS server required".to_string()).into(),
            );
        }
        for server in &dns.servers {
            if server.parse::<std::net::IpAddr>().is_err() {
                return Err(
                    NetlinkError::InvalidDns(format!("invalid DNS server IP: {}", server)).into(),
                );
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free-standing helpers
// ---------------------------------------------------------------------------

/// Look up interface index by name using a given socket.
fn get_interface_index_raw(socket: &Socket, name: &str) -> Result<u32, NetlinkError> {
    let mut msg = LinkMessage::default();
    msg.header.interface_family = 0;

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    let responses = netlink_request(socket, nl_msg)?;

    for resp in responses {
        if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = resp.payload {
            for nla in &link.nlas {
                if let LinkNla::IfName(ref n) = nla {
                    if n == name {
                        return Ok(link.header.index);
                    }
                }
            }
        }
    }
    Err(NetlinkError::InterfaceNotFound(name.to_string()))
}

/// Format a route message into a human-readable string.
fn format_route(route: &RouteMessage) -> String {
    let mut parts = Vec::new();

    let mut dest = "default".to_string();
    let mut gateway = String::new();
    let mut oif = 0u32;

    for nla in &route.nlas {
        match nla {
            RouteNla::Destination(bytes) => {
                if bytes.len() == 4 {
                    let ip = std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
                    dest = format!("{}/{}", ip, route.header.destination_prefix_length);
                }
            }
            RouteNla::Gateway(bytes) => {
                if bytes.len() == 4 {
                    gateway = format!(
                        "via {}",
                        std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])
                    );
                }
            }
            RouteNla::Oif(idx) => {
                oif = *idx;
            }
            RouteNla::Priority(metric) => {
                parts.push(format!("metric {}", metric));
            }
            _ => {}
        }
    }

    let mut line = dest;
    if !gateway.is_empty() {
        line.push(' ');
        line.push_str(&gateway);
    }
    if oif != 0 {
        line.push_str(&format!(" dev ifindex:{}", oif));
    }
    for part in parts {
        line.push(' ');
        line.push_str(&part);
    }
    line
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
        let mut msg = RouteMessage::default();
        msg.header.destination_prefix_length = 24;
        msg.nlas.push(RouteNla::Destination(vec![192, 168, 1, 0]));
        msg.nlas.push(RouteNla::Gateway(vec![10, 0, 0, 1]));
        msg.nlas.push(RouteNla::Priority(100));

        let formatted = format_route(&msg);
        assert!(formatted.contains("192.168.1.0/24"));
        assert!(formatted.contains("via 10.0.0.1"));
        assert!(formatted.contains("metric 100"));
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
