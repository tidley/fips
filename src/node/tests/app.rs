//! Tests for application service ports over established FIPS sessions.

use super::*;
use crate::node::tests::spanning_tree::{
    cleanup_nodes, process_available_packets, run_tree_test, verify_tree_convergence,
};
use std::time::Duration;

fn populate_all_coord_caches(nodes: &mut [super::spanning_tree::TestNode]) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let all_coords: Vec<(NodeAddr, crate::tree::TreeCoordinate)> = nodes
        .iter()
        .map(|tn| {
            (
                *tn.node.node_addr(),
                tn.node.tree_state().my_coords().clone(),
            )
        })
        .collect();

    for tn in nodes.iter_mut() {
        for (addr, coords) in &all_coords {
            if addr != tn.node.node_addr() {
                tn.node
                    .coord_cache_mut()
                    .insert(*addr, coords.clone(), now_ms);
            }
        }
    }
}

#[test]
fn test_bind_app_port_rejects_reserved_port() {
    let mut node = make_node();
    let result = node.bind_app_port(crate::node::session_wire::FSP_PORT_IPV6_SHIM);
    assert!(matches!(
        result,
        Err(NodeError::AppPortReserved(
            crate::node::session_wire::FSP_PORT_IPV6_SHIM
        ))
    ));
}

#[tokio::test]
async fn test_app_port_send_establishes_session_and_delivers_datagram() {
    let edges = vec![(0, 1)];
    let mut nodes = run_tree_test(2, &edges, false).await;
    verify_tree_convergence(&nodes);
    populate_all_coord_caches(&mut nodes);

    let node0_npub = nodes[0].node.npub();
    let node1_npub = nodes[1].node.npub();
    let node0_addr = *nodes[0].node.node_addr();
    let node1_addr = *nodes[1].node.node_addr();
    let rx = nodes[1].node.bind_app_port(7000).unwrap();

    nodes[0]
        .node
        .send_app_data_to_npub(&node1_npub, 6000, 7000, b"frame-1")
        .await
        .unwrap();

    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        process_available_packets(&mut nodes).await;
    }

    let datagram = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("expected app datagram");
    assert_eq!(datagram.peer_npub, node0_npub);
    assert_eq!(datagram.peer_node_addr, node0_addr);
    assert_eq!(datagram.src_port, 6000);
    assert_eq!(datagram.dst_port, 7000);
    assert_eq!(datagram.payload, b"frame-1");
    assert!(
        nodes[0]
            .node
            .get_session(&node1_addr)
            .map(|e| e.state().is_established())
            .unwrap_or(false)
    );

    cleanup_nodes(&mut nodes).await;
}

#[tokio::test]
async fn test_app_port_send_over_established_session() {
    let edges = vec![(0, 1)];
    let mut nodes = run_tree_test(2, &edges, false).await;
    verify_tree_convergence(&nodes);
    populate_all_coord_caches(&mut nodes);

    let node0_npub = nodes[0].node.npub();
    let node1_npub = nodes[1].node.npub();
    let rx = nodes[1].node.bind_app_port(7100).unwrap();

    nodes[0]
        .node
        .send_app_data_to_npub(&node1_npub, 6100, 7100, b"frame-a")
        .await
        .unwrap();
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        process_available_packets(&mut nodes).await;
    }
    let _ = rx.recv_timeout(Duration::from_secs(1)).unwrap();

    nodes[0]
        .node
        .send_app_data_to_npub(&node1_npub, 6100, 7100, b"frame-b")
        .await
        .unwrap();
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        process_available_packets(&mut nodes).await;
    }

    let datagram = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("expected second app datagram");
    assert_eq!(datagram.peer_npub, node0_npub);
    assert_eq!(datagram.payload, b"frame-b");

    cleanup_nodes(&mut nodes).await;
}
