//! Low-level netlink and namespace syscall helpers.
//!
//! This module owns **all** interactions with the kernel:
//! - Raw netlink socket send/recv via `netlink-sys`
//! - Netlink message serialization/deserialization via `netlink-packet-*`
//! - Namespace lifecycle syscalls via `nix` (unshare, mount, umount2, setns)
//! - File descriptor management via `nix` (open, close)
//!
//! The companion `netlink` module provides a safe, high-level API on top of
//! these primitives and contains **zero** `unsafe` blocks or `nix` imports.

use crate::netlink::NetlinkError;
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
use std::os::fd::BorrowedFd;
use std::path::Path;

// ---------------------------------------------------------------------------
// Netlink socket wrapper
// ---------------------------------------------------------------------------

/// Opaque wrapper around a raw netlink ROUTE socket.
///
/// This prevents the `netlink` module from needing to import `netlink_sys::Socket`.
pub(crate) struct RawNetlinkSocket {
    inner: Socket,
}

impl RawNetlinkSocket {
    /// Open a netlink ROUTE socket and bind it.
    pub(crate) fn open() -> Result<Self, NetlinkError> {
        let mut socket = Socket::new(NETLINK_ROUTE)
            .map_err(|e| NetlinkError::SocketError(format!("socket creation failed: {}", e)))?;
        socket
            .bind_auto()
            .map_err(|e| NetlinkError::SocketError(format!("bind failed: {}", e)))?;
        Ok(Self { inner: socket })
    }
}

// ---------------------------------------------------------------------------
// Sequence number
// ---------------------------------------------------------------------------

/// Allocate a fresh sequence number for netlink messages.
fn next_seq() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Raw netlink request/response
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

// ---------------------------------------------------------------------------
// Privilege check
// ---------------------------------------------------------------------------

/// Check whether the effective UID is root.
pub(crate) fn is_root() -> bool {
    Uid::effective().is_root()
}

// ---------------------------------------------------------------------------
// Namespace lifecycle (nix syscalls)
// ---------------------------------------------------------------------------

/// Create a new named network namespace.
///
/// Spawns a thread that calls `unshare(CLONE_NEWNET)` and then bind-mounts
/// `/proc/self/ns/net` onto `ns_path` to persist the namespace.
pub(crate) fn create_netns(ns_path: &Path) -> Result<(), String> {
    let ns_path = ns_path.to_path_buf();
    let result = std::thread::spawn(move || -> Result<(), String> {
        // Create a new network namespace for THIS thread only
        unshare(CloneFlags::CLONE_NEWNET)
            .map_err(|e| format!("unshare(CLONE_NEWNET) failed: {}", e))?;

        // Bind-mount /proc/self/ns/net onto the placeholder file.
        // This persists the namespace beyond the lifetime of the thread.
        let src = "/proc/self/ns/net";
        mount(
            Some(src),
            &ns_path,
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .map_err(|e| format!("bind mount failed: {}", e))?;

        Ok(())
    })
    .join()
    .map_err(|_| "thread panicked".to_string())?;

    result
}

/// Delete a named network namespace by unmounting and removing the file.
pub(crate) fn delete_netns(ns_path: &Path) -> Result<(), String> {
    // Unmount (lazy detach) the bind-mount
    umount2(ns_path, MntFlags::MNT_DETACH).map_err(|e| format!("umount2 failed: {}", e))?;

    // Remove the file
    std::fs::remove_file(ns_path).map_err(|e| format!("remove file: {}", e))?;

    Ok(())
}

/// Get the inode number of a path (used as namespace ID).
pub(crate) fn ns_inode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino() as u32
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Namespace file-descriptor helpers
// ---------------------------------------------------------------------------

/// Open a namespace file descriptor (read-only, close-on-exec).
fn open_ns_fd(path: &str) -> Result<i32, String> {
    nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|e| format!("open ns fd {}: {}", path, e))
}

/// Close a raw file descriptor.
fn close_fd(fd: i32) {
    let _ = nix::unistd::close(fd);
}

// ---------------------------------------------------------------------------
// Run-in-namespace helper
// ---------------------------------------------------------------------------

/// Run a closure inside the given namespace's network context.
///
/// Spawns a dedicated thread, switches it to the target namespace via
/// `setns()`, runs the closure, then restores the original namespace.
pub(crate) fn run_in_namespace<F, T>(ns_path: &str, f: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let ns_path = ns_path.to_string();

    let result = std::thread::spawn(move || -> Result<T, String> {
        // Save current network namespace
        let orig_ns =
            open_ns_fd("/proc/self/ns/net").map_err(|e| format!("open current ns: {}", e))?;

        // Open target namespace
        let target_ns = open_ns_fd(&ns_path).map_err(|e| format!("open target ns: {}", e))?;

        // Switch to target namespace
        // SAFETY: the fd was just opened and is valid for the lifetime of this scope
        nix::sched::setns(
            unsafe { BorrowedFd::borrow_raw(target_ns) },
            CloneFlags::CLONE_NEWNET,
        )
        .map_err(|e| format!("setns to target: {}", e))?;
        close_fd(target_ns);

        // Run the closure
        let result = f();

        // Restore original namespace
        let restore_result = nix::sched::setns(
            unsafe { BorrowedFd::borrow_raw(orig_ns) },
            CloneFlags::CLONE_NEWNET,
        );
        close_fd(orig_ns);

        if let Err(e) = restore_result {
            // This is serious — the thread is now stuck in the wrong namespace.
            // Best we can do is log and continue (the thread will be destroyed anyway).
            eprintln!("CRITICAL: Failed to restore original namespace: {}", e);
        }

        Ok(result)
    })
    .join()
    .map_err(|_| "namespace thread panicked".to_string())?;

    result
}

// ---------------------------------------------------------------------------
// Link operations (netlink RTM_*LINK)
// ---------------------------------------------------------------------------

/// Dump all links and return their interface names.
pub(crate) fn dump_interface_names(socket: &RawNetlinkSocket) -> Result<Vec<String>, NetlinkError> {
    let responses = dump_links(&socket.inner)?;
    Ok(extract_interface_names(&responses))
}

/// Get the kernel interface index for a given name.
pub(crate) fn get_interface_index(
    socket: &RawNetlinkSocket,
    name: &str,
) -> Result<u32, NetlinkError> {
    let responses = dump_links(&socket.inner)?;
    extract_interface_index(&responses, name)
}

/// Dump all links and return their interface names, using a freshly opened socket.
///
/// Used inside closures that run in a different namespace.
pub(crate) fn dump_interface_names_fresh() -> Result<Vec<String>, String> {
    let socket = RawNetlinkSocket::open().map_err(|e| e.to_string())?;
    let responses = dump_links(&socket.inner).map_err(|e| e.to_string())?;
    Ok(extract_interface_names(&responses))
}

/// Get interface index using a freshly opened socket.
///
/// Used inside closures that run in a different namespace.
#[allow(dead_code)]
pub(crate) fn get_interface_index_fresh(name: &str) -> Result<u32, String> {
    let socket = RawNetlinkSocket::open().map_err(|e| e.to_string())?;
    let responses = dump_links(&socket.inner).map_err(|e| e.to_string())?;
    extract_interface_index(&responses, name).map_err(|e| e.to_string())
}

/// Send a SetLink message to move an interface to a namespace fd.
pub(crate) fn set_link_ns_fd(
    socket: &RawNetlinkSocket,
    ifindex: u32,
    ns_fd: i32,
) -> Result<(), NetlinkError> {
    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.nlas.push(LinkNla::NetNsFd(ns_fd));

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::SetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    netlink_request(&socket.inner, nl_msg)?;
    Ok(())
}

/// Move an interface to a namespace (identified by ns file path).
///
/// Opens the namespace fd, sends the SetLink, closes the fd.
/// Returns `Ok(())` on success.
pub(crate) fn move_interface_to_ns(
    socket: &RawNetlinkSocket,
    ifindex: u32,
    ns_path: &str,
) -> Result<(), String> {
    let ns_fd = open_ns_fd(ns_path)?;
    let result = set_link_ns_fd(socket, ifindex, ns_fd).map_err(|e| e.to_string());
    close_fd(ns_fd);
    result
}

/// Move an interface (inside a namespace) to the default namespace (PID 1).
///
/// Opens a fresh socket, resolves the interface index, opens /proc/1/ns/net,
/// sends the SetLink, closes everything.
pub(crate) fn move_interface_to_default_ns(interface_name: &str) -> Result<(), String> {
    let sock = RawNetlinkSocket::open().map_err(|e| e.to_string())?;
    let ifindex = get_interface_index(&sock, interface_name).map_err(|e| e.to_string())?;

    let default_ns_fd =
        open_ns_fd("/proc/1/ns/net").map_err(|e| format!("open default ns fd: {}", e))?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.nlas.push(LinkNla::NetNsFd(default_ns_fd));

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::SetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    let result = netlink_request(&sock.inner, nl_msg).map_err(|e| e.to_string());
    close_fd(default_ns_fd);
    result?;
    Ok(())
}

/// Create a veth pair.
pub(crate) fn create_veth_pair(
    socket: &RawNetlinkSocket,
    veth_name: &str,
    peer_name: &str,
) -> Result<(), NetlinkError> {
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

    netlink_request(&socket.inner, nl_msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Route operations (netlink RTM_*ROUTE)
// ---------------------------------------------------------------------------

/// Route parameters for building a netlink route message.
///
/// This is a simple struct that mirrors `RouteConfig` but uses parsed network
/// types so the raw module doesn't depend on high-level validation logic.
pub(crate) struct RawRouteParams {
    pub destination: String,
    pub gateway: String,
    pub interface: Option<String>,
    pub metric: Option<u32>,
}

/// Add a route using the given parameters, opening a fresh socket.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn add_route_fresh(params: RawRouteParams) -> Result<(), String> {
    let sock = RawNetlinkSocket::open().map_err(|e| e.to_string())?;

    let mut msg = RouteMessage::default();
    msg.header.table = RT_TABLE_MAIN;
    msg.header.protocol = RTPROT_STATIC;
    msg.header.scope = RT_SCOPE_UNIVERSE;
    msg.header.kind = RTN_UNICAST;
    msg.header.address_family = libc::AF_INET as u8;

    // Destination
    if params.destination == "default" {
        msg.header.destination_prefix_length = 0;
    } else if let Some((ip_str, prefix_len_str)) = params.destination.split_once('/') {
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
        let ip: std::net::Ipv4Addr = params
            .destination
            .parse()
            .map_err(|e| format!("invalid destination IP: {}", e))?;
        msg.nlas.push(RouteNla::Destination(ip.octets().to_vec()));
    }

    // Gateway
    if !params.gateway.is_empty() {
        let gw: std::net::Ipv4Addr = params
            .gateway
            .parse()
            .map_err(|e| format!("invalid gateway IP: {}", e))?;
        msg.nlas.push(RouteNla::Gateway(gw.octets().to_vec()));
    }

    // Metric
    if let Some(metric) = params.metric {
        msg.nlas.push(RouteNla::Priority(metric));
    }

    // Output interface
    if let Some(ref iface) = params.interface {
        let ifindex =
            get_interface_index(&RawNetlinkSocket::open().map_err(|e| e.to_string())?, iface)
                .map_err(|e| e.to_string())?;
        msg.nlas.push(RouteNla::Oif(ifindex));
    }

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::NewRoute(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    netlink_request(&sock.inner, nl_msg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Dump all routes and return them as formatted strings.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn dump_routes_fresh() -> Result<Vec<String>, String> {
    let sock = RawNetlinkSocket::open().map_err(|e| e.to_string())?;

    let mut msg = RouteMessage::default();
    msg.header.address_family = libc::AF_INET as u8;

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetRoute(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    let responses = netlink_request(&sock.inner, nl_msg).map_err(|e| e.to_string())?;

    let mut routes = Vec::new();
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RtnlMessage::NewRoute(route_msg)) = resp.payload {
            routes.push(format_route(&route_msg));
        }
    }
    Ok(routes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a link dump request and return the raw responses.
fn dump_links(socket: &Socket) -> Result<Vec<NetlinkMessage<RtnlMessage>>, NetlinkError> {
    let mut msg = LinkMessage::default();
    // AF_UNSPEC = 0 — list all families
    msg.header.interface_family = 0;

    let mut nl_msg = NetlinkMessage::from(RtnlMessage::GetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    nl_msg.header.sequence_number = next_seq();
    nl_msg.finalize();

    netlink_request(socket, nl_msg)
}

/// Extract interface names from a link dump response.
fn extract_interface_names(responses: &[NetlinkMessage<RtnlMessage>]) -> Vec<String> {
    let mut names = Vec::new();
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = &resp.payload {
            for nla in &link.nlas {
                if let LinkNla::IfName(ref name) = nla {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

/// Extract the interface index for a given name from link dump responses.
fn extract_interface_index(
    responses: &[NetlinkMessage<RtnlMessage>],
    name: &str,
) -> Result<u32, NetlinkError> {
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RtnlMessage::NewLink(link)) = &resp.payload {
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
pub(crate) fn format_route(route: &RouteMessage) -> String {
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
}
