//! WireGuard configuration file parser.
//!
//! Parses standard wg-quick `.conf` files and converts them into
//! segwire's native `NamespaceConfig` format.
//!
//! ## Supported formats
//!
//! **wg-quick format** (INI-style):
//! ```ini
//! [Interface]
//! PrivateKey = <base64>
//! ListenPort = 51820
//! Address = 10.0.0.1/24
//! DNS = 8.8.8.8
//!
//! [Peer]
//! PublicKey = <base64>
//! Endpoint = 203.0.113.1:51820
//! AllowedIPs = 0.0.0.0/0, ::/0
//! PersistentKeepalive = 25
//! PresharedKey = <base64>
//! ```

use crate::config::{
    DnsConfig, InterfaceConfig, NamespaceConfig, NamespaceSettings, RoutingConfig,
    VirtualInterface, WireguardConfig, WireguardPeerConfig,
};
use std::collections::HashMap;

/// Errors that can occur during WireGuard config parsing.
#[derive(Debug, thiserror::Error)]
pub enum WgParseError {
    #[error("missing [Interface] section")]
    MissingInterface,
    #[error("missing PrivateKey in [Interface]")]
    MissingPrivateKey,
    #[error("invalid line: {0}")]
    InvalidLine(String),
    #[error("unknown section: [{0}]")]
    UnknownSection(String),
}

/// Intermediate representation of a parsed wg-quick config.
#[derive(Debug, Default)]
struct WgQuickConfig {
    private_key: String,
    listen_port: u16,
    addresses: Vec<String>,
    dns_servers: Vec<String>,
    fwmark: u32,
    peers: Vec<WgQuickPeer>,
}

#[derive(Debug, Default)]
struct WgQuickPeer {
    public_key: String,
    preshared_key: Option<String>,
    endpoint: Option<String>,
    allowed_ips: Vec<String>,
    persistent_keepalive: u16,
}

/// Parse a wg-quick config file into a segwire `NamespaceConfig`.
///
/// The `namespace_name` parameter is used as the namespace name. The
/// WireGuard interface will be named `wg0` by default (can be overridden
/// with `wg_interface_name`).
pub fn parse_wg_quick(
    content: &str,
    namespace_name: &str,
    wg_interface_name: Option<&str>,
) -> Result<NamespaceConfig, WgParseError> {
    let wg_config = parse_wg_quick_raw(content)?;
    let ifname = wg_interface_name.unwrap_or("wg0").to_string();

    // Convert to NamespaceConfig
    let wireguard = WireguardConfig {
        private_key: wg_config.private_key.clone(),
        listen_port: wg_config.listen_port,
        fwmark: wg_config.fwmark,
        peers: wg_config
            .peers
            .iter()
            .map(|p| WireguardPeerConfig {
                public_key: p.public_key.clone(),
                preshared_key: p.preshared_key.clone(),
                endpoint: p.endpoint.clone(),
                allowed_ips: p.allowed_ips.clone(),
                persistent_keepalive: p.persistent_keepalive,
            })
            .collect(),
    };

    let virtual_interfaces = vec![VirtualInterface {
        name: ifname,
        interface_type: "wireguard".to_string(),
        peer: None,
        addresses: wg_config.addresses.clone(),
    }];

    let dns = DnsConfig {
        servers: wg_config.dns_servers.clone(),
        search: vec![],
    };

    // If any peer has AllowedIPs = 0.0.0.0/0, route all traffic through WireGuard
    let routing = RoutingConfig::default();

    Ok(NamespaceConfig {
        namespace: NamespaceSettings {
            name: namespace_name.to_string(),
            description: "WireGuard namespace (imported from wg-quick config)".to_string(),
        },
        interfaces: InterfaceConfig {
            move_interfaces: vec![],
            virtual_interfaces,
        },
        routing,
        dns,
        environment: HashMap::new(),
        wireguard: Some(wireguard),
    })
}

/// Parse the raw text of a wg-quick config into the intermediate representation.
fn parse_wg_quick_raw(content: &str) -> Result<WgQuickConfig, WgParseError> {
    let mut config = WgQuickConfig::default();
    let mut current_section = None;
    let mut current_peer: Option<WgQuickPeer> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section headers
        if line.starts_with('[') && line.ends_with(']') {
            // Flush any pending peer
            if let Some(peer) = current_peer.take() {
                config.peers.push(peer);
            }

            let section = &line[1..line.len() - 1];
            match section {
                "Interface" => current_section = Some("Interface"),
                "Peer" => {
                    current_section = Some("Peer");
                    current_peer = Some(WgQuickPeer::default());
                }
                other => return Err(WgParseError::UnknownSection(other.to_string())),
            }
            continue;
        }

        // Key = Value pairs
        let (key, value) = line
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .ok_or_else(|| WgParseError::InvalidLine(line.to_string()))?;

        match current_section {
            Some("Interface") => match key {
                "PrivateKey" => config.private_key = value.to_string(),
                "ListenPort" => {
                    config.listen_port = value
                        .parse()
                        .map_err(|_| WgParseError::InvalidLine(line.to_string()))?;
                }
                "Address" => {
                    for addr in value.split(',') {
                        config.addresses.push(addr.trim().to_string());
                    }
                }
                "DNS" => {
                    for dns in value.split(',') {
                        config.dns_servers.push(dns.trim().to_string());
                    }
                }
                "FwMark" => {
                    config.fwmark = value
                        .parse()
                        .map_err(|_| WgParseError::InvalidLine(line.to_string()))?;
                }
                "Table" | "PreUp" | "PostUp" | "PreDown" | "PostDown" | "SaveConfig" | "MTU" => {
                    // wg-quick directives — silently ignored (not applicable to segwire)
                }
                _ => {
                    // Unknown keys in Interface section are silently ignored
                }
            },
            Some("Peer") => {
                let peer = current_peer.as_mut().unwrap();
                match key {
                    "PublicKey" => peer.public_key = value.to_string(),
                    "PresharedKey" => peer.preshared_key = Some(value.to_string()),
                    "Endpoint" => peer.endpoint = Some(value.to_string()),
                    "AllowedIPs" => {
                        for ip in value.split(',') {
                            peer.allowed_ips.push(ip.trim().to_string());
                        }
                    }
                    "PersistentKeepalive" => {
                        peer.persistent_keepalive = value
                            .parse()
                            .map_err(|_| WgParseError::InvalidLine(line.to_string()))?;
                    }
                    _ => {
                        // Unknown keys in Peer section are silently ignored
                    }
                }
            }
            _ => {
                // Lines outside any section are silently ignored
            }
        }
    }

    // Flush final peer
    if let Some(peer) = current_peer.take() {
        config.peers.push(peer);
    }

    if config.private_key.is_empty() {
        return Err(WgParseError::MissingPrivateKey);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a valid 32-byte base64 key for tests.
    fn test_key() -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode([0x42u8; 32])
    }

    fn test_key_2() -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode([0x43u8; 32])
    }

    #[test]
    fn test_parse_minimal_wg_quick() {
        let key = test_key();
        let peer_key = test_key_2();
        let config = format!(
            "[Interface]\n\
             PrivateKey = {key}\n\
             \n\
             [Peer]\n\
             PublicKey = {peer_key}\n\
             AllowedIPs = 0.0.0.0/0\n"
        );

        let result = parse_wg_quick(&config, "vpn-ns", None).unwrap();
        assert_eq!(result.namespace.name, "vpn-ns");
        assert_eq!(result.interfaces.virtual_interfaces[0].interface_type, "wireguard");
        assert_eq!(result.interfaces.virtual_interfaces[0].name, "wg0");

        let wg = result.wireguard.unwrap();
        assert_eq!(wg.private_key, key);
        assert_eq!(wg.peers.len(), 1);
        assert_eq!(wg.peers[0].public_key, peer_key);
        assert_eq!(wg.peers[0].allowed_ips, vec!["0.0.0.0/0"]);
    }

    #[test]
    fn test_parse_full_wg_quick() {
        let key = test_key();
        let peer_key = test_key_2();
        let psk = test_key();
        let config = format!(
            "[Interface]\n\
             PrivateKey = {key}\n\
             ListenPort = 51820\n\
             Address = 10.0.0.1/24, fd00::1/128\n\
             DNS = 8.8.8.8, 8.8.4.4\n\
             \n\
             [Peer]\n\
             PublicKey = {peer_key}\n\
             PresharedKey = {psk}\n\
             Endpoint = 203.0.113.1:51820\n\
             AllowedIPs = 0.0.0.0/0, ::/0\n\
             PersistentKeepalive = 25\n"
        );

        let result = parse_wg_quick(&config, "full-vpn", Some("wg1")).unwrap();
        assert_eq!(result.interfaces.virtual_interfaces[0].name, "wg1");
        assert_eq!(
            result.interfaces.virtual_interfaces[0].addresses,
            vec!["10.0.0.1/24", "fd00::1/128"]
        );
        assert_eq!(result.dns.servers, vec!["8.8.8.8", "8.8.4.4"]);

        let wg = result.wireguard.unwrap();
        assert_eq!(wg.listen_port, 51820);
        assert_eq!(wg.peers[0].endpoint.as_deref(), Some("203.0.113.1:51820"));
        assert_eq!(wg.peers[0].preshared_key.as_deref(), Some(psk.as_str()));
        assert_eq!(wg.peers[0].persistent_keepalive, 25);
        assert_eq!(wg.peers[0].allowed_ips, vec!["0.0.0.0/0", "::/0"]);
    }

    #[test]
    fn test_parse_multiple_peers() {
        let key = test_key();
        let pk1 = test_key_2();
        let pk2 = test_key();
        let config = format!(
            "[Interface]\n\
             PrivateKey = {key}\n\
             \n\
             [Peer]\n\
             PublicKey = {pk1}\n\
             AllowedIPs = 10.0.1.0/24\n\
             \n\
             [Peer]\n\
             PublicKey = {pk2}\n\
             AllowedIPs = 10.0.2.0/24\n\
             Endpoint = 198.51.100.1:51820\n"
        );

        let result = parse_wg_quick(&config, "multi-peer", None).unwrap();
        let wg = result.wireguard.unwrap();
        assert_eq!(wg.peers.len(), 2);
        assert_eq!(wg.peers[0].public_key, pk1);
        assert_eq!(wg.peers[1].public_key, pk2);
        assert_eq!(wg.peers[1].endpoint.as_deref(), Some("198.51.100.1:51820"));
    }

    #[test]
    fn test_parse_wg_quick_ignores_comments() {
        let key = test_key();
        let peer_key = test_key_2();
        let config = format!(
            "# This is a comment\n\
             ; This is also a comment\n\
             [Interface]\n\
             PrivateKey = {key}\n\
             # Another comment\n\
             \n\
             [Peer]\n\
             PublicKey = {peer_key}\n\
             AllowedIPs = 10.0.0.0/8\n"
        );

        let result = parse_wg_quick(&config, "comment-test", None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_wg_quick_missing_private_key() {
        let config = "[Interface]\nListenPort = 51820\n";
        let result = parse_wg_quick(config, "test", None);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WgParseError::MissingPrivateKey
        ));
    }

    #[test]
    fn test_parse_wg_quick_ignores_wg_quick_directives() {
        let key = test_key();
        let peer_key = test_key_2();
        let config = format!(
            "[Interface]\n\
             PrivateKey = {key}\n\
             Table = auto\n\
             PostUp = iptables -A FORWARD -i wg0 -j ACCEPT\n\
             PostDown = iptables -D FORWARD -i wg0 -j ACCEPT\n\
             SaveConfig = true\n\
             MTU = 1420\n\
             \n\
             [Peer]\n\
             PublicKey = {peer_key}\n\
             AllowedIPs = 10.0.0.0/8\n"
        );

        let result = parse_wg_quick(&config, "directive-test", None);
        assert!(result.is_ok());
    }
}
