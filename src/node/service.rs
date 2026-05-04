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
    use crate::Config;

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
}
