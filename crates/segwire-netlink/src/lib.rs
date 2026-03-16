//! Synchronous netlink socket for segwire.
//!
//! This crate provides a blocking [`NetlinkSocket`] for talking to the Linux
//! kernel via netlink.  Specific operations (link, address, route, wireguard)
//! are built on top of this socket in higher-level crates.
//!
//! # Typed requests (Route Netlink)
//!
//! ```no_run
//! use segwire_netlink::{NetlinkSocket, NetlinkProtocol};
//! use netlink_packet_route::{RouteNetlinkMessage, link::LinkMessage};
//! use netlink_packet_core::{NetlinkMessage, NLM_F_DUMP};
//!
//! let mut sock = NetlinkSocket::open(NetlinkProtocol::Route).unwrap();
//!
//! let mut msg = NetlinkMessage::from(RouteNetlinkMessage::GetLink(LinkMessage::default()));
//! msg.header.flags |= NLM_F_DUMP;
//!
//! let responses = sock.request(msg).unwrap();
//! ```
//!
//! # Raw requests (Generic Netlink)
//!
//! For protocols that don't have a typed `NetlinkDeserializable` implementation
//! (e.g. WireGuard via Generic Netlink), use [`NetlinkSocket::send_raw`] and
//! [`NetlinkSocket::recv_raw`] directly.

use std::fmt::Debug;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use netlink_packet_core::{
    Emitable, NetlinkDeserializable, NetlinkMessage, NetlinkPayload, NetlinkSerializable, NLM_F_ACK,
    NLM_F_DUMP, NLM_F_REQUEST,
};
use segwire_common::netlink::NetlinkError;

// ---------------------------------------------------------------------------
// NetlinkProtocol
// ---------------------------------------------------------------------------

/// Netlink protocol family for socket creation.
///
/// Each variant corresponds to a `NETLINK_*` constant from the Linux kernel.
/// Use with [`NetlinkSocket::open`] to create a socket bound to the chosen
/// protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetlinkProtocol {
    /// `NETLINK_ROUTE` — routing, links, addresses, neighbors.
    Route,
    /// `NETLINK_GENERIC` — generic netlink multiplexer (WireGuard, etc.).
    Generic,
}

impl NetlinkProtocol {
    /// Convert to the `nix::sys::socket::SockProtocol` representation.
    fn to_nix(self) -> nix::sys::socket::SockProtocol {
        match self {
            Self::Route => nix::sys::socket::SockProtocol::NetlinkRoute,
            // NB: nix doesn't have a named constant for NETLINK_GENERIC (16),
            // but we can construct it from the raw value on the socket call.
            Self::Generic => nix::sys::socket::SockProtocol::NetlinkRoute, // placeholder
        }
    }

    /// Raw protocol number for `socket()`.
    fn raw_value(self) -> i32 {
        match self {
            Self::Route => 0,   // NETLINK_ROUTE
            Self::Generic => 16, // NETLINK_GENERIC
        }
    }
}

// ---------------------------------------------------------------------------
// NetlinkSocket
// ---------------------------------------------------------------------------

/// Synchronous blocking netlink socket.
///
/// The socket is created, bound, and ready for use at construction time.
/// A monotonically incrementing sequence number is maintained per-socket
/// for outgoing messages.
///
/// This type is `Send` — it can be moved between threads.
#[derive(Debug)]
pub struct NetlinkSocket {
    fd: OwnedFd,
    seq: u32,
}

impl NetlinkSocket {
    /// Open and bind a netlink socket for the given protocol.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use segwire_netlink::{NetlinkSocket, NetlinkProtocol};
    /// let sock = NetlinkSocket::open(NetlinkProtocol::Route).unwrap();
    /// ```
    pub fn open(protocol: NetlinkProtocol) -> Result<Self, NetlinkError> {
        let fd = open_netlink_fd(protocol)?;
        Ok(Self { fd, seq: 1 })
    }

    /// Allocate a fresh sequence number for outgoing netlink messages.
    ///
    /// Sequence numbers are scoped to this socket and increment monotonically.
    pub fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = s.wrapping_add(1);
        s
    }

    /// Send a typed netlink message and collect all response messages.
    ///
    /// Automatically assigns a sequence number, sets `NLM_F_REQUEST`, and
    /// calls `finalize()` before sending.  Handles multi-part (DUMP) responses,
    /// ACKs, and kernel error messages.
    pub fn request<I>(
        &mut self,
        mut msg: NetlinkMessage<I>,
    ) -> Result<Vec<NetlinkMessage<I>>, NetlinkError>
    where
        I: Debug + NetlinkSerializable + NetlinkDeserializable,
    {
        // Prepare the message.
        let seq = self.next_seq();
        msg.header.sequence_number = seq;
        msg.header.flags |= NLM_F_REQUEST;
        msg.finalize();

        let mut buf = vec![0u8; msg.buffer_len()];
        msg.emit(&mut buf);

        // Send.
        self.send_raw(&buf)?;

        // Receive and parse responses.
        let is_dump = (msg.header.flags & NLM_F_DUMP) != 0;
        let mut responses = Vec::new();
        let mut recv_buf = vec![0u8; 65536];

        loop {
            let n = self.recv_raw(&mut recv_buf)?;

            if parse_response_batch(&recv_buf[..n], &mut responses)? {
                break;
            }

            // For non-DUMP requests, stop after the first batch.
            if !is_dump && !responses.is_empty() {
                break;
            }
        }

        Ok(responses)
    }

    /// Send raw bytes to the kernel.
    ///
    /// Useful for Generic Netlink messages that are serialized manually
    /// (e.g. WireGuard, where we bypass `netlink-packet-generic`).
    pub fn send_raw(&self, buf: &[u8]) -> Result<usize, NetlinkError> {
        let n = nix::sys::socket::send(
            self.fd.as_raw_fd(),
            buf,
            nix::sys::socket::MsgFlags::empty(),
        )
        .map_err(|e| NetlinkError::SocketError(format!("send() failed: {}", e)))?;
        Ok(n)
    }

    /// Receive raw bytes from the kernel.
    ///
    /// Returns the number of bytes read.  Useful for Generic Netlink messages
    /// that need manual response parsing.
    pub fn recv_raw(&self, buf: &mut [u8]) -> Result<usize, NetlinkError> {
        let n = nix::sys::socket::recv(
            self.fd.as_raw_fd(),
            buf,
            nix::sys::socket::MsgFlags::empty(),
        )
        .map_err(|e| NetlinkError::SocketError(format!("recv() failed: {}", e)))?;
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Generic Netlink helpers
// ---------------------------------------------------------------------------

/// Resolve a Generic Netlink family name (e.g. `"wireguard"`) to its kernel
/// family ID.
///
/// Opens a `NETLINK_GENERIC` socket, sends `CTRL_CMD_GETFAMILY`, and parses
/// the response for `CTRL_ATTR_FAMILY_ID`.
///
/// Uses raw byte construction for the control message to avoid version
/// conflicts between different `netlink-packet-generic` releases.
pub fn resolve_genl_family_id(family_name: &str) -> Result<u16, NetlinkError> {
    let mut sock = NetlinkSocket::open(NetlinkProtocol::Generic)?;

    // Build CTRL_CMD_GETFAMILY raw message.
    //
    // Layout:
    //   [16 bytes] Netlink header
    //   [ 4 bytes] GenL header: cmd=CTRL_CMD_GETFAMILY(3), version(1), reserved(0)
    //   [NLA]      CTRL_ATTR_FAMILY_NAME(2) = "<family_name>\0"
    const GENL_ID_CTRL: u16 = 0x10;
    const CTRL_CMD_GETFAMILY: u8 = 3;
    const CTRL_ATTR_FAMILY_NAME: u16 = 2;

    let name_bytes: Vec<u8> = family_name.bytes().chain(std::iter::once(0)).collect();
    let nla_payload_len = name_bytes.len();
    let nla_len = 4 + nla_payload_len;
    let nla_padded = (nla_len + 3) & !3;

    let genl_hdr_len = 4;
    let nlmsg_len = 16 + genl_hdr_len + nla_padded;

    let mut buf = vec![0u8; nlmsg_len];

    // Netlink header
    let seq = sock.next_seq();
    buf[0..4].copy_from_slice(&(nlmsg_len as u32).to_ne_bytes());
    buf[4..6].copy_from_slice(&GENL_ID_CTRL.to_ne_bytes());
    buf[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    buf[8..12].copy_from_slice(&seq.to_ne_bytes());

    // GenL header
    buf[16] = CTRL_CMD_GETFAMILY;
    buf[17] = 1; // version

    // NLA: CTRL_ATTR_FAMILY_NAME
    let nla_start = 20;
    buf[nla_start..nla_start + 2].copy_from_slice(&(nla_len as u16).to_ne_bytes());
    buf[nla_start + 2..nla_start + 4].copy_from_slice(&CTRL_ATTR_FAMILY_NAME.to_ne_bytes());
    buf[nla_start + 4..nla_start + 4 + nla_payload_len].copy_from_slice(&name_bytes);

    sock.send_raw(&buf)?;

    let mut recv_buf = vec![0u8; 4096];
    let n = sock.recv_raw(&mut recv_buf)?;

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
                    "GETFAMILY '{}' error: errno {}",
                    family_name, errno
                )));
            }
        }
        return Err(NetlinkError::ProtocolError(
            "unexpected ACK for GETFAMILY".to_string(),
        ));
    }

    // Parse NLAs looking for CTRL_ATTR_FAMILY_ID (1) = u16
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

    Err(NetlinkError::ProtocolError(format!(
        "{} family ID not found in GETFAMILY response",
        family_name
    )))
}

/// Send a raw Generic Netlink request and check for an ACK.
///
/// Constructs the netlink header + generic netlink header around the given
/// `payload` bytes, sends it on a fresh `NETLINK_GENERIC` socket, and
/// verifies the kernel returns a successful ACK (or no error).
///
/// The `payload` should contain the serialized attributes (NLAs) for the
/// specific genl family — the caller is responsible for building those.
pub fn send_genl_request(
    family_id: u16,
    cmd: u8,
    version: u8,
    payload: &[u8],
) -> Result<(), NetlinkError> {
    let mut sock = NetlinkSocket::open(NetlinkProtocol::Generic)?;

    // Layout:
    //   [16 bytes] Netlink header
    //   [ 4 bytes] GenL header (cmd, version, reserved)
    //   [variable] Payload
    let genl_hdr_len = 4;
    let nlmsg_len = 16 + genl_hdr_len + payload.len();

    let mut buf = vec![0u8; nlmsg_len];

    // Netlink header
    let seq = sock.next_seq();
    buf[0..4].copy_from_slice(&(nlmsg_len as u32).to_ne_bytes());
    buf[4..6].copy_from_slice(&family_id.to_ne_bytes());
    let flags = NLM_F_REQUEST | NLM_F_ACK;
    buf[6..8].copy_from_slice(&flags.to_ne_bytes());
    buf[8..12].copy_from_slice(&seq.to_ne_bytes());

    // GenL header
    buf[16] = cmd;
    buf[17] = version;

    // Payload
    buf[20..20 + payload.len()].copy_from_slice(payload);

    sock.send_raw(&buf)?;

    let mut recv_buf = vec![0u8; 4096];
    let n = sock.recv_raw(&mut recv_buf)?;

    // Check for error response
    if n >= 20 {
        let nlmsg_type = u16::from_ne_bytes([recv_buf[4], recv_buf[5]]);
        if nlmsg_type == 2 {
            // NLMSG_ERROR
            let errno =
                i32::from_ne_bytes([recv_buf[16], recv_buf[17], recv_buf[18], recv_buf[19]]);
            if errno != 0 {
                return Err(NetlinkError::ProtocolError(format!(
                    "genetlink error: errno {}",
                    errno
                )));
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Create, bind, and (optionally connect) a raw `AF_NETLINK` datagram socket.
fn open_netlink_fd(protocol: NetlinkProtocol) -> Result<OwnedFd, NetlinkError> {
    use nix::sys::socket::{
        bind, socket, AddressFamily, NetlinkAddr, SockFlag, SockType,
    };

    // Create AF_NETLINK datagram socket with CLOEXEC.
    //
    // nix only exposes `SockProtocol::NetlinkRoute` — for NETLINK_GENERIC we
    // must go through the raw libc value.
    let fd = match protocol {
        NetlinkProtocol::Route => socket(
            AddressFamily::Netlink,
            SockType::Datagram,
            SockFlag::SOCK_CLOEXEC,
            protocol.to_nix(),
        )
        .map_err(|e| NetlinkError::SocketError(format!("socket() failed: {}", e)))?,
        NetlinkProtocol::Generic => {
            // nix doesn't have a SockProtocol variant for NETLINK_GENERIC,
            // so we use libc directly.
            let raw_fd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                    protocol.raw_value(),
                )
            };
            if raw_fd < 0 {
                return Err(NetlinkError::SocketError(format!(
                    "socket() failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            unsafe { OwnedFd::from_raw_fd(raw_fd) }
        }
    };

    // Bind to autobind address (pid=0 → kernel assigns, groups=0).
    let addr = NetlinkAddr::new(0, 0);
    bind(fd.as_raw_fd(), &addr)
        .map_err(|e| NetlinkError::SocketError(format!("bind() failed: {}", e)))?;

    Ok(fd)
}

/// Parse one recv buffer of netlink messages, appending results to `responses`.
///
/// Returns `Ok(true)` when the stream is complete (received `NLMSG_DONE` or
/// an ACK), `Ok(false)` when more data is expected, or `Err` on parse/kernel
/// errors.
fn parse_response_batch<I>(
    data: &[u8],
    responses: &mut Vec<NetlinkMessage<I>>,
) -> Result<bool, NetlinkError>
where
    I: Debug + NetlinkDeserializable,
{
    let mut offset = 0;

    while offset < data.len() {
        // Parse the netlink header to get message length.
        if data.len() - offset < 4 {
            break;
        }
        let msg_len = u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        if msg_len < 16 || offset + msg_len > data.len() {
            break;
        }

        let msg_data = &data[offset..offset + msg_len];

        match NetlinkMessage::<I>::deserialize(msg_data) {
            Ok(parsed) => match &parsed.payload {
                NetlinkPayload::Done(_) => {
                    return Ok(true);
                }
                NetlinkPayload::Error(err) => {
                    if let Some(code) = err.code {
                        return Err(NetlinkError::ProtocolError(format!(
                            "netlink error: code {}",
                            code.get()
                        )));
                    }
                    // ACK — success.
                    return Ok(true);
                }
                _ => {
                    responses.push(parsed);
                }
            },
            Err(e) => {
                return Err(NetlinkError::ProtocolError(format!(
                    "failed to parse netlink message: {}",
                    e
                )));
            }
        }

        offset += msg_len;
        // Align to 4-byte boundary.
        offset = (offset + 3) & !3;
    }

    Ok(false)
}
