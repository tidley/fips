#![cfg(feature = "nostr-discovery")]

//! Integration tests for bootstrap handoff into the FIPS node.

use super::*;
use crate::EstablishedTraversal;
use crate::config::{
    PeerAssistDialMode, PeerAssistRequestPolicy, PeerConfig, TransportInstances, UdpConfig,
};
use crate::discovery::nostr::{
    ADVERT_IDENTIFIER, ADVERT_VERSION, AssistGrant, AssistObserved, AssistRequest, NostrDiscovery,
    OverlayAdvert, OverlayEndpointAdvert, OverlayTransportKind, PEER_ASSIST_MAGIC,
};
use crate::node::wire::{PHASE_MSG1, PHASE_MSG2, PHASE_MSG3};
use crate::transport::udp::UdpTransport;
use crate::utils::index::IndexAllocator;
use tokio::time::{Duration, timeout, timeout_at};

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

    node_a.packet_tx = Some(packet_tx_a.clone());
    node_a.packet_rx = Some(packet_rx_a);
    node_a.state = NodeState::Running;

    let mut transport_b = UdpTransport::new(transport_id_b, None, udp_config, packet_tx_b.clone());
    transport_b.start_async().await.unwrap();

    let addr_b = transport_b.local_addr().unwrap();
    node_b.packet_tx = Some(packet_tx_b.clone());
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

    // XX three-way handshake (drive directly so the cancellation pattern
    // doesn't strand `packet_rx` after one rx_loop iteration).
    //   1. node_a (initiator) sent msg1 in adopt_established_traversal
    //   2. node_b receives msg1, generates msg2 (reveals node_b identity)
    //   3. node_a receives msg2, generates msg3 (reveals node_a identity)
    //   4. node_b receives msg3, promotes node_a as peer
    let mut rx_a = node_a.packet_rx.take().expect("node_a packet_rx");
    let mut rx_b = node_b.packet_rx.take().expect("node_b packet_rx");

    let pkt_at_b = timeout(Duration::from_secs(1), rx_b.recv())
        .await
        .expect("timeout waiting for node_a -> node_b msg1")
        .expect("node_b channel closed");
    assert_eq!(pkt_at_b.data[0] & 0x0f, PHASE_MSG1);
    node_b.handle_msg1(pkt_at_b).await;

    let pkt_at_a = timeout(Duration::from_secs(1), rx_a.recv())
        .await
        .expect("timeout waiting for node_b -> node_a msg2")
        .expect("node_a channel closed");
    assert_eq!(pkt_at_a.data[0] & 0x0f, PHASE_MSG2);
    node_a.handle_msg2(pkt_at_a).await;

    let pkt_at_b = timeout(Duration::from_secs(1), rx_b.recv())
        .await
        .expect("timeout waiting for node_a -> node_b msg3")
        .expect("node_b channel closed");
    assert_eq!(pkt_at_b.data[0] & 0x0f, PHASE_MSG3);
    node_b.handle_msg3(pkt_at_b).await;

    let peer_a_node_addr =
        *PeerIdentity::from_pubkey_full(node_a.identity.pubkey_full()).node_addr();
    let peer_b_node_addr =
        *PeerIdentity::from_pubkey_full(node_b.identity.pubkey_full()).node_addr();

    assert_eq!(
        node_a.peer_count(),
        1,
        "node_a should promote node_b after receiving msg2"
    );
    assert_eq!(
        node_b.peer_count(),
        1,
        "node_b should promote node_a after receiving msg3"
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

#[tokio::test]
async fn test_third_peer_can_handshake_via_adopted_transport_socket() {
    let mut node_a = make_node(); // Existing traversal peer (Alice)
    let mut node_b = make_node(); // Node with adopted socket (Bob)
    let mut node_c = make_node(); // New peer onboarding via Bob socket (Colin)

    let transport_id_a = TransportId::new(1);
    let transport_id_c = TransportId::new(1);
    let udp_config = UdpConfig {
        bind_addr: Some("127.0.0.1:0".to_string()),
        mtu: Some(1280),
        ..Default::default()
    };

    let (packet_tx_a, packet_rx_a) = packet_channel(64);
    let (packet_tx_b, packet_rx_b) = packet_channel(64);
    let (packet_tx_c, packet_rx_c) = packet_channel(64);

    node_a.packet_tx = Some(packet_tx_a.clone());
    node_a.packet_rx = Some(packet_rx_a);
    node_a.state = NodeState::Running;

    node_b.packet_tx = Some(packet_tx_b.clone());
    node_b.packet_rx = Some(packet_rx_b);
    node_b.state = NodeState::Running;

    node_c.packet_tx = Some(packet_tx_c.clone());
    node_c.packet_rx = Some(packet_rx_c);
    node_c.state = NodeState::Running;

    let mut transport_a = UdpTransport::new(transport_id_a, None, udp_config.clone(), packet_tx_a);
    transport_a.start_async().await.unwrap();
    let addr_a = transport_a.local_addr().unwrap();
    node_a
        .transports
        .insert(transport_id_a, TransportHandle::Udp(transport_a));

    // Bob adopts a traversal socket already "established" to Alice.
    let adopted_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let handoff = EstablishedTraversal::new("sess-existing", node_a.npub(), addr_a, adopted_socket)
        .with_transport_name("nostr-nat");
    let handoff_result = node_b.adopt_established_traversal(handoff).await.unwrap();

    // Drive Alice/Bob handshake manually (msg1 -> msg2).
    let mut rx_a = node_a.packet_rx.take().expect("node_a packet_rx");
    let mut rx_b = node_b.packet_rx.take().expect("node_b packet_rx");

    let pkt_at_a = timeout(Duration::from_secs(1), rx_a.recv())
        .await
        .expect("timeout waiting for Bob->Alice msg1")
        .expect("node_a channel closed");
    assert_eq!(pkt_at_a.data[0] & 0x0f, PHASE_MSG1);
    node_a.handle_msg1(pkt_at_a).await;

    let pkt_at_b = timeout(Duration::from_secs(1), rx_b.recv())
        .await
        .expect("timeout waiting for Alice->Bob msg2")
        .expect("node_b channel closed");
    assert_eq!(pkt_at_b.data[0] & 0x0f, PHASE_MSG2);
    node_b.handle_msg2(pkt_at_b).await;

    // XX msg3: Bob (initiator of Alice/Bob sub-handshake) sent msg3 in
    // response to msg2; Alice receives it and promotes Bob.
    let pkt_at_a = timeout(Duration::from_secs(1), rx_a.recv())
        .await
        .expect("timeout waiting for Bob->Alice msg3")
        .expect("node_a channel closed");
    assert_eq!(pkt_at_a.data[0] & 0x0f, PHASE_MSG3);
    node_a.handle_msg3(pkt_at_a).await;

    let node_a_addr = *PeerIdentity::from_pubkey_full(node_a.identity.pubkey_full()).node_addr();
    assert!(
        node_b.get_peer(&node_a_addr).is_some(),
        "node_b should first be connected to node_a via adopted transport"
    );

    // Start Colin UDP transport and connect to Bob's adopted socket address.
    let mut transport_c = UdpTransport::new(transport_id_c, None, udp_config, packet_tx_c);
    transport_c.start_async().await.unwrap();
    let addr_c = transport_c.local_addr().unwrap();
    let addr_c_label = addr_c.to_string();
    node_c
        .transports
        .insert(transport_id_c, TransportHandle::Udp(transport_c));

    let peer_b_identity = PeerIdentity::from_pubkey_full(node_b.identity.pubkey_full());
    let adopted_addr = TransportAddr::from_string(&handoff_result.local_addr.to_string());
    node_c
        .initiate_connection(transport_id_c, adopted_addr, Some(peer_b_identity))
        .await
        .unwrap();

    // Drive Bob/Colin handshake manually (msg1 -> msg2).
    let mut rx_c = node_c.packet_rx.take().expect("node_c packet_rx");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_b = loop {
        let pkt = timeout_at(deadline, rx_b.recv())
            .await
            .expect("timeout waiting for Colin->Bob msg1")
            .expect("node_b channel closed");
        if pkt.remote_addr.as_str() == Some(addr_c_label.as_str())
            && pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG1)
        {
            break pkt;
        }
    };
    node_b.handle_msg1(pkt_at_b).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_c = loop {
        let pkt = timeout_at(deadline, rx_c.recv())
            .await
            .expect("timeout waiting for Bob->Colin msg2")
            .expect("node_c channel closed");
        if pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG2) {
            break pkt;
        }
    };
    node_c.handle_msg2(pkt_at_c).await;

    // XX msg3: node_c (initiator) sent msg3 in response to msg2.
    // node_b receives msg3 and promotes node_c.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_b = loop {
        let pkt = timeout_at(deadline, rx_b.recv())
            .await
            .expect("timeout waiting for Colin->Bob msg3")
            .expect("node_b channel closed");
        if pkt.remote_addr.as_str() == Some(&addr_c.to_string())
            && pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG3)
        {
            break pkt;
        }
    };
    node_b.handle_msg3(pkt_at_b).await;

    let node_c_addr = *PeerIdentity::from_pubkey_full(node_c.identity.pubkey_full()).node_addr();
    assert!(
        node_b.get_peer(&node_c_addr).is_some(),
        "node_b should promote node_c when node_c handshakes via adopted socket"
    );

    for (_, transport) in node_a.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_b.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_c.transports.iter_mut() {
        transport.stop().await.ok();
    }
}

#[tokio::test]
async fn test_adopted_traversal_observed_endpoint_stays_private() {
    let mut node_a = make_node();
    let mut node_b = make_node();
    node_b.config.node.discovery.nostr.enabled = true;
    node_b.config.node.discovery.nostr.peer_assist.dial_mode = PeerAssistDialMode::FallbackPrivate;
    node_b
        .config
        .node
        .discovery
        .nostr
        .peer_assist
        .helper
        .enabled = true;
    node_b
        .config
        .node
        .discovery
        .nostr
        .peer_assist
        .helper
        .request_policy = PeerAssistRequestPolicy::OpenRateLimited;
    node_b.config.transports.udp = TransportInstances::Single(UdpConfig {
        advertise_on_nostr: Some(true),
        public: Some(false),
        peer_assist: Some(true),
        ..Default::default()
    });

    let transport_id_a = TransportId::new(1);
    let udp_config = UdpConfig {
        bind_addr: Some("127.0.0.1:0".to_string()),
        mtu: Some(1280),
        ..Default::default()
    };

    let (packet_tx_a, packet_rx_a) = packet_channel(64);
    let (packet_tx_b, packet_rx_b) = packet_channel(64);

    node_a.packet_tx = Some(packet_tx_a.clone());
    node_a.packet_rx = Some(packet_rx_a);
    node_a.state = NodeState::Running;

    node_b.packet_tx = Some(packet_tx_b.clone());
    node_b.packet_rx = Some(packet_rx_b);
    node_b.state = NodeState::Running;

    let mut transport_a = UdpTransport::new(transport_id_a, None, udp_config, packet_tx_a);
    transport_a.start_async().await.unwrap();
    let addr_a = transport_a.local_addr().unwrap();
    node_a
        .transports
        .insert(transport_id_a, TransportHandle::Udp(transport_a));

    let adopted_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let handoff = EstablishedTraversal::new("sess-helper", node_a.npub(), addr_a, adopted_socket)
        .with_observed_endpoint("198.51.100.20:44750".parse().unwrap());
    node_b.adopt_established_traversal(handoff).await.unwrap();

    let advert = node_b
        .build_overlay_advert()
        .expect("private assist advert");
    assert!(
        advert
            .endpoints
            .iter()
            .any(|endpoint| endpoint.addr == "nat"),
        "node_b should still advertise udp:nat for Nostr onboarding"
    );
    assert!(
        !serde_json::to_string(&advert)
            .unwrap()
            .contains("198.51.100.20:44750"),
        "private helper endpoints must not be published in the overlay advert"
    );

    for (_, transport) in node_a.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_b.transports.iter_mut() {
        transport.stop().await.ok();
    }
}

#[tokio::test]
async fn test_no_stun_nat_advert_waits_for_helper_endpoint() {
    let mut node = make_node();
    node.config.node.discovery.nostr.enabled = true;
    node.config.node.discovery.nostr.stun_servers.clear();
    node.config.node.discovery.nostr.peer_assist.helper.enabled = true;
    node.config
        .node
        .discovery
        .nostr
        .peer_assist
        .helper
        .request_policy = PeerAssistRequestPolicy::OpenRateLimited;
    node.config.transports.udp = TransportInstances::Single(UdpConfig {
        bind_addr: Some("127.0.0.1:0".to_string()),
        advertise_on_nostr: Some(true),
        public: Some(false),
        peer_assist: Some(true),
        ..Default::default()
    });

    let transport_id = TransportId::new(1);
    let (packet_tx, packet_rx) = packet_channel(64);
    node.packet_tx = Some(packet_tx.clone());
    node.packet_rx = Some(packet_rx);
    node.state = NodeState::Running;

    let mut transport = UdpTransport::new(
        transport_id,
        None,
        UdpConfig {
            bind_addr: Some("127.0.0.1:0".to_string()),
            ..Default::default()
        },
        packet_tx,
    );
    transport.start_async().await.unwrap();
    node.transports
        .insert(transport_id, TransportHandle::Udp(transport));

    assert!(
        node.build_overlay_advert().is_none(),
        "no-STUN helper nodes must not publish udp:nat until a helper endpoint is known"
    );

    node.peer_assist_endpoints
        .insert(transport_id, "198.51.100.20:44750".parse().unwrap());

    let advert = node
        .build_overlay_advert()
        .expect("helper endpoint should enable deferred udp:nat advert");
    assert!(
        advert
            .endpoints
            .iter()
            .any(|endpoint| endpoint.addr == "nat"),
        "node should publish udp:nat once it has an active helper endpoint"
    );
    assert!(
        advert.stun_servers.is_none(),
        "no-STUN peer-assist adverts should not synthesize STUN metadata"
    );

    for (_, transport) in node.transports.iter_mut() {
        transport.stop().await.ok();
    }
}

#[tokio::test]
async fn test_private_assist_request_grant_observed_and_adopted_handoff() {
    let mut node_a = make_node(); // Existing traversal peer (Alice)
    let mut node_b = make_node(); // Helper node with adopted socket (Bob)
    let mut node_c = make_node(); // New peer using peer assist (Colin)

    let transport_id_a = TransportId::new(1);
    let udp_config = UdpConfig {
        bind_addr: Some("127.0.0.1:0".to_string()),
        mtu: Some(1280),
        ..Default::default()
    };

    let (packet_tx_a, packet_rx_a) = packet_channel(64);
    let (packet_tx_b, packet_rx_b) = packet_channel(128);
    let (packet_tx_c, packet_rx_c) = packet_channel(64);

    node_a.packet_tx = Some(packet_tx_a.clone());
    node_a.packet_rx = Some(packet_rx_a);
    node_a.state = NodeState::Running;

    node_b.packet_tx = Some(packet_tx_b.clone());
    node_b.packet_rx = Some(packet_rx_b);
    node_b.state = NodeState::Running;

    node_c.packet_tx = Some(packet_tx_c.clone());
    node_c.packet_rx = Some(packet_rx_c);
    node_c.state = NodeState::Running;

    let mut transport_a = UdpTransport::new(transport_id_a, None, udp_config.clone(), packet_tx_a);
    transport_a.start_async().await.unwrap();
    let addr_a = transport_a.local_addr().unwrap();
    node_a
        .transports
        .insert(transport_id_a, TransportHandle::Udp(transport_a));

    node_b.config.node.discovery.nostr.enabled = true;
    node_b.config.node.discovery.nostr.peer_assist.dial_mode = PeerAssistDialMode::FallbackPrivate;
    node_b
        .config
        .node
        .discovery
        .nostr
        .peer_assist
        .helper
        .enabled = true;
    node_b
        .config
        .node
        .discovery
        .nostr
        .peer_assist
        .helper
        .request_policy = PeerAssistRequestPolicy::OpenRateLimited;
    node_b.config.transports.udp = TransportInstances::Single(UdpConfig {
        advertise_on_nostr: Some(true),
        public: Some(false),
        peer_assist: Some(true),
        ..Default::default()
    });

    let adopted_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let helper_addr = adopted_socket.local_addr().unwrap();
    let handoff = EstablishedTraversal::new("sess-existing", node_a.npub(), addr_a, adopted_socket)
        .with_observed_endpoint(helper_addr);
    let _handoff_result = node_b.adopt_established_traversal(handoff).await.unwrap();

    // Drive Alice/Bob handshake manually to establish Bob's adopted transport first.
    let mut rx_a = node_a.packet_rx.take().expect("node_a packet_rx");
    let mut rx_b = node_b.packet_rx.take().expect("node_b packet_rx");

    let pkt_at_a = timeout(Duration::from_secs(1), rx_a.recv())
        .await
        .expect("timeout waiting for Bob->Alice msg1")
        .expect("node_a channel closed");
    assert_eq!(pkt_at_a.data[0] & 0x0f, PHASE_MSG1);
    node_a.handle_msg1(pkt_at_a).await;

    let pkt_at_b = timeout(Duration::from_secs(1), rx_b.recv())
        .await
        .expect("timeout waiting for Alice->Bob msg2")
        .expect("node_b channel closed");
    assert_eq!(pkt_at_b.data[0] & 0x0f, PHASE_MSG2);
    node_b.handle_msg2(pkt_at_b).await;

    let node_a_addr = *PeerIdentity::from_pubkey_full(node_a.identity.pubkey_full()).node_addr();
    assert!(
        node_b.get_peer(&node_a_addr).is_some(),
        "node_b should first be connected to node_a via adopted transport"
    );

    let mut bob_runtime_config = node_b.config.node.discovery.nostr.clone();
    bob_runtime_config.enabled = true;
    bob_runtime_config.peer_assist.dial_mode = PeerAssistDialMode::FallbackPrivate;
    bob_runtime_config.peer_assist.helper.enabled = true;
    bob_runtime_config.peer_assist.helper.request_policy = PeerAssistRequestPolicy::OpenRateLimited;
    bob_runtime_config.dm_relays = vec!["wss://relay.example".to_string()];
    let bob_runtime = NostrDiscovery::new_for_test(&node_b.identity, bob_runtime_config);
    bob_runtime
        .update_private_helper_endpoints(vec![helper_addr])
        .await;

    let mut colin_runtime_config = node_c.config.node.discovery.nostr.clone();
    colin_runtime_config.enabled = true;
    colin_runtime_config.stun_servers.clear();
    colin_runtime_config.peer_assist.dial_mode = PeerAssistDialMode::FallbackPrivate;
    colin_runtime_config.dm_relays = vec!["wss://relay.example".to_string()];
    let colin_runtime = NostrDiscovery::new_for_test(&node_c.identity, colin_runtime_config);

    let bob_advert = OverlayAdvert {
        identifier: ADVERT_IDENTIFIER.to_string(),
        version: ADVERT_VERSION,
        endpoints: vec![OverlayEndpointAdvert {
            transport: OverlayTransportKind::Udp,
            addr: "nat".to_string(),
        }],
        signal_relays: Some(vec!["wss://relay.example".to_string()]),
        stun_servers: None,
    };
    let bob_peer_config = PeerConfig {
        npub: node_b.npub(),
        via_nostr: true,
        ..Default::default()
    };

    let colin_connect_task = tokio::spawn({
        let runtime = colin_runtime.clone();
        let peer_config = bob_peer_config.clone();
        let advert = bob_advert.clone();
        async move {
            runtime
                .connect_peer_via_private_assist_for_test(peer_config, advert)
                .await
        }
    });

    let request: AssistRequest = loop {
        let signals = colin_runtime.drain_test_signals().await;
        if let Some(request) = signals
            .into_iter()
            .find_map(|payload| serde_json::from_str::<AssistRequest>(&payload).ok())
        {
            break request;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let colin_nostr_pubkey =
        nostr::Keys::parse(&hex::encode(node_c.identity.keypair().secret_bytes()))
            .unwrap()
            .public_key();

    bob_runtime
        .clone()
        .handle_incoming_assist_request_for_test(request.clone(), colin_nostr_pubkey, node_c.npub())
        .await
        .expect("register Bob-side peer assist observation");

    let grant: AssistGrant = loop {
        let signals = bob_runtime.drain_test_signals().await;
        if let Some(grant) = signals
            .into_iter()
            .find_map(|payload| serde_json::from_str::<AssistGrant>(&payload).ok())
        {
            break grant;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let helper_addr_label = helper_addr.to_string();
    assert_eq!(
        grant.helper_addr.as_deref(),
        Some(helper_addr_label.as_str())
    );

    colin_runtime
        .inject_assist_grant_for_test(grant.clone(), node_b.npub())
        .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let probe_packet = loop {
        let pkt = timeout_at(deadline, rx_b.recv())
            .await
            .expect("timeout waiting for Colin private-assist probe")
            .expect("node_b channel closed");
        if pkt.data.len() >= 4
            && u32::from_be_bytes(pkt.data[0..4].try_into().unwrap()) == PEER_ASSIST_MAGIC
        {
            break pkt;
        }
    };
    let observed_addr = probe_packet
        .remote_addr
        .as_str()
        .and_then(|addr| addr.parse::<std::net::SocketAddr>().ok())
        .expect("probe remote addr");
    assert!(
        bob_runtime
            .observe_peer_assist_probe(helper_addr, observed_addr, &probe_packet.data)
            .await,
        "Bob should accept the observed peer-assist probe"
    );

    let observed: AssistObserved = loop {
        let signals = bob_runtime.drain_test_signals().await;
        if let Some(observed) = signals
            .into_iter()
            .find_map(|payload| serde_json::from_str::<AssistObserved>(&payload).ok())
        {
            break observed;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let observed_addr_label = observed_addr.to_string();
    assert_eq!(
        observed
            .observed_address
            .as_ref()
            .map(|addr| format!("{}:{}", addr.ip, addr.port))
            .as_deref(),
        Some(observed_addr_label.as_str())
    );

    colin_runtime
        .inject_assist_observed_for_test(observed, node_b.npub())
        .await;

    let traversal = timeout(Duration::from_secs(2), colin_connect_task)
        .await
        .expect("timeout waiting for Colin traversal result")
        .expect("join peer-assist connect task")
        .expect("Colin traversal result");
    assert_eq!(traversal.remote_addr, helper_addr);
    assert_eq!(traversal.observed_endpoint, Some(observed_addr));

    let _handoff = node_c.adopt_established_traversal(traversal).await.unwrap();

    let mut rx_c = node_c.packet_rx.take().expect("node_c packet_rx");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_b = loop {
        let pkt = timeout_at(deadline, rx_b.recv())
            .await
            .expect("timeout waiting for Colin->Bob msg1")
            .expect("node_b channel closed");
        if pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG1) {
            break pkt;
        }
    };
    node_b.handle_msg1(pkt_at_b).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_c = loop {
        let pkt = timeout_at(deadline, rx_c.recv())
            .await
            .expect("timeout waiting for Bob->Colin msg2")
            .expect("node_c channel closed");
        if pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG2) {
            break pkt;
        }
    };
    node_c.handle_msg2(pkt_at_c).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let pkt_at_b = loop {
        let pkt = timeout_at(deadline, rx_b.recv())
            .await
            .expect("timeout waiting for Colin->Bob msg3")
            .expect("node_b channel closed");
        if pkt.data.first().map(|b| b & 0x0f) == Some(PHASE_MSG3) {
            break pkt;
        }
    };
    node_b.handle_msg3(pkt_at_b).await;

    let node_c_addr = *PeerIdentity::from_pubkey_full(node_c.identity.pubkey_full()).node_addr();
    assert!(
        node_b.get_peer(&node_c_addr).is_some(),
        "node_b should promote node_c after peer-assist handoff and handshake"
    );
    for (_, transport) in node_a.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_b.transports.iter_mut() {
        transport.stop().await.ok();
    }
    for (_, transport) in node_c.transports.iter_mut() {
        transport.stop().await.ok();
    }
}
