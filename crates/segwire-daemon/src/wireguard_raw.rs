//! WireGuard Generic Netlink operations.
//!
//! This module handles all WireGuard-specific kernel interactions:
//! - Creating WireGuard interfaces (via Route Netlink, `InfoKind::Wireguard`)
//! - Configuring WireGuard devices (via Generic Netlink, `netlink-packet-wireguard`)
//!
//! Interface creation reuses the Route Netlink socket pattern from [`crate::netlink_raw`]
//! (via [`segwire_netlink::sync_netlink_request`]).
//! WireGuard configuration (keys, peers, endpoints) uses the separate Generic Netlink
//! protocol family via [`segwire_netlink::resolve_genl_family_id`] and
//! [`segwire_netlink::send_genl_request`].

use base64::Engine;
use netlink_packet_core::{
    Emitable, NetlinkMessage, NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL,
};
use netlink_packet_route::link::{InfoKind, LinkAttribute, LinkInfo, LinkMessage};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_wireguard::{
    WireguardAllowedIp, WireguardAttribute, WireguardCmd, WireguardMessage, WireguardPeer,
    WireguardPeerAttribute,
};
use segwire_common::config::{WireguardConfig, WireguardPeerConfig};
use segwire_common::netlink::NetlinkError;
use std::net::IpAddr;

// ---------------------------------------------------------------------------
// WireGuard interface creation (via Route Netlink — same as other interfaces)
// ---------------------------------------------------------------------------

/// Create a WireGuard network interface (sync, via Route Netlink).
///
/// This only creates the bare interface — WireGuard-specific configuration
/// (keys, peers, endpoints) must be applied separately via
/// [`configure_wireguard_device`].
pub(crate) fn create_wireguard_interface(name: &str) -> Result<(), NetlinkError> {
    use segwire_netlink::{NetlinkProtocol, NetlinkSocket};

    let mut msg = LinkMessage::default();
    msg.attributes
        .push(LinkAttribute::IfName(name.to_string()));
    msg.attributes
        .push(LinkAttribute::LinkInfo(vec![LinkInfo::Kind(
            InfoKind::Wireguard,
        )]));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewLink(msg));
    nl_msg.header.flags |= NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;

    let mut sock = NetlinkSocket::open(NetlinkProtocol::Route)
        .map_err(|e| NetlinkError::VirtualInterfaceCreateFailed(name.to_string(), e.to_string()))?;
    sock.request(nl_msg)
        .map_err(|e| NetlinkError::VirtualInterfaceCreateFailed(name.to_string(), e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// WireGuard device configuration (via Generic Netlink)
// ---------------------------------------------------------------------------

/// Configure a WireGuard device with private key, listen port, fwmark, and peers.
///
/// The interface must already exist. This function sends a `SetDevice` Generic
/// Netlink message with the full device configuration.
///
/// Uses [`segwire_netlink::resolve_genl_family_id`] for family ID resolution
/// and [`segwire_netlink::send_genl_request`] for the actual request.
/// The WireGuard payload is serialized via its `Emitable` impl from
/// `netlink-packet-wireguard`.
///
/// Designed to be called inside a namespace closure (via `run_in_namespace`).
pub(crate) fn configure_wireguard_device(
    ifname: &str,
    config: &WireguardConfig,
) -> Result<(), String> {
    let family_id = segwire_netlink::resolve_genl_family_id("wireguard")
        .map_err(|e| e.to_string())?;

    // Decode private key
    let private_key_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&config.private_key)
        .map_err(|e| format!("invalid private key base64: {}", e))?
        .try_into()
        .map_err(|v: Vec<u8>| format!("private key must be 32 bytes, got {}", v.len()))?;

    // Build device attributes
    let mut attrs = vec![
        WireguardAttribute::IfName(ifname.to_string()),
        WireguardAttribute::PrivateKey(private_key_bytes),
    ];

    if config.listen_port != 0 {
        attrs.push(WireguardAttribute::ListenPort(config.listen_port));
    }

    if config.fwmark != 0 {
        attrs.push(WireguardAttribute::Fwmark(config.fwmark));
    }

    // Build peer list
    if !config.peers.is_empty() {
        let wg_peers: Vec<WireguardPeer> = config
            .peers
            .iter()
            .map(build_wireguard_peer)
            .collect::<Result<Vec<_>, _>>()?;

        attrs.push(WireguardAttribute::Peers(wg_peers));
    }

    // Build WireGuard message payload
    let wg_msg = WireguardMessage {
        cmd: WireguardCmd::SetDevice,
        attributes: attrs,
    };

    // Serialize the WireGuard payload and send via generic netlink
    let mut payload_buf = vec![0u8; wg_msg.buffer_len()];
    wg_msg.emit(&mut payload_buf);

    let cmd = match wg_msg.cmd {
        WireguardCmd::GetDevice => 0,
        WireguardCmd::SetDevice => 1,
        _ => 0,
    };

    segwire_netlink::send_genl_request(family_id, cmd, 1, &payload_buf)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Build a `WireguardPeer` from our config struct.
fn build_wireguard_peer(peer_cfg: &WireguardPeerConfig) -> Result<WireguardPeer, String> {
    let public_key_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&peer_cfg.public_key)
        .map_err(|e| format!("invalid peer public key base64: {}", e))?
        .try_into()
        .map_err(|v: Vec<u8>| format!("peer public key must be 32 bytes, got {}", v.len()))?;

    let mut attrs = vec![WireguardPeerAttribute::PublicKey(public_key_bytes)];

    if let Some(ref psk) = peer_cfg.preshared_key {
        let psk_bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(psk)
            .map_err(|e| format!("invalid preshared key base64: {}", e))?
            .try_into()
            .map_err(|v: Vec<u8>| format!("preshared key must be 32 bytes, got {}", v.len()))?;
        attrs.push(WireguardPeerAttribute::PresharedKey(psk_bytes));
    }

    if let Some(ref endpoint) = peer_cfg.endpoint {
        let sock_addr: std::net::SocketAddr = endpoint
            .parse()
            .map_err(|e| format!("invalid endpoint '{}': {}", endpoint, e))?;
        attrs.push(WireguardPeerAttribute::Endpoint(sock_addr));
    }

    if peer_cfg.persistent_keepalive != 0 {
        attrs.push(WireguardPeerAttribute::PersistentKeepalive(
            peer_cfg.persistent_keepalive,
        ));
    }

    if !peer_cfg.allowed_ips.is_empty() {
        let allowed: Vec<WireguardAllowedIp> = peer_cfg
            .allowed_ips
            .iter()
            .map(|cidr| parse_allowed_ip(cidr))
            .collect::<Result<Vec<_>, _>>()?;
        attrs.push(WireguardPeerAttribute::AllowedIps(allowed));
    }

    Ok(WireguardPeer(attrs))
}

/// Parse a CIDR string into a `WireguardAllowedIp`.
fn parse_allowed_ip(cidr: &str) -> Result<WireguardAllowedIp, String> {
    use netlink_packet_wireguard::WireguardAllowedIpAttr;

    let (ip_str, prefix_str) = cidr
        .split_once('/')
        .ok_or_else(|| format!("allowed_ip '{}' must be in CIDR notation", cidr))?;

    let ip: IpAddr = ip_str
        .parse()
        .map_err(|e| format!("invalid IP in allowed_ip '{}': {}", cidr, e))?;

    let prefix_len: u8 = prefix_str
        .parse()
        .map_err(|e| format!("invalid prefix length in '{}': {}", cidr, e))?;

    let family = match ip {
        IpAddr::V4(_) => netlink_packet_wireguard::WireguardAddressFamily::Ipv4,
        IpAddr::V6(_) => netlink_packet_wireguard::WireguardAddressFamily::Ipv6,
    };

    Ok(WireguardAllowedIp(vec![
        WireguardAllowedIpAttr::Family(family),
        WireguardAllowedIpAttr::IpAddr(ip),
        WireguardAllowedIpAttr::Cidr(prefix_len),
    ]))
}
