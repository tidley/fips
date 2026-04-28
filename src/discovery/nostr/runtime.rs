use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use nostr::nips::nip17;
use nostr::nips::nip19::ToBech32;
use nostr::prelude::{
    Alphabet, Event, EventBuilder, EventId, Filter, Kind, PublicKey, RelayUrl, SingleLetterTag,
    Tag, TagKind, Timestamp,
};
use nostr_sdk::{Client, Options, prelude::RelayPoolNotification};
use serde::Serialize;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::signal::{
    SignalEnvelope, build_peer_assist_probe, build_signal_event, create_assist_grant,
    create_assist_observed, create_assist_request, create_traversal_answer, create_traversal_offer,
    parse_peer_assist_probe, unwrap_signal_event, validate_assist_grant_for_request,
    validate_assist_observed_for_grant, validate_assist_request_freshness,
    validate_offer_freshness, validate_traversal_answer_for_offer,
};
use super::stun::observe_traversal_addresses;
use super::traversal::{nonce, now_ms, planned_remote_endpoints, run_punch_attempt};
use super::types::{
    ADVERT_IDENTIFIER, ADVERT_KIND, ADVERT_VERSION, AssistGrant, AssistObserved, AssistRequest,
    BootstrapError, BootstrapEvent, CachedOverlayAdvert, OverlayAdvert, OverlayEndpointAdvert,
    PROTOCOL_VERSION, PunchHint, SIGNAL_KIND, TraversalAddress, TraversalAnswer, TraversalOffer,
};
use crate::config::{NostrDiscoveryConfig, PeerAssistRequestPolicy, PeerConfig, UdpConfig};
use crate::discovery::EstablishedTraversal;

const MAX_CONCURRENT_INCOMING_OFFERS: usize = 16;
const ADVERT_CACHE_MAX_ENTRIES: usize = 2048;
const SEEN_SESSIONS_MAX_ENTRIES: usize = ADVERT_CACHE_MAX_ENTRIES;
const ADVERT_CACHE_STALE_GRACE_MULTIPLIER: u64 = 2;

#[derive(Debug, Clone)]
struct PendingPrivateAssistGrant {
    request_id: String,
    grant_id: String,
    grant_nonce: String,
    probe_token: String,
    sender_pubkey: PublicKey,
    sender_npub: String,
    helper_addr: String,
    relays: Vec<String>,
    expires_at: u64,
}

type RateLimitWindow = HashMap<String, Vec<u64>>;

pub struct NostrDiscovery {
    client: Client,
    keys: nostr::Keys,
    pubkey: PublicKey,
    npub: String,
    config: NostrDiscoveryConfig,
    advert_cache: RwLock<HashMap<String, CachedOverlayAdvert>>,
    local_advert: RwLock<Option<OverlayAdvert>>,
    current_advert_event_id: RwLock<Option<EventId>>,
    pending_answers: Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<TraversalAnswer>>>>,
    pending_assist_grants: Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<AssistGrant>>>>,
    pending_assist_observed:
        Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<AssistObserved>>>>,
    pending_private_assist_grants: Mutex<HashMap<String, PendingPrivateAssistGrant>>,
    helper_endpoints: RwLock<Vec<std::net::SocketAddr>>,
    assist_request_windows: Mutex<RateLimitWindow>,
    active_initiators: Mutex<HashSet<String>>,
    seen_sessions: Mutex<HashMap<String, u64>>,
    offer_slots: Arc<Semaphore>,
    event_tx: mpsc::UnboundedSender<BootstrapEvent>,
    event_rx: Mutex<mpsc::UnboundedReceiver<BootstrapEvent>>,
    notify_task: Mutex<Option<JoinHandle<()>>>,
    advertise_task: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    test_capture_outgoing: bool,
    #[cfg(test)]
    test_sent_signals: Mutex<Vec<String>>,
}

impl NostrDiscovery {
    pub async fn start(
        identity: &crate::Identity,
        config: NostrDiscoveryConfig,
    ) -> Result<Arc<Self>, BootstrapError> {
        if !config.enabled {
            return Err(BootstrapError::Disabled);
        }

        let keys = nostr::Keys::parse(&hex::encode(identity.keypair().secret_bytes()))
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        let client = Client::builder()
            .signer(keys.clone())
            .opts(Options::new().autoconnect(false).gossip(false))
            .build();

        let mut relay_union = HashSet::new();
        relay_union.extend(config.advert_relays.iter().cloned());
        relay_union.extend(config.dm_relays.iter().cloned());
        for relay in relay_union {
            client
                .add_relay(&relay)
                .await
                .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        }
        client.connect().await;

        let pubkey = keys.public_key();
        let npub = crate::encode_npub(&identity.pubkey());
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let runtime = Arc::new(Self {
            client,
            keys,
            pubkey,
            npub,
            config,
            advert_cache: RwLock::new(HashMap::new()),
            local_advert: RwLock::new(None),
            current_advert_event_id: RwLock::new(None),
            pending_answers: Mutex::new(HashMap::new()),
            pending_assist_grants: Mutex::new(HashMap::new()),
            pending_assist_observed: Mutex::new(HashMap::new()),
            pending_private_assist_grants: Mutex::new(HashMap::new()),
            helper_endpoints: RwLock::new(Vec::new()),
            assist_request_windows: Mutex::new(HashMap::new()),
            active_initiators: Mutex::new(HashSet::new()),
            seen_sessions: Mutex::new(HashMap::new()),
            offer_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_INCOMING_OFFERS)),
            event_tx,
            event_rx: Mutex::new(event_rx),
            notify_task: Mutex::new(None),
            advertise_task: Mutex::new(None),
            #[cfg(test)]
            test_capture_outgoing: false,
            #[cfg(test)]
            test_sent_signals: Mutex::new(Vec::new()),
        });

        runtime.subscribe().await?;
        runtime.publish_inbox_relays().await?;
        *runtime.advertise_task.lock().await = Some(runtime.clone().spawn_advertise_loop());
        *runtime.notify_task.lock().await = Some(runtime.clone().spawn_notify_loop());

        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        identity: &crate::Identity,
        config: NostrDiscoveryConfig,
    ) -> Arc<Self> {
        let keys = nostr::Keys::parse(&hex::encode(identity.keypair().secret_bytes()))
            .expect("parse nostr keys");
        let client = Client::builder()
            .signer(keys.clone())
            .opts(Options::new().autoconnect(false).gossip(false))
            .build();
        let pubkey = keys.public_key();
        let npub = crate::encode_npub(&identity.pubkey());
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            client,
            keys,
            pubkey,
            npub,
            config,
            advert_cache: RwLock::new(HashMap::new()),
            local_advert: RwLock::new(None),
            current_advert_event_id: RwLock::new(None),
            pending_answers: Mutex::new(HashMap::new()),
            pending_assist_grants: Mutex::new(HashMap::new()),
            pending_assist_observed: Mutex::new(HashMap::new()),
            pending_private_assist_grants: Mutex::new(HashMap::new()),
            helper_endpoints: RwLock::new(Vec::new()),
            assist_request_windows: Mutex::new(HashMap::new()),
            active_initiators: Mutex::new(HashSet::new()),
            seen_sessions: Mutex::new(HashMap::new()),
            offer_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_INCOMING_OFFERS)),
            event_tx,
            event_rx: Mutex::new(event_rx),
            notify_task: Mutex::new(None),
            advertise_task: Mutex::new(None),
            test_capture_outgoing: true,
            test_sent_signals: Mutex::new(Vec::new()),
        })
    }

    pub async fn request_connect(self: &Arc<Self>, peer_config: PeerConfig) {
        let peer_npub = peer_config.npub.clone();
        {
            let mut active = self.active_initiators.lock().await;
            if !active.insert(peer_npub.clone()) {
                return;
            }
        }

        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let event = match runtime.connect_peer(peer_config.clone()).await {
                Ok(traversal) => BootstrapEvent::Established { traversal },
                Err(err) => BootstrapEvent::Failed {
                    peer_config,
                    reason: err.to_string(),
                },
            };
            let _ = runtime.event_tx.send(event);
            runtime.active_initiators.lock().await.remove(&peer_npub);
        });
    }

    pub async fn drain_events(&self) -> Vec<BootstrapEvent> {
        let mut out = Vec::new();
        let mut rx = self.event_rx.lock().await;
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    #[cfg(test)]
    pub(crate) async fn drain_test_signals(&self) -> Vec<String> {
        let mut signals = self.test_sent_signals.lock().await;
        std::mem::take(&mut *signals)
    }

    pub async fn update_private_helper_endpoints(&self, mut endpoints: Vec<std::net::SocketAddr>) {
        endpoints.sort();
        endpoints.dedup();
        *self.helper_endpoints.write().await = endpoints;
    }

    #[cfg(test)]
    pub(crate) async fn connect_peer_via_private_assist_for_test(
        &self,
        peer_config: PeerConfig,
        advert: OverlayAdvert,
    ) -> Result<EstablishedTraversal, BootstrapError> {
        let target_pubkey =
            PublicKey::parse(&peer_config.npub).map_err(|e| BootstrapError::InvalidPeerNpub {
                npub: peer_config.npub.clone(),
                reason: e.to_string(),
            })?;
        let relays = if let Some(relays) = advert.signal_relays.clone() {
            relays
        } else {
            self.config.dm_relays.clone()
        };
        self.connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn handle_incoming_assist_request_for_test(
        self: Arc<Self>,
        request: AssistRequest,
        sender: PublicKey,
        sender_npub: String,
    ) -> Result<(), BootstrapError> {
        self.handle_incoming_assist_request(request, sender, sender_npub)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn inject_assist_grant_for_test(
        &self,
        grant: AssistGrant,
        sender_npub: String,
    ) {
        if let Some(tx) = self
            .pending_assist_grants
            .lock()
            .await
            .remove(&grant.in_reply_to)
        {
            let _ = tx.send(SignalEnvelope {
                payload: grant,
                event_id: EventId::all_zeros(),
                sender_npub,
            });
        }
    }

    #[cfg(test)]
    pub(crate) async fn inject_assist_observed_for_test(
        &self,
        observed: AssistObserved,
        sender_npub: String,
    ) {
        if let Some(tx) = self
            .pending_assist_observed
            .lock()
            .await
            .remove(&observed.in_reply_to)
        {
            let _ = tx.send(SignalEnvelope {
                payload: observed,
                event_id: EventId::all_zeros(),
                sender_npub,
            });
        }
    }

    pub async fn update_local_advert(
        &self,
        advert: Option<OverlayAdvert>,
    ) -> Result<(), BootstrapError> {
        let changed = {
            let mut slot = self.local_advert.write().await;
            if *slot == advert {
                false
            } else {
                *slot = advert;
                true
            }
        };
        if !changed {
            return Ok(());
        }
        self.publish_advert().await
    }

    pub async fn advert_endpoints_for_peer(
        &self,
        peer_npub: &str,
    ) -> Result<Vec<OverlayEndpointAdvert>, BootstrapError> {
        let target_pubkey =
            PublicKey::parse(peer_npub).map_err(|e| BootstrapError::InvalidPeerNpub {
                npub: peer_npub.to_string(),
                reason: e.to_string(),
            })?;
        let advert = self.fetch_advert(peer_npub, target_pubkey).await?;
        Ok(advert.endpoints)
    }

    pub async fn cached_open_discovery_candidates(
        &self,
        max: usize,
    ) -> Vec<(String, Vec<OverlayEndpointAdvert>)> {
        self.prune_advert_cache().await;
        let now = now_ms();
        let cache = self.advert_cache.read().await;
        cache
            .values()
            .filter(|entry| entry.author_npub != self.npub)
            .filter(|entry| entry.valid_until_ms > now)
            .map(|entry| (entry.author_npub.clone(), entry.advert.endpoints.clone()))
            .take(max)
            .collect()
    }

    pub async fn shutdown(&self) -> Result<(), BootstrapError> {
        if let Some(handle) = self.advertise_task.lock().await.take() {
            handle.abort();
        }

        let advert_event_id = self.current_advert_event_id.write().await.take();
        if let Some(event_id) = advert_event_id {
            self.publish_delete(&self.config.advert_relays, [event_id])
                .await?;
        }

        if let Some(handle) = self.notify_task.lock().await.take() {
            handle.abort();
        }

        Ok(())
    }

    fn spawn_notify_loop(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut notifications = self.client.notifications();
            while let Ok(notification) = notifications.recv().await {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if event.kind == Kind::Custom(ADVERT_KIND) {
                        if let Ok(author_npub) = event.pubkey.to_bech32() {
                            if let Some(valid_until_ms) = self.event_valid_until_ms(&event)
                                && let Ok(advert) =
                                    Self::parse_overlay_advert_event(&event, &self.config.app)
                            {
                                let mut cache = self.advert_cache.write().await;
                                let should_replace = cache
                                    .get(&author_npub)
                                    .map(|existing| {
                                        existing.created_at <= event.created_at.as_u64()
                                    })
                                    .unwrap_or(true);
                                if should_replace {
                                    cache.insert(
                                        author_npub.clone(),
                                        CachedOverlayAdvert {
                                            author_npub,
                                            advert,
                                            created_at: event.created_at.as_u64(),
                                            valid_until_ms,
                                        },
                                    );
                                }
                            }
                        }
                        self.prune_advert_cache().await;
                        continue;
                    }

                    if event.kind != Kind::Custom(SIGNAL_KIND) {
                        continue;
                    }

                    let unwrapped = match unwrap_signal_event(&self.keys, &event).await {
                        Ok(unwrapped) => unwrapped,
                        Err(err) => {
                            debug!(error = %err, "failed to unwrap traversal signal");
                            continue;
                        }
                    };
                    let sender_npub = match unwrapped.sender.to_bech32() {
                        Ok(npub) => npub,
                        Err(err) => {
                            debug!(error = %err, "failed to encode traversal sender npub");
                            continue;
                        }
                    };

                    if let Ok(answer) =
                        serde_json::from_str::<TraversalAnswer>(&unwrapped.rumor.content)
                        && answer.message_type == "answer"
                        && answer.recipient_npub == self.npub
                    {
                        if let Some(tx) = self
                            .pending_answers
                            .lock()
                            .await
                            .remove(&answer.in_reply_to)
                        {
                            let _ = tx.send(SignalEnvelope {
                                payload: answer,
                                event_id: event.id,
                                sender_npub: sender_npub.clone(),
                            });
                        }
                        continue;
                    }

                    if let Ok(grant) = serde_json::from_str::<AssistGrant>(&unwrapped.rumor.content)
                        && grant.message_type == "assist-grant"
                        && grant.recipient_npub == self.npub
                    {
                        if let Some(tx) = self
                            .pending_assist_grants
                            .lock()
                            .await
                            .remove(&grant.in_reply_to)
                        {
                            let _ = tx.send(SignalEnvelope {
                                payload: grant,
                                event_id: event.id,
                                sender_npub: sender_npub.clone(),
                            });
                        }
                        continue;
                    }

                    if let Ok(observed) =
                        serde_json::from_str::<AssistObserved>(&unwrapped.rumor.content)
                        && observed.message_type == "assist-observed"
                        && observed.recipient_npub == self.npub
                    {
                        if let Some(tx) = self
                            .pending_assist_observed
                            .lock()
                            .await
                            .remove(&observed.in_reply_to)
                        {
                            let _ = tx.send(SignalEnvelope {
                                payload: observed,
                                event_id: event.id,
                                sender_npub: sender_npub.clone(),
                            });
                        }
                        continue;
                    }

                    if let Ok(offer) =
                        serde_json::from_str::<TraversalOffer>(&unwrapped.rumor.content)
                        && offer.message_type == "offer"
                        && offer.recipient_npub == self.npub
                    {
                        let Ok(permit) = self.offer_slots.clone().try_acquire_owned() else {
                            warn!(sender_npub = %sender_npub, "dropping traversal offer because the inbound offer worker limit has been reached");
                            continue;
                        };
                        let runtime = Arc::clone(&self);
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(err) = runtime
                                .handle_incoming_offer(offer, unwrapped.sender, sender_npub)
                                .await
                            {
                                warn!(error = %err, "failed to handle traversal offer");
                            }
                        });
                        continue;
                    }

                    if let Ok(request) =
                        serde_json::from_str::<AssistRequest>(&unwrapped.rumor.content)
                        && request.message_type == "assist-request"
                        && request.recipient_npub == self.npub
                    {
                        let runtime = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(err) = runtime
                                .handle_incoming_assist_request(
                                    request,
                                    unwrapped.sender,
                                    sender_npub,
                                )
                                .await
                            {
                                warn!(error = %err, "failed to handle assist request");
                            }
                        });
                    }
                }
            }
        })
    }

    fn spawn_advertise_loop(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(self.config.advert_refresh_secs.max(1)));
            loop {
                interval.tick().await;
                if let Err(err) = self.publish_advert().await {
                    warn!(error = %err, "failed to refresh traversal advert");
                }
            }
        })
    }

    fn punch_hint(&self) -> PunchHint {
        PunchHint {
            start_at_ms: now_ms() + self.config.punch_start_delay_ms,
            interval_ms: self.config.punch_interval_ms,
            duration_ms: self.config.punch_duration_ms,
        }
    }

    async fn subscribe(&self) -> Result<(), BootstrapError> {
        self.client
            .subscribe_to(
                self.config.dm_relays.clone(),
                Filter::new()
                    .kind(Kind::Custom(SIGNAL_KIND))
                    .pubkey(self.pubkey)
                    .limit(0),
                None,
            )
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

        self.client
            .subscribe_to(
                self.config.advert_relays.clone(),
                Filter::new()
                    .kind(Kind::Custom(ADVERT_KIND))
                    .identifier(ADVERT_IDENTIFIER),
                None,
            )
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

        Ok(())
    }

    async fn publish_inbox_relays(&self) -> Result<(), BootstrapError> {
        let tags = self
            .config
            .dm_relays
            .iter()
            .filter_map(|relay| RelayUrl::parse(relay).ok())
            .map(|relay| {
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::R)),
                    [relay.to_string()],
                )
            })
            .collect::<Vec<_>>();

        let event = EventBuilder::new(Kind::InboxRelays, "")
            .tags(tags)
            .sign_with_keys(&self.keys)
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        self.client
            .send_event_to(self.config.dm_relays.clone(), event)
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        Ok(())
    }

    async fn publish_advert(&self) -> Result<(), BootstrapError> {
        let previous_event_id = self.current_advert_event_id.read().await.to_owned();
        if !self.config.advertise {
            if let Some(event_id) = previous_event_id {
                self.publish_delete(&self.config.advert_relays, [event_id])
                    .await?;
                *self.current_advert_event_id.write().await = None;
            }
            return Ok(());
        }

        let mut advert = match self.local_advert.read().await.clone() {
            Some(advert) => advert,
            None => {
                if let Some(event_id) = previous_event_id {
                    self.publish_delete(&self.config.advert_relays, [event_id])
                        .await?;
                    *self.current_advert_event_id.write().await = None;
                }
                return Ok(());
            }
        };

        advert.identifier = ADVERT_IDENTIFIER.to_string();
        advert.version = ADVERT_VERSION;
        if advert.endpoints.is_empty() {
            if let Some(event_id) = previous_event_id {
                self.publish_delete(&self.config.advert_relays, [event_id])
                    .await?;
                *self.current_advert_event_id.write().await = None;
            }
            return Ok(());
        }

        if advert.has_udp_nat_endpoint() {
            if advert
                .signal_relays
                .as_ref()
                .is_none_or(|relays| relays.is_empty())
            {
                return Err(BootstrapError::InvalidAdvert(
                    "udp:nat endpoint requires non-empty signalRelays".to_string(),
                ));
            }
            if !self.config.peer_assist.private_enabled()
                && advert
                    .stun_servers
                    .as_ref()
                    .is_none_or(|servers| servers.is_empty())
            {
                return Err(BootstrapError::InvalidAdvert(
                    "udp:nat endpoint requires non-empty stunServers".to_string(),
                ));
            }
        } else {
            advert.signal_relays = None;
            advert.stun_servers = None;
        }

        let expires_at = now_ms() + self.config.advert_ttl_secs * 1000;
        let tags = vec![
            Tag::identifier(ADVERT_IDENTIFIER.to_string()),
            Tag::custom(TagKind::custom("protocol"), [self.config.app.clone()]),
            Tag::custom(TagKind::custom("version"), [PROTOCOL_VERSION.to_string()]),
            Tag::expiration(Timestamp::from((expires_at / 1000).max(1))),
        ];

        let event = EventBuilder::new(Kind::Custom(ADVERT_KIND), serde_json::to_string(&advert)?)
            .tags(tags)
            .sign_with_keys(&self.keys)
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        self.client
            .send_event_to(self.config.advert_relays.clone(), event.clone())
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        if let Some(prev_id) = previous_event_id
            && prev_id != event.id
        {
            let _ = self
                .publish_delete(&self.config.advert_relays, [prev_id])
                .await;
        }
        *self.current_advert_event_id.write().await = Some(event.id);
        Ok(())
    }

    async fn connect_peer(
        &self,
        peer_config: PeerConfig,
    ) -> Result<EstablishedTraversal, BootstrapError> {
        let target_pubkey =
            PublicKey::parse(&peer_config.npub).map_err(|e| BootstrapError::InvalidPeerNpub {
                npub: peer_config.npub.clone(),
                reason: e.to_string(),
            })?;
        let advert = self.fetch_advert(&peer_config.npub, target_pubkey).await?;
        let relays = self
            .preferred_signal_relays(target_pubkey, Some(&advert))
            .await?;
        if relays.is_empty() {
            return Err(BootstrapError::MissingRelays(peer_config.npub));
        }

        let private_assist_enabled = self.config.peer_assist.private_enabled();
        if !advert.has_udp_nat_endpoint() {
            return Err(BootstrapError::MissingNatEndpoint(peer_config.npub));
        }

        if private_assist_enabled && self.config.stun_servers.is_empty() {
            return self
                .connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
                .await;
        }

        if private_assist_enabled
            && matches!(
                self.config.peer_assist.mode,
                crate::config::PeerAssistMode::PreferPrivate
            )
            && let Ok(traversal) = self
                .connect_peer_via_private_assist(peer_config.clone(), target_pubkey, &relays)
                .await
        {
            return Ok(traversal);
        }

        let base_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
        base_socket.set_nonblocking(true)?;

        let (reflexive_address, local_addresses, stun_server) =
            observe_traversal_addresses(&base_socket, &self.config.stun_servers).await?;
        let session_id = nonce();
        let offer = create_traversal_offer(
            session_id.clone(),
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            session_id.clone(),
            self.npub.clone(),
            peer_config.npub.clone(),
            reflexive_address.clone(),
            local_addresses,
            stun_server,
        );

        let (tx, rx) = oneshot::channel();
        self.pending_answers
            .lock()
            .await
            .insert(offer.nonce.clone(), tx);
        let offer_event = self.send_signal(&relays, target_pubkey, &offer).await?;

        let answer = match tokio::time::timeout(
            Duration::from_secs(self.config.signal_ttl_secs),
            rx,
        )
        .await
        {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => {
                let _ = self.pending_answers.lock().await.remove(&offer.nonce);
                return Err(BootstrapError::Protocol(
                    "answer channel closed".to_string(),
                ));
            }
            Err(_) => {
                let _ = self.pending_answers.lock().await.remove(&offer.nonce);
                return Err(BootstrapError::SignalTimeout(peer_config.npub));
            }
        };

        validate_traversal_answer_for_offer(
            &offer,
            &answer.payload,
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            &answer.sender_npub,
            &self.npub,
        )?;
        if !answer.payload.accepted {
            return Err(BootstrapError::Protocol(
                answer
                    .payload
                    .reason
                    .unwrap_or_else(|| "remote rejected traversal".to_string()),
            ));
        }

        let remotes = planned_remote_endpoints(
            &offer.local_addresses,
            offer.reflexive_address.as_ref(),
            &answer.payload.local_addresses,
            answer.payload.reflexive_address.as_ref(),
        )?;

        let remote_addr = run_punch_attempt(
            &base_socket,
            &session_id,
            &remotes,
            self.punch_hint(),
            Duration::from_secs(self.config.attempt_timeout_secs),
        )
        .await
        .map_err(|_| BootstrapError::PunchTimeout(peer_config.npub.clone()))?;

        let _ = self
            .publish_delete(&relays, [offer_event.id, answer.event_id])
            .await;

        Ok(
            EstablishedTraversal::new(session_id, peer_config.npub, remote_addr, base_socket)
                .with_public_endpoint(
                    reflexive_address
                        .as_ref()
                        .and_then(Self::traversal_address_to_socket)
                        .ok_or_else(|| {
                            BootstrapError::Protocol(
                                "missing reflexive address for established traversal".to_string(),
                            )
                        })?,
                )
                .with_transport_name("nostr-nat"),
        )
    }

    async fn connect_peer_via_private_assist(
        &self,
        peer_config: PeerConfig,
        target_pubkey: PublicKey,
        relays: &[String],
    ) -> Result<EstablishedTraversal, BootstrapError> {
        let base_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
        base_socket.set_nonblocking(true)?;

        let request_id = nonce();
        let request = create_assist_request(
            request_id.clone(),
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            nonce(),
            self.npub.clone(),
            peer_config.npub.clone(),
        );

        let (grant_tx, grant_rx) = oneshot::channel();
        self.pending_assist_grants
            .lock()
            .await
            .insert(request.nonce.clone(), grant_tx);
        let request_event = self.send_signal(relays, target_pubkey, &request).await?;

        let grant = match tokio::time::timeout(
            Duration::from_secs(self.config.peer_assist.grant_ttl_secs),
            grant_rx,
        )
        .await
        {
            Ok(Ok(grant)) => grant,
            Ok(Err(_)) => {
                let _ = self
                    .pending_assist_grants
                    .lock()
                    .await
                    .remove(&request.nonce);
                return Err(BootstrapError::Protocol(
                    "assist grant channel closed".to_string(),
                ));
            }
            Err(_) => {
                let _ = self
                    .pending_assist_grants
                    .lock()
                    .await
                    .remove(&request.nonce);
                return Err(BootstrapError::SignalTimeout(peer_config.npub));
            }
        };

        validate_assist_grant_for_request(
            &request,
            &grant.payload,
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            &grant.sender_npub,
            &self.npub,
        )?;
        if !grant.payload.accepted {
            return Err(BootstrapError::Protocol(
                grant
                    .payload
                    .reason
                    .unwrap_or_else(|| "remote rejected private assist".to_string()),
            ));
        }

        let helper_addr = grant
            .payload
            .helper_addr
            .as_deref()
            .ok_or_else(|| {
                BootstrapError::Protocol("missing helper endpoint in grant".to_string())
            })?
            .parse::<std::net::SocketAddr>()
            .map_err(|e| BootstrapError::Protocol(format!("invalid helper endpoint: {e}")))?;
        let grant_id = grant.payload.grant_id.clone();
        let probe_token =
            grant.payload.probe_token.clone().ok_or_else(|| {
                BootstrapError::Protocol("missing probe token in grant".to_string())
            })?;

        let (observed_tx, observed_rx) = oneshot::channel();
        self.pending_assist_observed
            .lock()
            .await
            .insert(grant.payload.nonce.clone(), observed_tx);

        let probe_socket = base_socket.try_clone()?;
        let probe_task = tokio::spawn(async move {
            let probe = build_peer_assist_probe(&grant_id, &probe_token);
            loop {
                let _ = probe_socket.send_to(&probe, helper_addr);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });

        let observed = match tokio::time::timeout(
            Duration::from_secs(self.config.peer_assist.grant_ttl_secs),
            observed_rx,
        )
        .await
        {
            Ok(Ok(observed)) => observed,
            Ok(Err(_)) => {
                let _ = self
                    .pending_assist_observed
                    .lock()
                    .await
                    .remove(&grant.payload.nonce);
                probe_task.abort();
                return Err(BootstrapError::Protocol(
                    "assist observed channel closed".to_string(),
                ));
            }
            Err(_) => {
                let _ = self
                    .pending_assist_observed
                    .lock()
                    .await
                    .remove(&grant.payload.nonce);
                probe_task.abort();
                return Err(BootstrapError::SignalTimeout(peer_config.npub));
            }
        };
        probe_task.abort();

        validate_assist_observed_for_grant(
            &grant.payload,
            &observed.payload,
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            &observed.sender_npub,
            &self.npub,
        )?;
        if !observed.payload.accepted {
            return Err(BootstrapError::Protocol(
                observed
                    .payload
                    .reason
                    .unwrap_or_else(|| "remote rejected observed endpoint".to_string()),
            ));
        }

        let _ = self
            .publish_delete(
                relays,
                [request_event.id, grant.event_id, observed.event_id],
            )
            .await;

        let observed_endpoint = observed
            .payload
            .observed_address
            .as_ref()
            .and_then(Self::traversal_address_to_socket)
            .ok_or_else(|| {
                BootstrapError::Protocol(
                    "missing observed address for accepted peer-assist answer".to_string(),
                )
            })?;

        Ok(
            EstablishedTraversal::new(request_id, peer_config.npub, helper_addr, base_socket)
                .with_public_endpoint(observed_endpoint)
                .with_transport_name("nostr-assist")
                .with_transport_config(UdpConfig {
                    peer_assist: Some(true),
                    ..Default::default()
                }),
        )
    }

    async fn handle_incoming_offer(
        self: Arc<Self>,
        offer: TraversalOffer,
        sender: PublicKey,
        sender_npub: String,
    ) -> Result<(), BootstrapError> {
        validate_offer_freshness(
            &offer,
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            &sender_npub,
            &self.npub,
        )?;
        self.mark_session_seen(&offer.session_id).await?;

        let base_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
        base_socket.set_nonblocking(true)?;
        let (reflexive_address, local_addresses, stun_server) =
            observe_traversal_addresses(&base_socket, &self.config.stun_servers).await?;
        let accepted = reflexive_address.is_some() || !local_addresses.is_empty();
        let answer = create_traversal_answer(
            offer.session_id.clone(),
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            nonce(),
            self.npub.clone(),
            offer.sender_npub.clone(),
            offer.nonce.clone(),
            accepted,
            reflexive_address.clone(),
            local_addresses,
            stun_server,
            accepted.then(|| self.punch_hint()),
            (!accepted).then_some("no-usable-addresses".to_string()),
        );
        let relays = self.preferred_signal_relays(sender, None).await?;
        let answer_event = self.send_signal(&relays, sender, &answer).await?;
        if !accepted {
            let _ = self.publish_delete(&relays, [answer_event.id]).await;
            return Ok(());
        }

        let remotes = planned_remote_endpoints(
            &answer.local_addresses,
            answer.reflexive_address.as_ref(),
            &offer.local_addresses,
            offer.reflexive_address.as_ref(),
        )?;

        if let Ok(remote_addr) = run_punch_attempt(
            &base_socket,
            &offer.session_id,
            &remotes,
            answer
                .punch
                .clone()
                .expect("accepted answers always include a punch hint"),
            Duration::from_secs(self.config.attempt_timeout_secs),
        )
        .await
        {
            let _ = self.event_tx.send(BootstrapEvent::Established {
                traversal: EstablishedTraversal::new(
                    offer.session_id,
                    offer.sender_npub,
                    remote_addr,
                    base_socket,
                )
                .with_public_endpoint(
                    reflexive_address
                        .as_ref()
                        .and_then(Self::traversal_address_to_socket)
                        .unwrap_or(remote_addr),
                )
                .with_transport_name("nostr-nat"),
            });
        }

        let _ = self.publish_delete(&relays, [answer_event.id]).await;
        Ok(())
    }

    async fn handle_incoming_assist_request(
        self: Arc<Self>,
        request: AssistRequest,
        sender: PublicKey,
        sender_npub: String,
    ) -> Result<(), BootstrapError> {
        validate_assist_request_freshness(
            &request,
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            &sender_npub,
            &self.npub,
        )?;
        self.mark_session_seen(&request.request_id).await?;

        let relays = self.preferred_signal_relays(sender, None).await?;
        if !self.config.peer_assist.private_enabled() {
            let grant = create_assist_grant(
                request.request_id,
                nonce(),
                now_ms(),
                self.config.peer_assist.grant_ttl_secs * 1000,
                nonce(),
                self.npub.clone(),
                request.sender_npub,
                request.nonce,
                false,
                None,
                None,
                None,
                Some("peer-assist disabled".to_string()),
            );
            let _ = self.send_signal(&relays, sender, &grant).await?;
            return Ok(());
        }

        if !self.requester_allowed(&sender_npub).await {
            let grant = create_assist_grant(
                request.request_id,
                nonce(),
                now_ms(),
                self.config.peer_assist.grant_ttl_secs * 1000,
                nonce(),
                self.npub.clone(),
                request.sender_npub,
                request.nonce,
                false,
                None,
                None,
                None,
                Some("peer-assist requester not allowed".to_string()),
            );
            let _ = self.send_signal(&relays, sender, &grant).await?;
            return Ok(());
        }

        let helper_addr = self
            .helper_endpoints
            .read()
            .await
            .first()
            .copied()
            .ok_or_else(|| BootstrapError::Protocol("no eligible helper transport".to_string()))?;

        let pending_count = self.pending_private_assist_grants.lock().await.len();
        if pending_count >= self.config.peer_assist.max_pending_requests {
            let grant = create_assist_grant(
                request.request_id,
                nonce(),
                now_ms(),
                self.config.peer_assist.grant_ttl_secs * 1000,
                nonce(),
                self.npub.clone(),
                request.sender_npub,
                request.nonce,
                false,
                None,
                None,
                None,
                Some("peer-assist helper is at capacity".to_string()),
            );
            let _ = self.send_signal(&relays, sender, &grant).await?;
            return Ok(());
        }

        let grant_id = nonce();
        let grant_nonce = nonce();
        let probe_token = nonce();
        self.pending_private_assist_grants.lock().await.insert(
            grant_id.clone(),
            PendingPrivateAssistGrant {
                request_id: request.request_id.clone(),
                grant_id: grant_id.clone(),
                grant_nonce: grant_nonce.clone(),
                probe_token: probe_token.clone(),
                sender_pubkey: sender,
                sender_npub: sender_npub.clone(),
                helper_addr: helper_addr.to_string(),
                relays: relays.clone(),
                expires_at: now_ms() + self.config.peer_assist.grant_ttl_secs * 1000,
            },
        );

        let grant = create_assist_grant(
            request.request_id,
            grant_id,
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            grant_nonce,
            self.npub.clone(),
            request.sender_npub,
            request.nonce,
            true,
            Some(helper_addr.to_string()),
            Some(probe_token),
            Some(1),
            None,
        );
        let _ = self.send_signal(&relays, sender, &grant).await?;
        Ok(())
    }

    pub async fn observe_peer_assist_probe(
        &self,
        helper_addr: std::net::SocketAddr,
        remote_addr: std::net::SocketAddr,
        data: &[u8],
    ) -> bool {
        let Some(probe) = parse_peer_assist_probe(data) else {
            return false;
        };
        let now = now_ms();
        let pending = {
            let mut pending = self.pending_private_assist_grants.lock().await;
            pending.retain(|_, entry| entry.expires_at > now);
            let Some(entry) = pending.remove(&probe.grant_id) else {
                return false;
            };
            if entry.helper_addr != helper_addr.to_string() || entry.probe_token != probe.token {
                pending.insert(entry.grant_id.clone(), entry);
                return false;
            }
            entry
        };

        let observed = create_assist_observed(
            pending.request_id,
            pending.grant_id,
            now,
            self.config.peer_assist.grant_ttl_secs * 1000,
            nonce(),
            self.npub.clone(),
            pending.sender_npub.clone(),
            pending.grant_nonce,
            true,
            helper_addr.to_string(),
            Some(TraversalAddress {
                protocol: "udp".to_string(),
                ip: remote_addr.ip().to_string(),
                port: remote_addr.port(),
            }),
            None,
        );
        match self
            .send_signal(&pending.relays, pending.sender_pubkey, &observed)
            .await
        {
            Ok(_) => true,
            Err(err) => {
                debug!(
                    sender_npub = %pending.sender_npub,
                    error = %err,
                    "failed to send peer-assist answer"
                );
                false
            }
        }
    }

    async fn requester_allowed(&self, sender_npub: &str) -> bool {
        match self.config.peer_assist.request_policy {
            PeerAssistRequestPolicy::Allowlist => self
                .config
                .peer_assist
                .request_allowlist
                .iter()
                .any(|allowed| allowed == sender_npub),
            PeerAssistRequestPolicy::OpenRateLimited => {
                let now = now_ms();
                let window_ms = self
                    .config
                    .peer_assist
                    .request_window_secs
                    .saturating_mul(1000);
                let mut windows = self.assist_request_windows.lock().await;
                let entry = windows.entry(sender_npub.to_string()).or_default();
                entry.retain(|timestamp| now.saturating_sub(*timestamp) <= window_ms);
                if entry.len() >= self.config.peer_assist.max_requests_per_peer_per_window {
                    return false;
                }
                entry.push(now);
                true
            }
        }
    }

    fn traversal_address_to_socket(addr: &TraversalAddress) -> Option<std::net::SocketAddr> {
        if !addr.protocol.eq_ignore_ascii_case("udp") {
            return None;
        }
        format!("{}:{}", addr.ip, addr.port).parse().ok()
    }

    async fn fetch_advert(
        &self,
        peer_npub: &str,
        target_pubkey: PublicKey,
    ) -> Result<OverlayAdvert, BootstrapError> {
        self.prune_advert_cache().await;
        if let Some(cached) = self.advert_cache.read().await.get(peer_npub).cloned() {
            return Ok(cached.advert);
        }

        let events = self
            .client
            .fetch_events_from(
                self.config.advert_relays.clone(),
                Filter::new()
                    .author(target_pubkey)
                    .kind(Kind::Custom(ADVERT_KIND))
                    .identifier(ADVERT_IDENTIFIER),
                Duration::from_secs(2),
            )
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

        let mut best: Option<CachedOverlayAdvert> = None;
        for event in events.iter() {
            let Some(valid_until_ms) = self.event_valid_until_ms(event) else {
                continue;
            };
            let Ok(advert) = Self::parse_overlay_advert_event(event, &self.config.app) else {
                continue;
            };
            let Ok(author_npub) = event.pubkey.to_bech32() else {
                continue;
            };
            if author_npub != peer_npub {
                continue;
            }
            let replace = best
                .as_ref()
                .map(|current| event.created_at.as_u64() >= current.created_at)
                .unwrap_or(true);
            if replace {
                best = Some(CachedOverlayAdvert {
                    author_npub,
                    advert,
                    created_at: event.created_at.as_u64(),
                    valid_until_ms,
                });
            }
        }

        let cached = best.ok_or_else(|| BootstrapError::MissingAdvert(peer_npub.to_string()))?;
        self.advert_cache
            .write()
            .await
            .insert(peer_npub.to_string(), cached.clone());
        self.prune_advert_cache().await;
        Ok(cached.advert)
    }

    async fn preferred_signal_relays(
        &self,
        target_pubkey: PublicKey,
        advert: Option<&OverlayAdvert>,
    ) -> Result<Vec<String>, BootstrapError> {
        let mut merged = self.find_recipient_inbox_relays(target_pubkey).await?;
        if let Some(advert) = advert {
            if let Some(relays) = advert.signal_relays.as_ref() {
                for relay in relays {
                    if !merged.contains(relay) {
                        merged.push(relay.clone());
                    }
                }
            }
        }
        for relay in &self.config.dm_relays {
            if !merged.contains(relay) {
                merged.push(relay.clone());
            }
        }
        Ok(merged)
    }

    async fn find_recipient_inbox_relays(
        &self,
        target_pubkey: PublicKey,
    ) -> Result<Vec<String>, BootstrapError> {
        let mut lookup_relays = self.config.dm_relays.clone();
        for relay in &self.config.advert_relays {
            if !lookup_relays.contains(relay) {
                lookup_relays.push(relay.clone());
            }
        }
        let events = self
            .client
            .fetch_events_from(
                lookup_relays,
                Filter::new()
                    .author(target_pubkey)
                    .kind(Kind::InboxRelays)
                    .since(Timestamp::from(
                        Timestamp::now().as_u64().saturating_sub(30 * 24 * 60 * 60),
                    )),
                Duration::from_millis(1500),
            )
            .await;
        let events = match events {
            Ok(events) => events,
            Err(err) => {
                debug!(error = %err, "failed to fetch inbox relays, falling back to configured DM relays");
                return Ok(self.config.dm_relays.clone());
            }
        };
        let newest = events.iter().max_by_key(|event| event.created_at.as_u64());
        if let Some(event) = newest {
            let relays = nip17::extract_relay_list(event)
                .map(|relay| relay.to_string())
                .collect::<Vec<_>>();
            if !relays.is_empty() {
                return Ok(relays);
            }
        }
        Ok(self.config.dm_relays.clone())
    }

    fn parse_overlay_advert_event(
        event: &Event,
        expected_app: &str,
    ) -> Result<OverlayAdvert, BootstrapError> {
        let advertised_app = event
            .tags
            .find(TagKind::custom("protocol"))
            .and_then(|tag| tag.content())
            .ok_or_else(|| {
                BootstrapError::InvalidAdvert("missing required protocol tag".to_string())
            })?;
        if advertised_app != expected_app {
            return Err(BootstrapError::InvalidAdvert(format!(
                "unsupported protocol '{}'",
                advertised_app
            )));
        }

        let advert: OverlayAdvert = serde_json::from_str(&event.content)?;
        Self::validate_overlay_advert(advert)
    }

    pub(super) fn validate_overlay_advert(
        mut advert: OverlayAdvert,
    ) -> Result<OverlayAdvert, BootstrapError> {
        if advert.identifier != ADVERT_IDENTIFIER {
            return Err(BootstrapError::InvalidAdvert(format!(
                "unsupported identifier '{}'",
                advert.identifier
            )));
        }
        if advert.version != ADVERT_VERSION {
            return Err(BootstrapError::InvalidAdvert(format!(
                "unsupported version '{}'",
                advert.version
            )));
        }
        if advert.endpoints.is_empty() {
            return Err(BootstrapError::InvalidAdvert(
                "missing required endpoints".to_string(),
            ));
        }
        for endpoint in &advert.endpoints {
            if endpoint.addr.trim().is_empty() {
                return Err(BootstrapError::InvalidAdvert(
                    "endpoint addr cannot be empty".to_string(),
                ));
            }
        }

        let has_nat = advert.has_udp_nat_endpoint();
        if has_nat {
            if advert
                .signal_relays
                .as_ref()
                .is_none_or(|relays| relays.is_empty())
            {
                return Err(BootstrapError::InvalidAdvert(
                    "udp:nat endpoint requires signalRelays".to_string(),
                ));
            }
        } else {
            advert.signal_relays = None;
            advert.stun_servers = None;
        }

        Ok(advert)
    }

    async fn prune_advert_cache(&self) {
        let now = now_ms();
        let mut cache = self.advert_cache.write().await;
        cache.retain(|_, entry| entry.valid_until_ms > now);
        if cache.len() <= ADVERT_CACHE_MAX_ENTRIES {
            return;
        }

        let mut oldest = cache
            .iter()
            .map(|(npub, entry)| (npub.clone(), entry.valid_until_ms))
            .collect::<Vec<_>>();
        oldest.sort_by_key(|(_, ts)| *ts);
        let overflow = cache.len().saturating_sub(ADVERT_CACHE_MAX_ENTRIES);
        for (npub, _) in oldest.into_iter().take(overflow) {
            cache.remove(&npub);
        }
    }

    fn advert_max_age_ms(&self) -> u64 {
        self.config.advert_ttl_secs * 1000 * ADVERT_CACHE_STALE_GRACE_MULTIPLIER
    }

    fn event_valid_until_ms(&self, event: &Event) -> Option<u64> {
        Self::compute_advert_valid_until_ms(event, self.advert_max_age_ms(), now_ms())
    }

    pub(super) fn compute_advert_valid_until_ms(
        event: &Event,
        advert_max_age_ms: u64,
        now_ms: u64,
    ) -> Option<u64> {
        if event.is_expired() {
            return None;
        }

        let created_ms = event.created_at.as_u64().saturating_mul(1000);
        let created_window_until = created_ms.saturating_add(advert_max_age_ms);
        if created_window_until <= now_ms {
            return None;
        }

        let expires_ms = event
            .tags
            .expiration()
            .map(|timestamp| timestamp.as_u64().saturating_mul(1000));
        let valid_until_ms = expires_ms
            .map(|expires| expires.min(created_window_until))
            .unwrap_or(created_window_until);

        (valid_until_ms > now_ms).then_some(valid_until_ms)
    }

    async fn send_signal<T: Serialize>(
        &self,
        relays: &[String],
        receiver: PublicKey,
        payload: &T,
    ) -> Result<Event, BootstrapError> {
        let rumor = EventBuilder::private_msg_rumor(receiver, serde_json::to_string(payload)?)
            .build(self.pubkey);
        let signal = build_signal_event(
            &self.keys,
            receiver,
            rumor,
            Timestamp::from((now_ms() + self.config.signal_ttl_secs * 1000) / 1000),
        )
        .await?;
        #[cfg(test)]
        if self.test_capture_outgoing {
            self.test_sent_signals
                .lock()
                .await
                .push(serde_json::to_string(payload)?);
            return Ok(signal);
        }
        self.client
            .send_event_to(relays.to_vec(), signal.clone())
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        Ok(signal)
    }

    async fn publish_delete<I>(&self, relays: &[String], ids: I) -> Result<(), BootstrapError>
    where
        I: IntoIterator<Item = EventId>,
    {
        let event = EventBuilder::delete(ids)
            .sign_with_keys(&self.keys)
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        self.client
            .send_event_to(relays.to_vec(), event)
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        Ok(())
    }

    async fn mark_session_seen(&self, session_id: &str) -> Result<(), BootstrapError> {
        let now = now_ms();
        let expiry = now + self.config.replay_window_secs * 1000;
        let mut seen = self.seen_sessions.lock().await;
        seen.retain(|_, expires_at| *expires_at > now);
        if seen.contains_key(session_id) {
            return Err(BootstrapError::Replay(session_id.to_string()));
        }
        seen.insert(session_id.to_string(), expiry);
        if seen.len() > SEEN_SESSIONS_MAX_ENTRIES {
            let mut oldest = seen
                .iter()
                .map(|(session, expires_at)| (session.clone(), *expires_at))
                .collect::<Vec<_>>();
            oldest.sort_by_key(|(_, expires_at)| *expires_at);
            let overflow = seen.len().saturating_sub(SEEN_SESSIONS_MAX_ENTRIES);
            for (session, _) in oldest.into_iter().take(overflow) {
                seen.remove(&session);
            }
        }
        Ok(())
    }
}
