//! WireGuard Generic Netlink operations.
//!
//! This module handles all WireGuard-specific kernel interactions:
//! - Creating WireGuard interfaces (via Route Netlink, `InfoKind::Wireguard`)
//! - Configuring WireGuard devices (via Generic Netlink, `netlink-packet-wireguard`)
//!
//! Interface creation reuses the Route Netlink socket pattern from [`crate::netlink_raw`].
//! WireGuard configuration (keys, peers, endpoints) uses the separate Generic Netlink
//! protocol family, which requires resolving the "wireguard" family ID at runtime.
//!
//! Both the family-ID resolution and the SetDevice command are serialized using
//! raw bytes to avoid version conflicts between `netlink-packet-generic` 0.3.x
//! (pulled by `netlink-packet-route`) and 0.4.x (pulled by `netlink-packet-wireguard`).

use base64::Engine;
use netlink_packet_core::{
    Emitable, NetlinkMessage, NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL, NLM_F_REQUEST,
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
pub(crate) fn create_wireguard_interface_sync(name: &str) -> Result<(), NetlinkError> {
    let mut msg = LinkMessage::default();
    msg.attributes
        .push(LinkAttribute::IfName(name.to_string()));
    msg.attributes
        .push(LinkAttribute::LinkInfo(vec![LinkInfo::Kind(
            InfoKind::Wireguard,
        )]));

    let mut nl_msg = NetlinkMessage::from(RouteNetlinkMessage::NewLink(msg));
    nl_msg.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
    nl_msg.finalize();

    crate::netlink_raw::sync_netlink_request(nl_msg)
        .map_err(|e| NetlinkError::VirtualInterfaceCreateFailed(name.to_string(), e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// WireGuard device configuration (via Generic Netlink)
// ---------------------------------------------------------------------------

/// Resolve the "wireguard" Generic Netlink family ID by querying the kernel.
///
/// Uses raw byte construction for CTRL_CMD_GETFAMILY to avoid version
/// conflicts between netlink-packet-generic 0.3.x (ctrl module) and 0.4.x
/// (used by netlink-packet-wireguard).
fn resolve_wireguard_family_id() -> Result<u16, NetlinkError> {
    use netlink_sys::{protocols::NETLINK_GENERIC, Socket, SocketAddr};

    let mut socket = Socket::new(NETLINK_GENERIC)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink socket: {}", e)))?;
    socket
        .bind_auto()
        .map_err(|e| NetlinkError::SocketError(format!("genetlink bind: {}", e)))?;
    socket
        .connect(&SocketAddr::new(0, 0))
        .map_err(|e| NetlinkError::SocketError(format!("genetlink connect: {}", e)))?;

    // Build CTRL_CMD_GETFAMILY raw message.
    //
    // Layout:
    //   [16 bytes] Netlink header: length, type=GENL_ID_CTRL(0x10), flags=NLM_F_REQUEST, seq, pid
    //   [ 4 bytes] GenL header:    cmd=CTRL_CMD_GETFAMILY(3), version(1), reserved(0)
    //   [NLA]      CTRL_ATTR_FAMILY_NAME(2) = "wireguard\0"
    const GENL_ID_CTRL: u16 = 0x10;
    const CTRL_CMD_GETFAMILY: u8 = 3;
    const CTRL_ATTR_FAMILY_NAME: u16 = 2;

    let family_name = b"wireguard\0";
    let nla_payload_len = family_name.len();
    let nla_len = 4 + nla_payload_len;
    let nla_padded = (nla_len + 3) & !3;

    let genl_hdr_len = 4;
    let nlmsg_len = 16 + genl_hdr_len + nla_padded;

    let mut buf = vec![0u8; nlmsg_len];

    // Netlink header
    buf[0..4].copy_from_slice(&(nlmsg_len as u32).to_ne_bytes());
    buf[4..6].copy_from_slice(&GENL_ID_CTRL.to_ne_bytes());
    buf[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());

    // GenL header
    buf[16] = CTRL_CMD_GETFAMILY;
    buf[17] = 1; // version

    // NLA: CTRL_ATTR_FAMILY_NAME
    let nla_start = 20;
    buf[nla_start..nla_start + 2].copy_from_slice(&(nla_len as u16).to_ne_bytes());
    buf[nla_start + 2..nla_start + 4].copy_from_slice(&CTRL_ATTR_FAMILY_NAME.to_ne_bytes());
    buf[nla_start + 4..nla_start + 4 + nla_payload_len].copy_from_slice(family_name);

    socket
        .send(&buf, 0)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink send: {}", e)))?;

    let mut recv_buf = vec![0u8; 4096];
    let n = socket
        .recv(&mut recv_buf, 0)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink recv: {}", e)))?;

    if n < 20 {
        return Err(NetlinkError::ProtocolError("response too short".to_string()));
    }

    // Check for netlink error (nlmsg_type == NLMSG_ERROR = 2)
    let nlmsg_type = u16::from_ne_bytes([recv_buf[4], recv_buf[5]]);
    if nlmsg_type == 2 {
        if n >= 24 {
            let errno =
                i32::from_ne_bytes([recv_buf[16], recv_buf[17], recv_buf[18], recv_buf[19]]);
            if errno != 0 {
                return Err(NetlinkError::ProtocolError(format!(
                    "GETFAMILY error: errno {} (is the wireguard kernel module loaded?)",
                    errno
                )));
            }
        }
        return Err(NetlinkError::ProtocolError(
            "unexpected ACK for GETFAMILY".to_string(),
        ));
    }

    // Parse NLAs after genl header looking for CTRL_ATTR_FAMILY_ID (1) = u16
    const CTRL_ATTR_FAMILY_ID: u16 = 1;
    let mut offset = 20;
    while offset + 4 <= n {
        let nla_len = u16::from_ne_bytes([recv_buf[offset], recv_buf[offset + 1]]) as usize;
        let nla_type = u16::from_ne_bytes([recv_buf[offset + 2], recv_buf[offset + 3]]);

        if nla_len < 4 {
            break;
        }

        if nla_type == CTRL_ATTR_FAMILY_ID && nla_len >= 6 {
            let family_id = u16::from_ne_bytes([recv_buf[offset + 4], recv_buf[offset + 5]]);
            return Ok(family_id);
        }

        offset += (nla_len + 3) & !3;
    }

    Err(NetlinkError::ProtocolError(
        "wireguard family ID not found in GETFAMILY response".to_string(),
    ))
}

/// Configure a WireGuard device with private key, listen port, fwmark, and peers.
///
/// The interface must already exist. This function sends a `SetDevice` Generic
/// Netlink message with the full device configuration.
///
/// Uses raw byte serialization for the netlink/genl headers to avoid the
/// `netlink-packet-generic` 0.3.x/0.4.x version conflict. The WireGuard
/// payload is serialized via its `Emitable` impl from `netlink-packet-wireguard`.
///
/// Designed to be called inside a namespace closure (via `run_in_namespace`).
pub(crate) fn configure_wireguard_device(
    ifname: &str,
    config: &WireguardConfig,
) -> Result<(), String> {
    let family_id = resolve_wireguard_family_id().map_err(|e| e.to_string())?;

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

    send_wireguard_request(family_id, &wg_msg).map_err(|e| e.to_string())?;

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

/// Send a WireGuard Generic Netlink message with raw byte serialization.
///
/// Manually constructs the netlink header + genl header, then serializes
/// the WireGuard payload using its `Emitable` trait from netlink-packet-wireguard.
/// This bypasses the `GenlMessage` wrapper, avoiding the version conflict.
fn send_wireguard_request(
    family_id: u16,
    wg_msg: &WireguardMessage,
) -> Result<(), NetlinkError> {
    use netlink_sys::{protocols::NETLINK_GENERIC, Socket, SocketAddr};

    let mut socket = Socket::new(NETLINK_GENERIC)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink socket: {}", e)))?;
    socket
        .bind_auto()
        .map_err(|e| NetlinkError::SocketError(format!("genetlink bind: {}", e)))?;
    socket
        .connect(&SocketAddr::new(0, 0))
        .map_err(|e| NetlinkError::SocketError(format!("genetlink connect: {}", e)))?;

    // Layout:
    //   [16 bytes] Netlink header
    //   [ 4 bytes] GenL header (cmd, version, reserved)
    //   [variable] WireGuard payload (attributes serialized by Emitable)
    let payload_len = wg_msg.buffer_len();
    let genl_hdr_len = 4;
    let nlmsg_len = 16 + genl_hdr_len + payload_len;

    let mut buf = vec![0u8; nlmsg_len];

    // Netlink header
    buf[0..4].copy_from_slice(&(nlmsg_len as u32).to_ne_bytes());
    buf[4..6].copy_from_slice(&family_id.to_ne_bytes());
    let flags = NLM_F_REQUEST | NLM_F_ACK;
    buf[6..8].copy_from_slice(&flags.to_ne_bytes());

    // GenL header
    buf[16] = match wg_msg.cmd {
        WireguardCmd::GetDevice => 0,
        WireguardCmd::SetDevice => 1,
        _ => 0,
    };
    buf[17] = 1; // version

    // WireGuard payload
    wg_msg.emit(&mut buf[20..]);

    socket
        .send(&buf, 0)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink send: {}", e)))?;

    let mut recv_buf = vec![0u8; 4096];
    let n = socket
        .recv(&mut recv_buf, 0)
        .map_err(|e| NetlinkError::SocketError(format!("genetlink recv: {}", e)))?;

    // Check for error response
    if n >= 20 {
        let nlmsg_type = u16::from_ne_bytes([recv_buf[4], recv_buf[5]]);
        if nlmsg_type == 2 {
            // NLMSG_ERROR
            let errno =
                i32::from_ne_bytes([recv_buf[16], recv_buf[17], recv_buf[18], recv_buf[19]]);
            if errno != 0 {
                return Err(NetlinkError::ProtocolError(format!(
                    "wireguard set_device error: errno {}",
                    errno
                )));
            }
        }
    }

    Ok(())
}
