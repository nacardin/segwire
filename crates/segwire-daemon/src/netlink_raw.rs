//! Low-level netlink socket helpers.
//!
//! This module owns **all** netlink protocol interactions via `netlink-tpc`'s
//! high-level `NetlinkSocket` API:
//! - Async operations use the shared `NetlinkSocket` held by `NetlinkManager`
//! - Sync operations (inside namespace thread closures) spin up a thread-local
//!   monoio runtime and open a fresh `NetlinkSocket`
//!
//! Namespace lifecycle syscalls (`unshare`, `setns`, `mount`, etc.) live in the
//! companion [`crate::netns_raw`] module.  This file contains **zero** `nix`
//! imports and **zero** manual netlink serialization — everything goes through
//! `NetlinkSocket::request()`.

use segwire_common::netlink::NetlinkError;
use crate::netns_raw;
use netlink_packet_core::{
    NetlinkMessage, NetlinkPayload, NLM_F_ACK, NLM_F_CREATE, NLM_F_DUMP, NLM_F_EXCL, NLM_F_REQUEST,
};
use netlink_packet_route::{
    link::{
        InfoData, InfoKind, InfoVeth, LinkAttribute, LinkInfo, LinkMessage,
    },
    route::{
        RouteAddress, RouteAttribute, RouteHeader, RouteMessage,
        RouteProtocol, RouteScope, RouteType,
    },
    AddressFamily, RouteNetlinkMessage,
};
use netlink_tpc::{NetlinkProtocol, NetlinkSocket};
use std::net::Ipv4Addr;

// ---------------------------------------------------------------------------
// Sync netlink socket (for use inside namespace thread closures)
// ---------------------------------------------------------------------------

/// Send a netlink message and collect all response messages using a fresh
/// `NetlinkSocket` inside a thread-local monoio runtime.
fn sync_netlink_request(
    msg: NetlinkMessage<RouteNetlinkMessage>,
) -> Result<Vec<NetlinkMessage<RouteNetlinkMessage>>, NetlinkError> {
    let mut socket = NetlinkSocket::open(NetlinkProtocol::Route)
        .map_err(|e| NetlinkError::SocketError(format!("socket creation failed: {}", e)))?;

    let mut rt = monoio::RuntimeBuilder::<monoio::FusionDriver>::new()
        .build()
        .map_err(|e| NetlinkError::SocketError(format!("monoio runtime failed: {}", e)))?;

    rt.block_on(async {
        socket.request(msg).await.map_err(|e| {
            NetlinkError::ProtocolError(format!("netlink request: {}", e))
        })
    })
}

// ---------------------------------------------------------------------------
// Link operations — sync (for use inside namespace thread closures)
// ---------------------------------------------------------------------------

/// Dump all links and return their interface names, using a fresh socket.
///
/// Used inside closures that run in a different namespace.
pub(crate) fn dump_interface_names_fresh() -> Result<Vec<String>, String> {
    let responses = dump_links_sync().map_err(|e| e.to_string())?;
    Ok(extract_interface_names(&responses))
}

/// Get interface index using a fresh socket.
///
/// Used inside closures that run in a different namespace.
#[allow(dead_code)]
pub(crate) fn get_interface_index_fresh(name: &str) -> Result<u32, String> {
    let responses = dump_links_sync().map_err(|e| e.to_string())?;
    extract_interface_index(&responses, name).map_err(|e| e.to_string())
}

/// Move an interface (inside a namespace) to the default namespace (PID 1).
///
/// Opens a fresh socket, resolves the interface index, opens /proc/1/ns/net,
/// sends the SetLink, closes everything.
pub(crate) fn move_interface_to_default_ns(interface_name: &str) -> Result<(), String> {
    let responses = dump_links_sync().map_err(|e| e.to_string())?;
    let ifindex =
        extract_interface_index(&responses, interface_name).map_err(|e| e.to_string())?;

    let default_ns_fd =
        netns_raw::open_ns_fd("/proc/1/ns/net").map_err(|e| format!("open default ns fd: {}", e))?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.attributes.push(LinkAttribute::NetNsFd(default_ns_fd));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::SetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
    nl_msg.finalize();

    let result = sync_netlink_request(nl_msg).map_err(|e| e.to_string());
    netns_raw::close_fd(default_ns_fd);
    result?;
    Ok(())
}

/// Move an interface to a namespace identified by ns file path (sync).
///
/// Opens the namespace fd, sends SetLink, closes the fd.
pub(crate) fn move_interface_to_ns_sync(ifindex: u32, ns_path: &str) -> Result<(), String> {
    let ns_fd = netns_raw::open_ns_fd(ns_path)?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.attributes.push(LinkAttribute::NetNsFd(ns_fd));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::SetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK;
    nl_msg.finalize();

    let result = sync_netlink_request(nl_msg).map_err(|e| e.to_string());
    netns_raw::close_fd(ns_fd);
    result?;
    Ok(())
}

/// Create a veth pair (sync).
pub(crate) fn create_veth_pair_sync(veth_name: &str, peer_name: &str) -> Result<(), NetlinkError> {
    let mut peer_msg = LinkMessage::default();
    peer_msg
        .attributes
        .push(LinkAttribute::IfName(peer_name.to_string()));

    let mut msg = LinkMessage::default();
    msg.attributes
        .push(LinkAttribute::IfName(veth_name.to_string()));
    msg.attributes.push(LinkAttribute::LinkInfo(vec![
        LinkInfo::Kind(InfoKind::Veth),
        LinkInfo::Data(InfoData::Veth(InfoVeth::Peer(peer_msg))),
    ]));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
    nl_msg.finalize();

    sync_netlink_request(nl_msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Route operations — sync (always run inside namespace thread closures)
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
    let mut msg = RouteMessage::default();
    msg.header.table = RouteHeader::RT_TABLE_MAIN;
    msg.header.protocol = RouteProtocol::Static;
    msg.header.scope = RouteScope::Universe;
    msg.header.kind = RouteType::Unicast;
    msg.header.address_family = AddressFamily::Inet;

    // Destination
    if params.destination == "default" {
        msg.header.destination_prefix_length = 0;
    } else if let Some((ip_str, prefix_len_str)) = params.destination.split_once('/') {
        let prefix_len: u8 = prefix_len_str
            .parse()
            .map_err(|e| format!("invalid prefix length: {}", e))?;
        msg.header.destination_prefix_length = prefix_len;

        let ip: Ipv4Addr = ip_str
            .parse()
            .map_err(|e| format!("invalid destination IP: {}", e))?;
        msg.attributes
            .push(RouteAttribute::Destination(RouteAddress::Inet(ip)));
    } else {
        // Single host route
        msg.header.destination_prefix_length = 32;
        let ip: Ipv4Addr = params
            .destination
            .parse()
            .map_err(|e| format!("invalid destination IP: {}", e))?;
        msg.attributes
            .push(RouteAttribute::Destination(RouteAddress::Inet(ip)));
    }

    // Gateway
    if !params.gateway.is_empty() {
        let gw: Ipv4Addr = params
            .gateway
            .parse()
            .map_err(|e| format!("invalid gateway IP: {}", e))?;
        msg.attributes
            .push(RouteAttribute::Gateway(RouteAddress::Inet(gw)));
    }

    // Metric
    if let Some(metric) = params.metric {
        msg.attributes.push(RouteAttribute::Priority(metric));
    }

    // Output interface
    if let Some(ref iface) = params.interface {
        let responses = dump_links_sync().map_err(|e| e.to_string())?;
        let ifindex = extract_interface_index(&responses, iface).map_err(|e| e.to_string())?;
        msg.attributes.push(RouteAttribute::Oif(ifindex));
    }

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewRoute(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
    nl_msg.finalize();

    sync_netlink_request(nl_msg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Dump all routes and return them as formatted strings.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn dump_routes_fresh() -> Result<Vec<String>, String> {
    let mut msg = RouteMessage::default();
    msg.header.address_family = AddressFamily::Inet;

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::GetRoute(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    nl_msg.finalize();

    let responses = sync_netlink_request(nl_msg).map_err(|e| e.to_string())?;

    let mut routes = Vec::new();
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(route_msg)) =
            resp.payload
        {
            routes.push(format_route(&route_msg));
        }
    }
    Ok(routes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------


/// Send a link dump request via a fresh socket with a thread-local runtime.
fn dump_links_sync() -> Result<Vec<NetlinkMessage<RouteNetlinkMessage>>, NetlinkError> {
    let msg = LinkMessage::default();

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::GetLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    nl_msg.finalize();

    sync_netlink_request(nl_msg)
}

/// Extract interface names from a link dump response.
fn extract_interface_names(
    responses: &[NetlinkMessage<RouteNetlinkMessage>],
) -> Vec<String> {
    let mut names = Vec::new();
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(link)) = &resp.payload {
            for attr in &link.attributes {
                if let LinkAttribute::IfName(ref name) = attr {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

/// Extract the interface index for a given name from link dump responses.
fn extract_interface_index(
    responses: &[NetlinkMessage<RouteNetlinkMessage>],
    name: &str,
) -> Result<u32, NetlinkError> {
    for resp in responses {
        if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(link)) = &resp.payload {
            for attr in &link.attributes {
                if let LinkAttribute::IfName(ref n) = attr {
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

    for attr in &route.attributes {
        match attr {
            RouteAttribute::Destination(addr) => {
                match addr {
                    RouteAddress::Inet(ip) => {
                        dest = format!("{}/{}", ip, route.header.destination_prefix_length);
                    }
                    RouteAddress::Inet6(ip) => {
                        dest = format!("{}/{}", ip, route.header.destination_prefix_length);
                    }
                    _ => {}
                }
            }
            RouteAttribute::Gateway(addr) => {
                match addr {
                    RouteAddress::Inet(ip) => {
                        gateway = format!("via {}", ip);
                    }
                    RouteAddress::Inet6(ip) => {
                        gateway = format!("via {}", ip);
                    }
                    _ => {}
                }
            }
            RouteAttribute::Oif(idx) => {
                oif = *idx;
            }
            RouteAttribute::Priority(metric) => {
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
        msg.attributes.push(RouteAttribute::Destination(
            RouteAddress::Inet(Ipv4Addr::new(192, 168, 1, 0)),
        ));
        msg.attributes.push(RouteAttribute::Gateway(
            RouteAddress::Inet(Ipv4Addr::new(10, 0, 0, 1)),
        ));
        msg.attributes.push(RouteAttribute::Priority(100));

        let formatted = format_route(&msg);
        assert!(formatted.contains("192.168.1.0/24"));
        assert!(formatted.contains("via 10.0.0.1"));
        assert!(formatted.contains("metric 100"));
    }
}
