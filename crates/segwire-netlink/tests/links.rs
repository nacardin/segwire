use netlink_packet_core::{NetlinkMessage, NetlinkPayload, NLM_F_DUMP};
use netlink_packet_route::link::LinkMessage;
use netlink_packet_route::RouteNetlinkMessage;
use segwire_netlink::{NetlinkProtocol, NetlinkSocket};

#[test]
fn list_all_links() {
    let mut sock =
        NetlinkSocket::open(NetlinkProtocol::Route).expect("failed to open netlink route socket");

    // Build an RTM_GETLINK dump request.
    let mut msg = NetlinkMessage::from(RouteNetlinkMessage::GetLink(LinkMessage::default()));
    msg.header.flags |= NLM_F_DUMP;

    let responses = sock.request(msg).expect("RTM_GETLINK dump failed");

    // There should always be at least the loopback interface.
    assert!(
        !responses.is_empty(),
        "expected at least one link, got none"
    );

    let mut found_lo = false;

    for resp in &responses {
        if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(link)) = &resp.payload {
            let name = link
                .attributes
                .iter()
                .find_map(|attr| {
                    if let netlink_packet_route::link::LinkAttribute::IfName(n) = attr {
                        Some(n.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("<unknown>");

            eprintln!("  link[{}]: {}", link.header.index, name);

            if name == "lo" {
                found_lo = true;
            }
        }
    }

    assert!(found_lo, "loopback interface 'lo' not found in link dump");
}
