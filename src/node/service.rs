//! In-process FSP service ports.
//!
//! This is the first app-facing boundary for embedded clients. FSP already
//! carries encrypted DataPacket payloads with a small port header; this module
//! lets local Rust code bind one of those ports without creating a TUN device.

use super::session_wire::FSP_PORT_IPV6_SHIM;
use super::{Node, NodeError};
use crate::{NodeAddr, PeerIdentity};
use tokio::sync::mpsc;

/// Decrypted service payload delivered to a local in-process FSP service port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServicePacket {
    /// Remote FIPS node that sent the payload.
    pub src_addr: NodeAddr,
    /// Source service port chosen by the remote sender.
    pub src_port: u16,
    /// Local service port that received the payload.
    pub dst_port: u16,
    /// Decrypted service bytes.
    pub payload: Vec<u8>,
}

/// Receiver returned by [`Node::register_service_port`].
pub type ServiceRx = mpsc::Receiver<ServicePacket>;

impl Node {
    /// Register a local in-process FSP service port.
    ///
    /// Port 256 is reserved for the IPv6 shim. Other ports are delivered to
    /// the returned channel when encrypted FSP DataPackets arrive.
    pub fn register_service_port(
        &mut self,
        port: u16,
        queue_depth: usize,
    ) -> Result<ServiceRx, NodeError> {
        if port == FSP_PORT_IPV6_SHIM {
            return Err(NodeError::ServicePortReserved { port });
        }
        if self.service_ports.contains_key(&port) {
            return Err(NodeError::ServicePortAlreadyRegistered { port });
        }

        let (tx, rx) = mpsc::channel(queue_depth.max(1));
        self.service_ports.insert(port, tx);
        Ok(rx)
    }

    /// Remove a local in-process FSP service port registration.
    pub fn unregister_service_port(&mut self, port: u16) -> bool {
        self.service_ports.remove(&port).is_some()
    }

    /// Check whether a local in-process FSP service port is registered.
    pub fn is_service_port_registered(&self, port: u16) -> bool {
        self.service_ports.contains_key(&port)
    }

    /// Check whether an end-to-end session to a peer is established.
    pub fn has_established_service_session(&self, dest_addr: &NodeAddr) -> bool {
        self.sessions
            .get(dest_addr)
            .is_some_and(|entry| entry.is_established())
    }

    /// Register the peer identity and initiate an end-to-end session if needed.
    ///
    /// This is useful for embedded clients that know a peer npub and want to
    /// send service data without synthesizing IPv6 packets for the TUN path.
    /// The method returns once the session exists or the first handshake packet
    /// has been queued; callers can use [`Node::has_established_service_session`]
    /// to observe establishment before sending application data.
    pub async fn ensure_service_session(
        &mut self,
        peer_identity: &PeerIdentity,
    ) -> Result<(), NodeError> {
        let dest_addr = *peer_identity.node_addr();
        let dest_pubkey = peer_identity.pubkey_full();
        self.register_identity(dest_addr, dest_pubkey);

        if let Some(entry) = self.sessions.get(&dest_addr)
            && (entry.is_established() || entry.is_initiating())
        {
            return Ok(());
        }

        self.initiate_session(dest_addr, dest_pubkey).await
    }

    /// Send encrypted service bytes to a remote FSP service port.
    ///
    /// The destination must already have an established end-to-end session.
    /// Call [`Node::ensure_service_session`] first when the session may not
    /// exist yet.
    pub async fn send_service_data(
        &mut self,
        dest_addr: &NodeAddr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Result<(), NodeError> {
        if src_port == FSP_PORT_IPV6_SHIM {
            return Err(NodeError::ServicePortReserved { port: src_port });
        }
        if dst_port == FSP_PORT_IPV6_SHIM {
            return Err(NodeError::ServicePortReserved { port: dst_port });
        }

        self.send_session_data(dest_addr, src_port, dst_port, payload)
            .await
    }

    pub(in crate::node) fn deliver_service_packet(
        &mut self,
        src_addr: &NodeAddr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> bool {
        let Some(tx) = self.service_ports.get(&dst_port) else {
            return false;
        };

        let packet = ServicePacket {
            src_addr: *src_addr,
            src_port,
            dst_port,
            payload: payload.to_vec(),
        };

        match tx.try_send(packet) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::debug!(dst_port, "FSP service port queue full, dropping payload");
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    dst_port,
                    "FSP service port receiver closed, dropping payload"
                );
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::session::{EndToEndState, SessionEntry};
    use crate::noise::{HandshakeState, NoiseSession};
    use crate::{Config, Identity};

    fn make_noise_session(our_identity: &Identity, remote_identity: &Identity) -> NoiseSession {
        let mut initiator = HandshakeState::new_initiator(our_identity.keypair());
        let mut responder = HandshakeState::new_responder(remote_identity.keypair());

        let mut init_epoch = [0u8; 8];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut init_epoch);
        initiator.set_local_epoch(init_epoch);
        let mut resp_epoch = [0u8; 8];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut resp_epoch);
        responder.set_local_epoch(resp_epoch);

        let msg1 = initiator.write_message_1().unwrap();
        responder.read_message_1(&msg1).unwrap();
        let msg2 = responder.write_message_2().unwrap();
        initiator.read_message_2(&msg2).unwrap();
        let msg3 = initiator.write_message_3().unwrap();
        responder.read_message_3(&msg3).unwrap();

        initiator.into_session().unwrap()
    }

    #[test]
    fn register_rejects_reserved_ipv6_shim_port() {
        let mut node = Node::new(Config::default()).unwrap();

        let err = node
            .register_service_port(FSP_PORT_IPV6_SHIM, 8)
            .unwrap_err();

        assert!(matches!(
            err,
            NodeError::ServicePortReserved {
                port: FSP_PORT_IPV6_SHIM
            }
        ));
    }

    #[test]
    fn register_rejects_duplicate_port() {
        let mut node = Node::new(Config::default()).unwrap();
        let _rx = node.register_service_port(4096, 8).unwrap();

        let err = node.register_service_port(4096, 8).unwrap_err();

        assert!(matches!(
            err,
            NodeError::ServicePortAlreadyRegistered { port: 4096 }
        ));
    }

    #[test]
    fn deliver_registered_service_packet() {
        let mut node = Node::new(Config::default()).unwrap();
        let mut rx = node.register_service_port(4096, 8).unwrap();
        let src_addr = NodeAddr::from_bytes([7u8; 16]);

        assert!(node.deliver_service_packet(&src_addr, 5000, 4096, b"hello"));

        let packet = rx.try_recv().unwrap();
        assert_eq!(packet.src_addr, src_addr);
        assert_eq!(packet.src_port, 5000);
        assert_eq!(packet.dst_port, 4096);
        assert_eq!(packet.payload, b"hello");
    }

    #[test]
    fn deliver_unregistered_service_packet_returns_false() {
        let mut node = Node::new(Config::default()).unwrap();
        let src_addr = NodeAddr::from_bytes([7u8; 16]);

        assert!(!node.deliver_service_packet(&src_addr, 5000, 4096, b"hello"));
    }

    #[test]
    fn unregister_and_registration_status_are_reported() {
        let mut node = Node::new(Config::default()).unwrap();

        assert!(!node.is_service_port_registered(4096));
        assert!(!node.unregister_service_port(4096));

        let _rx = node.register_service_port(4096, 8).unwrap();
        assert!(node.is_service_port_registered(4096));
        assert!(node.unregister_service_port(4096));
        assert!(!node.is_service_port_registered(4096));
        assert!(!node.unregister_service_port(4096));
    }

    #[test]
    fn deliver_full_service_queue_counts_as_handled() {
        let mut node = Node::new(Config::default()).unwrap();
        let _rx = node.register_service_port(4096, 1).unwrap();
        let src_addr = NodeAddr::from_bytes([7u8; 16]);

        assert!(node.deliver_service_packet(&src_addr, 5000, 4096, b"one"));
        assert!(node.deliver_service_packet(&src_addr, 5000, 4096, b"two"));
    }

    #[test]
    fn deliver_closed_service_queue_counts_as_handled() {
        let mut node = Node::new(Config::default()).unwrap();
        let rx = node.register_service_port(4096, 1).unwrap();
        drop(rx);

        let src_addr = NodeAddr::from_bytes([7u8; 16]);
        assert!(node.deliver_service_packet(&src_addr, 5000, 4096, b"hello"));
    }

    #[tokio::test]
    async fn send_service_data_rejects_reserved_ports_before_session_lookup() {
        let mut node = Node::new(Config::default()).unwrap();
        let dest_addr = NodeAddr::from_bytes([8u8; 16]);

        let err = node
            .send_service_data(&dest_addr, FSP_PORT_IPV6_SHIM, 4096, b"hello")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            NodeError::ServicePortReserved {
                port: FSP_PORT_IPV6_SHIM
            }
        ));

        let err = node
            .send_service_data(&dest_addr, 5000, FSP_PORT_IPV6_SHIM, b"hello")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            NodeError::ServicePortReserved {
                port: FSP_PORT_IPV6_SHIM
            }
        ));
    }

    #[test]
    fn established_service_session_status_tracks_session_state() {
        let mut node = Node::new(Config::default()).unwrap();
        let remote = Identity::generate();
        let remote_addr = *remote.node_addr();

        assert!(!node.has_established_service_session(&remote_addr));

        let handshake = HandshakeState::new_initiator(node.identity().keypair());
        node.sessions.insert(
            remote_addr,
            SessionEntry::new(
                remote_addr,
                remote.pubkey_full(),
                EndToEndState::Initiating(handshake),
                1000,
                true,
            ),
        );
        assert!(!node.has_established_service_session(&remote_addr));

        let session = make_noise_session(node.identity(), &remote);
        node.sessions.insert(
            remote_addr,
            SessionEntry::new(
                remote_addr,
                remote.pubkey_full(),
                EndToEndState::Established(session),
                1000,
                true,
            ),
        );
        assert!(node.has_established_service_session(&remote_addr));
    }

    #[tokio::test]
    async fn ensure_service_session_noops_for_existing_initiating_session() {
        let mut node = Node::new(Config::default()).unwrap();
        let remote = Identity::generate();
        let peer_identity = PeerIdentity::from_pubkey_full(remote.pubkey_full());
        let remote_addr = *peer_identity.node_addr();
        let handshake = HandshakeState::new_initiator(node.identity().keypair());

        node.sessions.insert(
            remote_addr,
            SessionEntry::new(
                remote_addr,
                remote.pubkey_full(),
                EndToEndState::Initiating(handshake),
                1000,
                true,
            ),
        );

        node.ensure_service_session(&peer_identity).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_service_session_registers_identity_and_reports_no_route() {
        let mut node = Node::new(Config::default()).unwrap();
        let remote = Identity::generate();
        let peer_identity = PeerIdentity::from_pubkey_full(remote.pubkey_full());

        let err = node
            .ensure_service_session(&peer_identity)
            .await
            .unwrap_err();

        assert!(matches!(err, NodeError::SendFailed { .. }));
        assert!(
            node.identity_cache_len() == 1,
            "embedded callers should be able to register peer identity before routing succeeds"
        );
    }
}
