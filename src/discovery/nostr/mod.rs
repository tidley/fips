#![cfg(feature = "nostr-discovery")]

mod runtime;
mod signal;
pub(crate) mod stun;
mod traversal;
mod types;

#[cfg(test)]
mod tests;

pub use runtime::NostrDiscovery;
pub use types::{
    ADVERT_IDENTIFIER, ADVERT_KIND, ADVERT_VERSION, AssistGrant, AssistObserved, AssistRequest,
    BootstrapError, BootstrapEvent, CachedOverlayAdvert, OverlayAdvert, OverlayEndpointAdvert,
    OverlayTransportKind, PEER_ASSIST_MAGIC, PROTOCOL_VERSION, PUNCH_ACK_MAGIC, PUNCH_MAGIC,
    PunchHint, PunchPacket, PunchPacketKind, SIGNAL_KIND, TraversalAddress, TraversalAnswer,
    TraversalOffer,
};
