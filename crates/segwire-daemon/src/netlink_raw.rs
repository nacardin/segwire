//! Route Netlink operations for network namespace management.
//!
//! This module builds and sends typed Route Netlink messages via
//! [`segwire_netlink::NetlinkSocket`].  Each function opens a fresh socket,
//! which means it captures the calling thread's current network namespace.
//!
//! Namespace lifecycle syscalls (`unshare`, `setns`, `mount`, etc.) live in the
//! companion [`crate::netns_raw`] module.

use crate::netns_raw;
use netlink_packet_core::{
    NetlinkMessage, NetlinkPayload, NLM_F_ACK, NLM_F_CREATE, NLM_F_DUMP, NLM_F_EXCL,
};
use netlink_packet_route::{
    address::{AddressAttribute, AddressMessage},
    link::{InfoData, InfoKind, InfoVeth, LinkAttribute, LinkFlags, LinkInfo, LinkMessage},
    route::{
        RouteAddress, RouteAttribute, RouteHeader, RouteMessage, RouteProtocol, RouteScope,
        RouteType,
    },
    AddressFamily, RouteNetlinkMessage,
};
use segwire_common::netlink::NetlinkError;
use segwire_netlink::{NetlinkProtocol, NetlinkSocket};
use std::net::IpAddr;

// ---------------------------------------------------------------------------
// Link operations
// ---------------------------------------------------------------------------

/// Dump all links and return their interface names, using a fresh socket.
///
/// Used inside closures that run in a different namespace.
pub(crate) fn dump_interface_names_fresh() -> Result<Vec<String>, String> {
    let responses = dump_links().map_err(|e| e.to_string())?;
    Ok(extract_interface_names(&responses))
}

/// Get interface index using a fresh socket.
///
/// Used inside closures that run in a different namespace.
#[allow(dead_code)]
pub(crate) fn get_interface_index_fresh(name: &str) -> Result<u32, String> {
    let responses = dump_links().map_err(|e| e.to_string())?;
    extract_interface_index(&responses, name).map_err(|e| e.to_string())
}

/// Move an interface (inside a namespace) to the default namespace (PID 1).
///
/// Opens a fresh socket, resolves the interface index, opens /proc/1/ns/net,
/// sends the SetLink, closes everything.
pub(crate) fn move_interface_to_default_ns(interface_name: &str) -> Result<(), String> {
    let responses = dump_links().map_err(|e| e.to_string())?;
    let ifindex = extract_interface_index(&responses, interface_name).map_err(|e| e.to_string())?;

    let default_ns_fd = netns_raw::open_ns_fd("/proc/1/ns/net")
        .map_err(|e| format!("open default ns fd: {}", e))?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.attributes.push(LinkAttribute::NetNsFd(default_ns_fd));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::SetLink(msg));
    nl_msg.header.flags |= NLM_F_ACK;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    let result = sock.request(nl_msg).map_err(|e| e.to_string());
    netns_raw::close_fd(default_ns_fd);
    result?;
    Ok(())
}

/// Move an interface to a namespace identified by ns file path (sync).
///
/// Opens the namespace fd, sends SetLink, closes the fd.
pub(crate) fn move_interface_to_ns(ifindex: u32, ns_path: &str) -> Result<(), String> {
    let ns_fd = netns_raw::open_ns_fd(ns_path)?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.attributes.push(LinkAttribute::NetNsFd(ns_fd));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::SetLink(msg));
    nl_msg.header.flags |= NLM_F_ACK;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    let result = sock.request(nl_msg).map_err(|e| e.to_string());
    netns_raw::close_fd(ns_fd);
    result?;
    Ok(())
}

/// Create a veth pair (sync).
pub(crate) fn create_veth_pair(veth_name: &str, peer_name: &str) -> Result<(), NetlinkError> {
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
    nl_msg.header.flags |= NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route)?;
    sock.request(nl_msg)?;
    Ok(())
}

/// Create a generic virtual interface (dummy, bridge, macvlan, ipvlan) (sync).
pub(crate) fn create_virtual_interface(
    name: &str,
    kind_str: &str,
) -> Result<(), NetlinkError> {
    let kind = match kind_str {
        "dummy" => InfoKind::Dummy,
        "bridge" => InfoKind::Bridge,
        "macvlan" => InfoKind::MacVlan,
        "ipvlan" => InfoKind::IpVlan,
        _ => {
            return Err(NetlinkError::ProtocolError(format!(
                "unsupported virtual interface type: {}",
                kind_str
            )))
        }
    };

    let mut msg = LinkMessage::default();
    msg.attributes.push(LinkAttribute::IfName(name.to_string()));
    msg.attributes
        .push(LinkAttribute::LinkInfo(vec![LinkInfo::Kind(kind)]));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewLink(msg));
    nl_msg.header.flags |= NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route)?;
    sock.request(nl_msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Route operations
// ---------------------------------------------------------------------------

/// Route parameters for building a netlink route message.
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

    let mut detected_family: Option<AddressFamily> = None;

    fn ip_to_route_addr(ip: IpAddr) -> RouteAddress {
        match ip {
            IpAddr::V4(v4) => RouteAddress::Inet(v4),
            IpAddr::V6(v6) => RouteAddress::Inet6(v6),
        }
    }
    fn family_for(ip: &IpAddr) -> AddressFamily {
        match ip {
            IpAddr::V4(_) => AddressFamily::Inet,
            IpAddr::V6(_) => AddressFamily::Inet6,
        }
    }

    // Destination
    if params.destination == "default" {
        msg.header.destination_prefix_length = 0;
    } else if let Some((ip_str, prefix_len_str)) = params.destination.split_once('/') {
        let prefix_len: u8 = prefix_len_str
            .parse()
            .map_err(|e| format!("invalid prefix length: {}", e))?;
        msg.header.destination_prefix_length = prefix_len;

        let ip: IpAddr = ip_str
            .parse()
            .map_err(|e| format!("invalid destination IP: {}", e))?;
        detected_family = Some(family_for(&ip));
        msg.attributes
            .push(RouteAttribute::Destination(ip_to_route_addr(ip)));
    } else {
        let ip: IpAddr = params
            .destination
            .parse()
            .map_err(|e| format!("invalid destination IP: {}", e))?;
        detected_family = Some(family_for(&ip));
        msg.header.destination_prefix_length = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        msg.attributes
            .push(RouteAttribute::Destination(ip_to_route_addr(ip)));
    }

    // Gateway
    if !params.gateway.is_empty() {
        let gw: IpAddr = params
            .gateway
            .parse()
            .map_err(|e| format!("invalid gateway IP: {}", e))?;
        if detected_family.is_none() {
            detected_family = Some(family_for(&gw));
        }
        msg.attributes
            .push(RouteAttribute::Gateway(ip_to_route_addr(gw)));
    }

    msg.header.address_family = detected_family.unwrap_or(AddressFamily::Inet);

    // Metric
    if let Some(metric) = params.metric {
        msg.attributes.push(RouteAttribute::Priority(metric));
    }

    // Output interface
    if let Some(ref iface) = params.interface {
        let responses = dump_links().map_err(|e| e.to_string())?;
        let ifindex = extract_interface_index(&responses, iface).map_err(|e| e.to_string())?;
        msg.attributes.push(RouteAttribute::Oif(ifindex));
    }

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewRoute(msg));
    nl_msg.header.flags |= NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    sock.request(nl_msg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Dump all routes (IPv4 and IPv6) and return them as formatted strings.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn dump_routes_fresh() -> Result<Vec<String>, String> {
    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    let mut routes = Vec::new();

    for family in [AddressFamily::Inet, AddressFamily::Inet6] {
        let mut msg = RouteMessage::default();
        msg.header.address_family = family;

        let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::GetRoute(msg));
        nl_msg.header.flags |= NLM_F_DUMP;

        let responses = sock.request(nl_msg).map_err(|e| e.to_string())?;

        for resp in responses {
            if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(route_msg)) =
                resp.payload
            {
                routes.push(format_route(&route_msg));
            }
        }
    }
    Ok(routes)
}

// ---------------------------------------------------------------------------
// Address operations
// ---------------------------------------------------------------------------

/// Add an IP address (IPv4 or IPv6) to an interface, opening a fresh socket.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn add_address_fresh(ifname: &str, addr: IpAddr, prefix_len: u8) -> Result<(), String> {
    let responses = dump_links().map_err(|e| e.to_string())?;
    let ifindex = extract_interface_index(&responses, ifname).map_err(|e| e.to_string())?;

    let family = match addr {
        IpAddr::V4(_) => AddressFamily::Inet,
        IpAddr::V6(_) => AddressFamily::Inet6,
    };

    let mut msg = AddressMessage::default();
    msg.header.family = family;
    msg.header.prefix_len = prefix_len;
    msg.header.index = ifindex;
    msg.attributes.push(AddressAttribute::Local(addr));
    msg.attributes.push(AddressAttribute::Address(addr));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewAddress(msg));
    nl_msg.header.flags |= NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    sock.request(nl_msg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Set a network interface to UP state, opening a fresh socket.
///
/// Designed to be called inside a namespace closure.
pub(crate) fn set_link_up_fresh(ifname: &str) -> Result<(), String> {
    let responses = dump_links().map_err(|e| e.to_string())?;
    let ifindex = extract_interface_index(&responses, ifname).map_err(|e| e.to_string())?;

    let mut msg = LinkMessage::default();
    msg.header.index = ifindex;
    msg.header.flags = LinkFlags::Up;
    msg.header.change_mask = LinkFlags::Up;

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::SetLink(msg));
    nl_msg.header.flags |= NLM_F_ACK;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).map_err(|e| e.to_string())?;
    sock.request(nl_msg).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a link dump request via a fresh socket.
fn dump_links() -> Result<Vec<NetlinkMessage<RouteNetlinkMessage>>, NetlinkError> {
    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route)?;

    let mut nl_msg =
        NetlinkMessage::from(RouteNetlinkMessage::GetLink(LinkMessage::default()));
    nl_msg.header.flags |= NLM_F_DUMP;

    sock.request(nl_msg)
}

/// Extract interface names from a link dump response.
fn extract_interface_names(responses: &[NetlinkMessage<RouteNetlinkMessage>]) -> Vec<String> {
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
            RouteAttribute::Destination(addr) => match addr {
                RouteAddress::Inet(ip) => {
                    dest = format!("{}/{}", ip, route.header.destination_prefix_length);
                }
                RouteAddress::Inet6(ip) => {
                    dest = format!("{}/{}", ip, route.header.destination_prefix_length);
                }
                _ => {}
            },
            RouteAttribute::Gateway(addr) => match addr {
                RouteAddress::Inet(ip) => {
                    gateway = format!("via {}", ip);
                }
                RouteAddress::Inet6(ip) => {
                    gateway = format!("via {}", ip);
                }
                _ => {}
            },
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
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_format_route() {
        let mut msg = RouteMessage::default();
        msg.header.destination_prefix_length = 24;
        msg.attributes
            .push(RouteAttribute::Destination(RouteAddress::Inet(
                Ipv4Addr::new(192, 168, 1, 0),
            )));
        msg.attributes
            .push(RouteAttribute::Gateway(RouteAddress::Inet(Ipv4Addr::new(
                10, 0, 0, 1,
            ))));
        msg.attributes.push(RouteAttribute::Priority(100));

        let formatted = format_route(&msg);
        assert!(formatted.contains("192.168.1.0/24"));
        assert!(formatted.contains("via 10.0.0.1"));
        assert!(formatted.contains("metric 100"));
    }

    #[test]
    fn test_format_route_ipv6() {
        let mut msg = RouteMessage::default();
        msg.header.destination_prefix_length = 64;
        msg.attributes
            .push(RouteAttribute::Destination(RouteAddress::Inet6(
                Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0),
            )));
        msg.attributes
            .push(RouteAttribute::Gateway(RouteAddress::Inet6(Ipv6Addr::new(
                0xfe80, 0, 0, 0, 0, 0, 0, 1,
            ))));
        msg.attributes.push(RouteAttribute::Priority(200));

        let formatted = format_route(&msg);
        assert!(formatted.contains("fd00::/64"));
        assert!(formatted.contains("via fe80::1"));
        assert!(formatted.contains("metric 200"));
    }
}
