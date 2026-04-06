use crate::NodeAddr;
use crate::node::NodeError;
use tokio::sync::oneshot;

/// Datagram delivered to a bound FIPS application port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDatagram {
    /// Remote peer npub.
    pub peer_npub: String,
    /// Remote peer node address.
    pub peer_node_addr: NodeAddr,
    /// Source service port.
    pub src_port: u16,
    /// Destination service port.
    pub dst_port: u16,
    /// Service payload.
    pub payload: Vec<u8>,
}

impl AppDatagram {
    pub fn new(
        peer_npub: String,
        peer_node_addr: NodeAddr,
        src_port: u16,
        dst_port: u16,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            peer_npub,
            peer_node_addr,
            src_port,
            dst_port,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingAppPacket {
    pub(crate) src_port: u16,
    pub(crate) dst_port: u16,
    pub(crate) payload: Vec<u8>,
}

impl PendingAppPacket {
    pub(crate) fn new(src_port: u16, dst_port: u16, payload: Vec<u8>) -> Self {
        Self {
            src_port,
            dst_port,
            payload,
        }
    }
}

/// Command channel for driving application traffic into a running node.
#[derive(Debug)]
pub enum AppCommand {
    SendDatagram {
        peer_npub: String,
        src_port: u16,
        dst_port: u16,
        payload: Vec<u8>,
        response: oneshot::Sender<Result<(), NodeError>>,
    },
}
