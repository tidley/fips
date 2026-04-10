//! Integration tests for bootstrap handoff into the FIPS node.

use super::*;
use crate::EstablishedTraversal;
use crate::config::UdpConfig;
use crate::transport::udp::UdpTransport;
use crate::utils::index::IndexAllocator;
use tokio::time::Duration;

#[tokio::test]
async fn test_adopted_udp_traversal_completes_handshake() {
    let mut node_a = make_node();
    let mut node_b = make_node();

    let transport_id_b = TransportId::new(1);
    let udp_config = UdpConfig {
        bind_addr: Some("127.0.0.1:0".to_string()),
        mtu: Some(1280),
        ..Default::default()
    };

    let (packet_tx_a, packet_rx_a) = packet_channel(64);
    let (packet_tx_b, packet_rx_b) = packet_channel(64);

    node_a.packet_tx = Some(packet_tx_a);
    node_a.packet_rx = Some(packet_rx_a);
    node_a.state = NodeState::Running;

    let mut transport_b = UdpTransport::new(transport_id_b, None, udp_config, packet_tx_b.clone());
    transport_b.start_async().await.unwrap();

    let addr_b = transport_b.local_addr().unwrap();
    node_b.packet_tx = Some(packet_tx_b);
    node_b.packet_rx = Some(packet_rx_b);
    node_b.state = NodeState::Running;
    node_b
        .transports
        .insert(transport_id_b, TransportHandle::Udp(transport_b));

    let adopted_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let handoff = EstablishedTraversal::new("sess-1", node_b.npub(), addr_b, adopted_socket)
        .with_transport_name("nostr-punched");

    let result = node_a.adopt_established_traversal(handoff).await.unwrap();
    assert_eq!(result.remote_addr, addr_b);
    assert!(node_a.get_transport(&result.transport_id).is_some());

    tokio::select! {
        result = node_b.run_rx_loop() => {
            panic!("node_b rx loop exited unexpectedly: {:?}", result);
        }
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }

    tokio::select! {
        result = node_a.run_rx_loop() => {
            panic!("node_a rx loop exited unexpectedly: {:?}", result);
        }
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }

    let peer_a_node_addr =
        *PeerIdentity::from_pubkey_full(node_a.identity.pubkey_full()).node_addr();
    let peer_b_node_addr =
        *PeerIdentity::from_pubkey_full(node_b.identity.pubkey_full()).node_addr();

    assert_eq!(
        node_a.peer_count(),
        1,
        "node_a should promote node_b after handoff"
    );
    assert_eq!(
        node_b.peer_count(),
        1,
        "node_b should promote node_a after receiving msg1"
    );
    assert!(node_a.get_peer(&peer_b_node_addr).unwrap().has_session());
    assert!(node_b.get_peer(&peer_a_node_addr).unwrap().has_session());

    for (_, transport) in node_a.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_b.transports.iter_mut() {
        transport.stop().await.ok();
    }
}

#[tokio::test]
async fn test_failed_adopted_traversal_cleans_up_transport() {
    let mut node = make_node();
    let (packet_tx, packet_rx) = packet_channel(64);
    node.packet_tx = Some(packet_tx);
    node.packet_rx = Some(packet_rx);
    node.state = NodeState::Running;
    node.index_allocator = IndexAllocator::with_max_attempts(0);

    let peer = make_node();
    let adopted_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let handoff = EstablishedTraversal::new(
        "sess-fail",
        peer.npub(),
        "127.0.0.1:9".parse().unwrap(),
        adopted_socket,
    )
    .with_transport_name("nostr-punched");

    let result = node.adopt_established_traversal(handoff).await;
    assert!(
        result.is_err(),
        "handoff should fail when handshake setup cannot allocate a session index"
    );
    assert!(
        node.transports.is_empty(),
        "failed handoff should remove the adopted transport"
    );
}
