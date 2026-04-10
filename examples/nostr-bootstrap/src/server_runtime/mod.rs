mod rendezvous;
mod stun;
mod subscriptions;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use nostr::nips::nip19::ToBech32;
use nostr::{Keys, PublicKey};
use nostr_sdk::prelude::{Client, Options};
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::common::StunObservation;
use crate::LegacyEndpoint;

#[derive(Debug, Clone)]
pub struct ServerRuntimeParams {
    pub nsec: String,
    pub udp_port: u16,
    pub advert_relays: Vec<String>,
    pub dm_relays: Vec<String>,
    pub stun_servers: Vec<String>,
    pub trusted_npubs: Vec<String>,
    pub public_host: Option<String>,
}

pub struct ServerRuntimeCore {
    pub client: Client,
    pub udp_socket: Arc<UdpSocket>,
    pub keys: Keys,
    pub resolved_nsec: String,
    pub npub: String,
    pub pubkey: PublicKey,
    pub advert_relays: Vec<String>,
    pub dm_relays: Vec<String>,
    pub trusted_npubs: HashSet<String>,
    pub advertise_interval_ms: u64,
    inbox_lookup_relays: Vec<String>,
    stun_servers: Vec<String>,
    public_host: Option<String>,
    punch_interval_ms: u64,
    punch_duration_ms: u64,
    punch_start_delay_ms: u64,
    advertise_ttl_ms: u64,
    stun_timeout_ms: u64,
    stun_refresh_ms: u64,
    pending_stun: Mutex<HashMap<[u8; 12], oneshot::Sender<LegacyEndpoint>>>,
    stun_observation: RwLock<Option<StunObservation>>,
    stun_observed_at: Mutex<Option<Instant>>,
    session_hashes: Mutex<HashMap<[u8; 16], String>>,
    pending_handoffs: Mutex<HashMap<String, String>>,
    handoff_socket: Mutex<Option<std::net::UdpSocket>>,
}

impl ServerRuntimeCore {
    pub async fn create(params: ServerRuntimeParams) -> Result<Arc<Self>> {
        let keys = Keys::parse(&params.nsec).context("invalid NOSTR_NSEC/--nsec")?;
        let client = Client::builder()
            .signer(keys.clone())
            .opts(Options::new().autoconnect(false).gossip(false))
            .build();

        let mut relay_union = HashSet::new();
        relay_union.extend(params.advert_relays.iter().cloned());
        relay_union.extend(params.dm_relays.iter().cloned());
        for relay in relay_union {
            client.add_relay(relay).await?;
        }
        client.connect().await;

        let base_udp_socket = std::net::UdpSocket::bind(("0.0.0.0", params.udp_port))?;
        base_udp_socket.set_nonblocking(true)?;
        let udp_socket = Arc::new(UdpSocket::from_std(base_udp_socket.try_clone()?)?);
        let pubkey = keys.public_key();
        let npub = pubkey.to_bech32()?;

        Ok(Arc::new(Self {
            client,
            udp_socket,
            keys,
            resolved_nsec: params.nsec,
            npub,
            pubkey,
            advert_relays: params.advert_relays.clone(),
            dm_relays: params.dm_relays.clone(),
            trusted_npubs: params
                .trusted_npubs
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect(),
            advertise_interval_ms: 5 * 60 * 1000,
            inbox_lookup_relays: {
                let mut set = HashSet::new();
                set.extend(params.advert_relays);
                set.extend(params.dm_relays);
                set.into_iter().collect()
            },
            stun_servers: params.stun_servers,
            public_host: params.public_host,
            punch_interval_ms: 300,
            punch_duration_ms: 30_000,
            punch_start_delay_ms: 3_000,
            advertise_ttl_ms: 10 * 60 * 1000,
            stun_timeout_ms: 2_000,
            stun_refresh_ms: 60 * 1000,
            pending_stun: Mutex::new(HashMap::new()),
            stun_observation: RwLock::new(None),
            stun_observed_at: Mutex::new(None),
            session_hashes: Mutex::new(HashMap::new()),
            pending_handoffs: Mutex::new(HashMap::new()),
            handoff_socket: Mutex::new(Some(base_udp_socket)),
        }))
    }

    pub async fn take_handoff_socket(&self) -> Result<std::net::UdpSocket> {
        self.handoff_socket
            .lock()
            .await
            .take()
            .context("FIPS handoff socket already consumed")
    }
}
