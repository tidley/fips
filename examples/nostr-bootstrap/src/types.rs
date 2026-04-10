use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalAddress {
    pub protocol: String,
    pub ip: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunchHint {
    #[serde(rename = "startAtMs")]
    pub start_at_ms: u64,
    #[serde(rename = "intervalMs")]
    pub interval_ms: u64,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalAdvert {
    pub app: String,
    #[serde(rename = "eventKind")]
    pub event_kind: u16,
    pub protocol: String,
    #[serde(rename = "publisherNpub")]
    pub publisher_npub: String,
    #[serde(rename = "publishedAt")]
    pub published_at: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    pub sequence: u64,
    pub relays: Vec<String>,
    #[serde(rename = "stunServers")]
    pub stun_servers: Vec<String>,
    pub transports: Vec<String>,
    #[serde(rename = "endpointHint")]
    pub endpoint_hint: Option<EndpointHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalOffer {
    pub app: String,
    #[serde(rename = "eventKind")]
    pub event_kind: u16,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    pub nonce: String,
    #[serde(rename = "senderNpub")]
    pub sender_npub: String,
    #[serde(rename = "recipientNpub")]
    pub recipient_npub: String,
    #[serde(rename = "reflexiveAddress")]
    pub reflexive_address: Option<TraversalAddress>,
    #[serde(rename = "localAddresses")]
    pub local_addresses: Vec<TraversalAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalAnswer {
    pub app: String,
    #[serde(rename = "eventKind")]
    pub event_kind: u16,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    pub nonce: String,
    #[serde(rename = "senderNpub")]
    pub sender_npub: String,
    #[serde(rename = "recipientNpub")]
    pub recipient_npub: String,
    #[serde(rename = "inReplyTo")]
    pub in_reply_to: String,
    pub accepted: bool,
    #[serde(rename = "reflexiveAddress")]
    pub reflexive_address: Option<TraversalAddress>,
    #[serde(rename = "localAddresses")]
    pub local_addresses: Vec<TraversalAddress>,
    pub punch: Option<PunchHint>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRelays {
    pub relays: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyWants {
    #[serde(rename = "stunInfo")]
    pub stun_info: bool,
    #[serde(rename = "fipsConnect")]
    pub fips_connect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyPunch {
    #[serde(rename = "startAtMs")]
    pub start_at_ms: u64,
    #[serde(rename = "intervalMs")]
    pub interval_ms: u64,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyStunInfo {
    pub uri: String,
    #[serde(rename = "metadataTag")]
    pub metadata_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyHelloMessage {
    #[serde(rename = "type")]
    pub message_type: String,
    pub version: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub nonce: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    pub wants: LegacyWants,
    #[serde(rename = "clientEndpoint")]
    pub client_endpoint: Option<LegacyEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyServerInfoMessage {
    #[serde(rename = "type")]
    pub message_type: String,
    pub version: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub nonce: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    pub endpoint: LegacyEndpoint,
    pub punch: Option<LegacyPunch>,
    pub stun: Option<LegacyStunInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFrame {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "type")]
    pub frame_type: String,
    pub channel: Option<String>,
    pub payload: Value,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StunEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RendezvousError {
    #[error("invalid STUN url: {0}")]
    InvalidStunUrl(String),
    #[error("invalid punch packet length")]
    InvalidPunchPacketLength,
    #[error("invalid punch packet magic")]
    InvalidPunchPacketMagic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PunchPacketKind {
    Probe,
    Ack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PunchPacket {
    pub kind: PunchPacketKind,
    pub session_hash: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunchStrategy {
    Lan,
    Reflexive,
    Mixed,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    Local,
    Reflexive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPunchTarget {
    pub strategy: PunchStrategy,
    pub local_source: AddressSource,
    pub remote_source: AddressSource,
    pub local: TraversalAddress,
    pub remote: TraversalAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchWindow {
    pub start_at_ms: u64,
    pub interval_ms: u64,
    pub duration_ms: u64,
}
