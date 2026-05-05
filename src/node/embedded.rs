//! Embedded app-facing node helpers.
//!
//! These methods expose the Nostr bootstrap handoff path without requiring the
//! daemon, control socket, TUN device, or `fipsctl`. They are intended for
//! in-process clients such as mobile apps.

use super::{Node, NodeError};
use crate::config::PeerConfig;
use crate::discovery::BootstrapHandoffResult;
use crate::node::service::ServiceOutbound;
use crate::{NodeAddr, PeerIdentity};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// Result of draining embedded Nostr bootstrap events.
#[derive(Clone, Debug)]
pub enum NostrBootstrapOutcome {
    /// A punched UDP traversal was adopted into the node.
    Adopted(BootstrapHandoffResult),
    /// The traversal runtime failed to establish a usable UDP path.
    Failed { npub: String, reason: String },
}

/// Lightweight status snapshot for embedded clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedNodeStatus {
    pub npub: String,
    pub node_addr: String,
    pub fips_address: String,
    pub state: String,
    pub tun_state: String,
    pub peer_count: usize,
    pub link_count: usize,
    pub session_count: usize,
}

/// Commands accepted by the embedded node event loop.
pub enum EmbeddedNodeCommand {
    RequestNostrBootstrap {
        peer_config: PeerConfig,
        respond_to: Option<oneshot::Sender<Result<(), String>>>,
    },
    EnsureServiceSession {
        peer_identity: PeerIdentity,
        respond_to: Option<oneshot::Sender<Result<(), String>>>,
    },
    HasServiceSession {
        dest_addr: NodeAddr,
        respond_to: oneshot::Sender<bool>,
    },
    SendServiceData {
        outbound: ServiceOutbound,
        respond_to: Option<oneshot::Sender<Result<(), String>>>,
    },
    Status {
        respond_to: oneshot::Sender<EmbeddedNodeStatus>,
    },
    Stop,
}

impl Node {
    /// Return a compact status snapshot for app-owned runtimes.
    pub fn embedded_status(&self) -> EmbeddedNodeStatus {
        EmbeddedNodeStatus {
            npub: self.npub(),
            node_addr: self.node_addr().to_string(),
            fips_address: self.identity().address().to_string(),
            state: self.state().to_string(),
            tun_state: self.tun_state().to_string(),
            peer_count: self.peer_count(),
            link_count: self.link_count(),
            session_count: self.session_count(),
        }
    }

    /// Ask the embedded Nostr discovery runtime to connect to a peer.
    ///
    /// This starts the existing Nostr advert/signaling/STUN traversal flow.
    /// Call [`Node::drain_nostr_bootstrap`] periodically to adopt successful
    /// traversals into the normal FIPS transport and handshake stack.
    #[cfg(feature = "nostr-discovery")]
    pub async fn request_nostr_bootstrap(
        &mut self,
        peer_config: PeerConfig,
    ) -> Result<(), NodeError> {
        let runtime = self.nostr_discovery.clone().ok_or_else(|| {
            NodeError::NostrDiscoveryUnavailable("runtime is not running".to_string())
        })?;
        runtime.request_connect(peer_config).await;
        Ok(())
    }

    /// Ask the embedded Nostr discovery runtime to connect to a peer.
    ///
    /// This build was compiled without `nostr-discovery`, so no runtime can be
    /// available.
    #[cfg(not(feature = "nostr-discovery"))]
    pub async fn request_nostr_bootstrap(
        &mut self,
        _peer_config: PeerConfig,
    ) -> Result<(), NodeError> {
        Err(NodeError::NostrDiscoveryUnavailable(
            "compiled without nostr-discovery".to_string(),
        ))
    }

    /// Drain embedded Nostr bootstrap events and adopt established traversals.
    ///
    /// This is the library-friendly equivalent of the daemon lifecycle's Nostr
    /// polling path, but it does not schedule retries or try static fallback
    /// addresses. Embedded callers can decide their own retry/UI policy from
    /// the returned outcomes.
    #[cfg(feature = "nostr-discovery")]
    pub async fn drain_nostr_bootstrap(&mut self) -> Result<Vec<NostrBootstrapOutcome>, NodeError> {
        use crate::discovery::nostr::BootstrapEvent;

        let runtime = self.nostr_discovery.clone().ok_or_else(|| {
            NodeError::NostrDiscoveryUnavailable("runtime is not running".to_string())
        })?;

        let mut outcomes = Vec::new();
        for event in runtime.drain_events().await {
            match event {
                BootstrapEvent::Established { traversal } => {
                    let handoff = self.adopt_established_traversal(traversal).await?;
                    outcomes.push(NostrBootstrapOutcome::Adopted(handoff));
                }
                BootstrapEvent::Failed {
                    peer_config,
                    reason,
                } => {
                    outcomes.push(NostrBootstrapOutcome::Failed {
                        npub: peer_config.npub,
                        reason,
                    });
                }
            }
        }

        Ok(outcomes)
    }

    /// Drain embedded Nostr bootstrap events and adopt established traversals.
    ///
    /// This build was compiled without `nostr-discovery`, so no runtime can be
    /// available.
    #[cfg(not(feature = "nostr-discovery"))]
    pub async fn drain_nostr_bootstrap(&mut self) -> Result<Vec<NostrBootstrapOutcome>, NodeError> {
        Err(NodeError::NostrDiscoveryUnavailable(
            "compiled without nostr-discovery".to_string(),
        ))
    }
}
