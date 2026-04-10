mod rendezvous;
mod stun;
mod subscriptions;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use nostr::nips::nip19::ToBech32;
use nostr::{Keys, PublicKey};
use nostr_sdk::prelude::{Client, Options};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot, watch, Mutex, RwLock};

use crate::common::{now_ms, StunObservation};
use crate::{
    LegacyEndpoint, LegacyServerInfoMessage, SessionFrame, TraversalAdvert, TraversalAnswer,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub remote: LegacyEndpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaResponse {
    pub ok: bool,
    pub npub: String,
    #[serde(rename = "udpPort")]
    pub udp_port: u16,
    #[serde(rename = "advertRelays")]
    pub advert_relays: Vec<String>,
    #[serde(rename = "dmRelays")]
    pub dm_relays: Vec<String>,
    #[serde(rename = "discoveryEnabled")]
    pub discovery_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ConnectOutcome {
    pub target_npub: String,
    pub discovered_advert: Option<TraversalAdvert>,
    pub session_id: String,
    pub established_remote: LegacyEndpoint,
}

#[derive(Debug, Clone)]
pub struct ClientRuntimeParams {
    pub nsec: String,
    pub udp_port: u16,
    pub advert_relays: Vec<String>,
    pub dm_relays: Vec<String>,
    pub stun_servers: Vec<String>,
    pub public_host: Option<String>,
    pub discovery_enabled: bool,
    pub handoff_fips: bool,
}

pub struct ClientRuntimeCore {
    pub client: Client,
    pub udp_socket: Arc<UdpSocket>,
    pub keys: Keys,
    pub resolved_nsec: String,
    pub npub: String,
    pub pubkey: PublicKey,
    pub advert_relays: Vec<String>,
    pub dm_relays: Vec<String>,
    pub discovery_enabled: bool,
    pub handoff_fips: bool,
    inbox_lookup_relays: Vec<String>,
    public_host: Option<String>,
    stun_servers: Vec<String>,
    stun_timeout_ms: u64,
    stun_refresh_ms: u64,
    punch_interval_ms: u64,
    punch_duration_ms: u64,
    punch_start_delay_ms: u64,
    pending_stun: Mutex<HashMap<[u8; 12], oneshot::Sender<LegacyEndpoint>>>,
    stun_observation: RwLock<Option<StunObservation>>,
    stun_observed_at: Mutex<Option<Instant>>,
    advert_cache: RwLock<HashMap<String, TraversalAdvert>>,
    pending_answer: Mutex<HashMap<String, oneshot::Sender<TraversalAnswer>>>,
    pending_server_info: Mutex<HashMap<String, oneshot::Sender<LegacyServerInfoMessage>>>,
    pending_punch: Mutex<HashMap<String, oneshot::Sender<LegacyEndpoint>>>,
    punch_hashes: Mutex<HashMap<[u8; 16], String>>,
    active_session: RwLock<Option<ActiveSession>>,
    handoff_socket: Mutex<Option<std::net::UdpSocket>>,
    udp_shutdown: watch::Sender<bool>,
    events: broadcast::Sender<EventEnvelope>,
}

impl ClientRuntimeCore {
    pub async fn create(params: ClientRuntimeParams) -> Result<Arc<Self>> {
        let keys = Keys::parse(&params.nsec).context("invalid NOSTR_NSEC/--nsec")?;
        let client = Client::builder()
            .signer(keys.clone())
            .opts(Options::new().autoconnect(false).gossip(false))
            .build();

        let mut relay_union = HashSet::new();
        relay_union.extend(params.advert_relays.iter().cloned());
        relay_union.extend(params.dm_relays.iter().cloned());
        for relay in &relay_union {
            client.add_relay(relay).await?;
        }
        client.connect().await;

        let base_udp_socket = std::net::UdpSocket::bind(("0.0.0.0", params.udp_port))?;
        base_udp_socket.set_nonblocking(true)?;
        let udp_socket = Arc::new(UdpSocket::from_std(base_udp_socket.try_clone()?)?);
        let pubkey = keys.public_key();
        let npub = pubkey.to_bech32()?;
        let (event_tx, _) = broadcast::channel(256);
        let (udp_shutdown_tx, _) = watch::channel(false);
        let inbox_lookup_relays = {
            let mut set = HashSet::new();
            set.extend(params.dm_relays.iter().cloned());
            set.extend(params.advert_relays.iter().cloned());
            set.into_iter().collect::<Vec<_>>()
        };

        Ok(Arc::new(Self {
            client,
            udp_socket,
            keys,
            resolved_nsec: params.nsec,
            npub,
            pubkey,
            advert_relays: params.advert_relays,
            dm_relays: params.dm_relays,
            discovery_enabled: params.discovery_enabled,
            handoff_fips: params.handoff_fips,
            inbox_lookup_relays,
            public_host: params.public_host,
            stun_servers: params.stun_servers,
            stun_timeout_ms: 2_000,
            stun_refresh_ms: 60_000,
            punch_interval_ms: 300,
            punch_duration_ms: 30_000,
            punch_start_delay_ms: 3_000,
            pending_stun: Mutex::new(HashMap::new()),
            stun_observation: RwLock::new(None),
            stun_observed_at: Mutex::new(None),
            advert_cache: RwLock::new(HashMap::new()),
            pending_answer: Mutex::new(HashMap::new()),
            pending_server_info: Mutex::new(HashMap::new()),
            pending_punch: Mutex::new(HashMap::new()),
            punch_hashes: Mutex::new(HashMap::new()),
            active_session: RwLock::new(None),
            handoff_socket: Mutex::new(Some(base_udp_socket)),
            udp_shutdown: udp_shutdown_tx,
            events: event_tx,
        }))
    }

    pub fn meta_response(&self) -> MetaResponse {
        MetaResponse {
            ok: true,
            npub: self.npub.clone(),
            udp_port: self
                .udp_socket
                .local_addr()
                .map(|addr| addr.port())
                .unwrap_or(0),
            advert_relays: self.advert_relays.clone(),
            dm_relays: self.dm_relays.clone(),
            discovery_enabled: self.discovery_enabled,
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events.subscribe()
    }

    pub async fn emit(&self, event: &str, data: Value) {
        let _ = self.events.send(EventEnvelope {
            event: event.to_owned(),
            data,
        });
    }

    pub async fn current_status_value(&self) -> Value {
        let active = self.active_session.read().await.clone();
        if let Some(active) = active {
            json!({"connected": true, "sessionId": active.session_id, "remote": active.remote})
        } else {
            json!({"connected": false, "sessionId": null, "remote": null})
        }
    }

    pub async fn set_active_session(&self, session_id: String, remote: LegacyEndpoint) {
        *self.active_session.write().await = Some(ActiveSession {
            session_id: session_id.clone(),
            remote: remote.clone(),
        });
        self.emit(
            "status",
            json!({"connected": true, "sessionId": session_id, "remote": remote}),
        )
        .await;
    }

    pub async fn send_session_frame(
        &self,
        channel: &str,
        payload: Value,
        frame_type: &str,
    ) -> Result<()> {
        let active = self
            .active_session
            .read()
            .await
            .clone()
            .context("not connected")?;
        let frame = SessionFrame {
            session_id: active.session_id.clone(),
            frame_type: frame_type.to_owned(),
            channel: Some(channel.to_owned()),
            payload,
            at: now_ms(),
        };
        let bytes = crate::encode_session_frame(&frame)?;
        self.udp_socket
            .send_to(
                &bytes,
                SocketAddr::new(active.remote.host.parse()?, active.remote.port),
            )
            .await?;
        Ok(())
    }

    pub async fn active_session_matches(
        &self,
        session_id: &str,
        remote: SocketAddr,
    ) -> Option<ActiveSession> {
        let active = self.active_session.read().await.clone()?;
        if active.session_id != session_id {
            return None;
        }
        if remote.ip().to_string() != active.remote.host || remote.port() != active.remote.port {
            return None;
        }
        Some(active)
    }

    pub async fn take_handoff_socket(&self) -> Result<std::net::UdpSocket> {
        self.handoff_socket
            .lock()
            .await
            .take()
            .context("FIPS handoff socket already consumed")
    }

    pub async fn shutdown_udp(&self) {
        let _ = self.udp_shutdown.send(true);
    }
}
