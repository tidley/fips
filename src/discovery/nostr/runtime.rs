use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use nostr::nips::nip17;
use nostr::nips::nip19::ToBech32;
use nostr::prelude::{
    Alphabet, Event, EventBuilder, EventId, Filter, Kind, PublicKey, RelayUrl, SingleLetterTag,
    Tag, TagKind, Timestamp,
};
use nostr_sdk::{Client, ClientOptions, prelude::RelayPoolNotification};
use serde::Serialize;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, trace, warn};

use super::assist::{
    PeerAssistState, PendingPrivateAssistGrant, RateLimitWindow, allow_in_rate_window,
};
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

const ADVERT_CACHE_STALE_GRACE_MULTIPLIER: u64 = 2;

fn short_npub(npub: &str) -> String {
    npub.strip_prefix("npub1")
        .filter(|s| s.len() >= 8)
        .map(|s| format!("npub1{}..{}", &s[..4], &s[s.len() - 4..]))
        .unwrap_or_else(|| npub.to_string())
}

fn short_id(id: &str) -> String {
    if id.len() > 8 {
        id[..8].to_string()
    } else {
        id.to_string()
    }
}

fn endpoint_summary(endpoints: &[OverlayEndpointAdvert]) -> String {
    endpoints
        .iter()
        .map(|e| format!("{:?}:{}", e.transport, e.addr).to_lowercase())
        .collect::<Vec<_>>()
        .join(",")
}

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
    peer_assist: PeerAssistState,
    traversal_offer_windows: Mutex<RateLimitWindow>,
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
    #[cfg(test)]
    test_sent_deletes: Mutex<Vec<Vec<String>>>,
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
            .opts(ClientOptions::new().autoconnect(false))
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
        let offer_slots = Arc::new(Semaphore::new(config.max_concurrent_incoming_offers));

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
            peer_assist: PeerAssistState::new(),
            traversal_offer_windows: Mutex::new(HashMap::new()),
            active_initiators: Mutex::new(HashSet::new()),
            seen_sessions: Mutex::new(HashMap::new()),
            offer_slots,
            event_tx,
            event_rx: Mutex::new(event_rx),
            notify_task: Mutex::new(None),
            advertise_task: Mutex::new(None),
            #[cfg(test)]
            test_capture_outgoing: false,
            #[cfg(test)]
            test_sent_signals: Mutex::new(Vec::new()),
            #[cfg(test)]
            test_sent_deletes: Mutex::new(Vec::new()),
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
            .opts(ClientOptions::new().autoconnect(false))
            .build();
        let pubkey = keys.public_key();
        let npub = crate::encode_npub(&identity.pubkey());
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let offer_slots = Arc::new(Semaphore::new(config.max_concurrent_incoming_offers));
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
            peer_assist: PeerAssistState::new(),
            traversal_offer_windows: Mutex::new(HashMap::new()),
            active_initiators: Mutex::new(HashSet::new()),
            seen_sessions: Mutex::new(HashMap::new()),
            offer_slots,
            event_tx,
            event_rx: Mutex::new(event_rx),
            notify_task: Mutex::new(None),
            advertise_task: Mutex::new(None),
            test_capture_outgoing: true,
            test_sent_signals: Mutex::new(Vec::new()),
            test_sent_deletes: Mutex::new(Vec::new()),
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

    #[cfg(test)]
    pub(crate) async fn drain_test_deletes(&self) -> Vec<Vec<String>> {
        let mut deletes = self.test_sent_deletes.lock().await;
        std::mem::take(&mut *deletes)
    }

    #[cfg(test)]
    pub(crate) async fn current_advert_event_id_for_test(&self) -> Option<EventId> {
        self.current_advert_event_id.read().await.to_owned()
    }

    #[cfg(test)]
    pub(crate) async fn inject_advert_for_test(&self, peer_npub: String, advert: OverlayAdvert) {
        let now = now_ms();
        self.advert_cache.write().await.insert(
            peer_npub.clone(),
            CachedOverlayAdvert {
                author_npub: peer_npub,
                advert,
                created_at: now,
                valid_until_ms: now.saturating_add(self.advert_max_age_ms()),
            },
        );
    }

    pub async fn update_private_helper_endpoints(&self, endpoints: Vec<std::net::SocketAddr>) {
        self.peer_assist.update_helper_endpoints(endpoints).await;
    }

    #[cfg(test)]
    pub(crate) async fn connect_peer_for_test(
        &self,
        peer_config: PeerConfig,
    ) -> Result<EstablishedTraversal, BootstrapError> {
        self.connect_peer(peer_config).await
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
        let in_reply_to = grant.in_reply_to.clone();
        self.peer_assist
            .complete_grant(
                &in_reply_to,
                SignalEnvelope {
                    payload: grant,
                    event_id: EventId::all_zeros(),
                    sender_npub,
                },
            )
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn inject_assist_observed_for_test(
        &self,
        observed: AssistObserved,
        sender_npub: String,
    ) {
        let in_reply_to = observed.in_reply_to.clone();
        self.peer_assist
            .complete_observed(
                &in_reply_to,
                SignalEnvelope {
                    payload: observed,
                    event_id: EventId::all_zeros(),
                    sender_npub,
                },
            )
            .await;
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

        // Don't proactively retract the advert via NIP-09 on shutdown.
        // Parameterized-replaceable semantics handle restart supersedence,
        // and NIP-40 expiration (advert_ttl_secs) bounds staleness on
        // permanent shutdown. An explicit retraction races with the next
        // daemon's republish on strict relays (e.g. Damus rate-limits the
        // burst, leaving the advert deleted and never restored).
        let _ = self.current_advert_event_id.write().await.take();

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
                        let author_npub = event.pubkey.to_bech32().expect("infallible");
                        if let Some(valid_until_ms) = self.event_valid_until_ms(&event)
                            && let Ok(advert) =
                                Self::parse_overlay_advert_event(&event, &self.config.app)
                        {
                            let mut cache = self.advert_cache.write().await;
                            let should_replace = cache
                                .get(&author_npub)
                                .map(|existing| existing.created_at <= event.created_at.as_secs())
                                .unwrap_or(true);
                            if should_replace && author_npub != self.npub {
                                debug!(
                                    peer = %short_npub(&author_npub),
                                    endpoints = %endpoint_summary(&advert.endpoints),
                                    event = %short_id(&event.id.to_string()),
                                    "advert: peer cached"
                                );
                            }
                            if should_replace {
                                cache.insert(
                                    author_npub.clone(),
                                    CachedOverlayAdvert {
                                        author_npub,
                                        advert,
                                        created_at: event.created_at.as_secs(),
                                        valid_until_ms,
                                    },
                                );
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
                            trace!(error = %err, "failed to unwrap traversal signal");
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
                        let in_reply_to = grant.in_reply_to.clone();
                        self.peer_assist
                            .complete_grant(
                                &in_reply_to,
                                SignalEnvelope {
                                    payload: grant,
                                    event_id: event.id,
                                    sender_npub: sender_npub.clone(),
                                },
                            )
                            .await;
                        continue;
                    }

                    if let Ok(observed) =
                        serde_json::from_str::<AssistObserved>(&unwrapped.rumor.content)
                        && observed.message_type == "assist-observed"
                        && observed.recipient_npub == self.npub
                    {
                        let in_reply_to = observed.in_reply_to.clone();
                        self.peer_assist
                            .complete_observed(
                                &in_reply_to,
                                SignalEnvelope {
                                    payload: observed,
                                    event_id: event.id,
                                    sender_npub: sender_npub.clone(),
                                },
                            )
                            .await;
                        continue;
                    }

                    if let Ok(offer) =
                        serde_json::from_str::<TraversalOffer>(&unwrapped.rumor.content)
                        && offer.message_type == "offer"
                        && offer.recipient_npub == self.npub
                    {
                        if !self.traversal_offer_allowed(&sender_npub).await {
                            warn!(sender_npub = %sender_npub, "dropping traversal offer because sender exceeded the rate limit");
                            continue;
                        }
                        let Ok(permit) = self.offer_slots.clone().try_acquire_owned() else {
                            warn!(
                                sender_npub = %sender_npub,
                                limit = self.config.max_concurrent_incoming_offers,
                                "rate-limited inbound traversal offer (max_concurrent_incoming_offers reached); offer dropped"
                            );
                            continue;
                        };
                        let runtime = Arc::clone(&self);
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(err) = runtime
                                .handle_incoming_offer(offer, unwrapped.sender, sender_npub)
                                .await
                            {
                                debug!(error = %err, "failed to handle traversal offer");
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
            // Swallow the immediate first tick: Node::start() already publishes
            // the initial advert via refresh_overlay_advert().
            interval.tick().await;
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
            .send_event_to(self.config.dm_relays.clone(), &event)
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        Ok(())
    }

    async fn publish_advert(&self) -> Result<(), BootstrapError> {
        if !self.config.advertise {
            return self.retract_current_advert().await;
        }

        let mut advert = match self.local_advert.read().await.clone() {
            Some(advert) => advert,
            // No eligible local endpoints remain. Retract the previous advert
            // so peers do not keep discovering stale udp:nat helper metadata.
            None => return self.retract_current_advert().await,
        };

        advert.identifier = ADVERT_IDENTIFIER.to_string();
        advert.version = ADVERT_VERSION;
        // Defensive: build_overlay_advert returns None on empty endpoints,
        // so this is only reachable from non-lifecycle callers.
        if advert.endpoints.is_empty() {
            return self.retract_current_advert().await;
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
            if advert
                .stun_servers
                .as_ref()
                .is_none_or(|servers| servers.is_empty())
                && (!self.config.peer_assist.helper_enabled()
                    || self.peer_assist.first_helper_endpoint().await.is_none())
            {
                return Err(BootstrapError::InvalidAdvert(
                    "udp:nat endpoint requires non-empty stunServers or an active peer-assist helper endpoint".to_string(),
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
        #[cfg(test)]
        if self.test_capture_outgoing {
            *self.current_advert_event_id.write().await = Some(event.id);
            return Ok(());
        }
        self.client
            .send_event_to(self.config.advert_relays.clone(), &event)
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        debug!(
            event = %short_id(&event.id.to_string()),
            relays = self.config.advert_relays.len(),
            endpoints = %endpoint_summary(&advert.endpoints),
            ttl_secs = self.config.advert_ttl_secs,
            "advert: published"
        );
        // Kind 37195 lives in NIP-01's parameterized replaceable range
        // (30000–39999). Relays supersede the previous event for the same
        // (pubkey, kind, d-tag) triple by created_at — emitting an explicit
        // NIP-09 delete here is redundant and races with the replacement
        // publish, which strict relays (e.g. Damus) honor by removing the
        // new advert too.
        *self.current_advert_event_id.write().await = Some(event.id);
        Ok(())
    }

    async fn retract_current_advert(&self) -> Result<(), BootstrapError> {
        let previous_event_id = self.current_advert_event_id.write().await.take();
        if let Some(event_id) = previous_event_id {
            if let Err(err) = self
                .publish_delete(&self.config.advert_relays, [event_id.to_owned()])
                .await
            {
                *self.current_advert_event_id.write().await = Some(event_id);
                return Err(err);
            }
        }
        Ok(())
    }

    async fn connect_peer(
        &self,
        peer_config: PeerConfig,
    ) -> Result<EstablishedTraversal, BootstrapError> {
        let peer_short = short_npub(&peer_config.npub);
        debug!(peer = %peer_short, "traversal: initiator starting");
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

        let private_assist_enabled = self.config.peer_assist.dial_enabled();
        if !advert.has_udp_nat_endpoint() {
            if private_assist_enabled {
                return self
                    .connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
                    .await;
            }
            return Err(BootstrapError::MissingNatEndpoint(peer_config.npub));
        }

        if private_assist_enabled && self.config.stun_servers.is_empty() {
            return self
                .connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
                .await;
        }

        let mut private_assist_tried = false;
        let mut private_assist_error = None;
        if private_assist_enabled && self.config.peer_assist.prefer_private() {
            private_assist_tried = true;
            match self
                .connect_peer_via_private_assist(peer_config.clone(), target_pubkey, &relays)
                .await
            {
                Ok(traversal) => return Ok(traversal),
                Err(err) => {
                    debug!(
                        peer = %peer_short,
                        error = %err,
                        "traversal: private assist failed; trying normal NAT traversal"
                    );
                    private_assist_error = Some(err);
                }
            }
        }

        if private_assist_enabled && self.config.stun_servers.is_empty() {
            if !private_assist_tried {
                return self
                    .connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
                    .await;
            }
            if let Some(err) = private_assist_error {
                return Err(err);
            }
        }

        let normal_result: Result<EstablishedTraversal, BootstrapError> = async {
            let base_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
            base_socket.set_nonblocking(true)?;

            let (reflexive_address, local_addresses, stun_server) = observe_traversal_addresses(
                &base_socket,
                &self.config.stun_servers,
                self.config.share_local_candidates,
            )
            .await?;
            debug!(
                peer = %peer_short,
                reflexive = %reflexive_address.as_ref().map(|a| format!("{}:{}", a.ip, a.port)).unwrap_or_else(|| "-".into()),
                local = local_addresses.len(),
                stun = %stun_server.as_deref().unwrap_or("-"),
                "traversal: initiator STUN observed"
            );
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
            debug!(
                peer = %peer_short,
                session = %short_id(&offer.session_id),
                relays = relays.len(),
                event = %short_id(&offer_event.id.to_string()),
                "traversal: offer sent"
            );

            let answer =
                match tokio::time::timeout(Duration::from_secs(self.config.signal_ttl_secs), rx)
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
                        return Err(BootstrapError::SignalTimeout(peer_config.npub.clone()));
                    }
                };

            debug!(
                peer = %peer_short,
                session = %short_id(&offer.session_id),
                accepted = answer.payload.accepted,
                reflexive = %answer.payload.reflexive_address.as_ref().map(|a| format!("{}:{}", a.ip, a.port)).unwrap_or_else(|| "-".into()),
                local = answer.payload.local_addresses.len(),
                "traversal: answer received"
            );
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
            debug!(
                peer = %peer_short,
                session = %short_id(&session_id),
                remote = %remote_addr,
                "traversal: initiator punch succeeded"
            );

            let _ = self
                .publish_delete(&relays, [offer_event.id, answer.event_id])
                .await;

            let mut traversal = EstablishedTraversal::new(
                session_id,
                peer_config.npub.clone(),
                remote_addr,
                base_socket,
            )
            .with_transport_name("nostr-nat");
            if let Some(observed_endpoint) = reflexive_address
                .as_ref()
                .and_then(Self::traversal_address_to_socket)
            {
                traversal = traversal.with_observed_endpoint(observed_endpoint);
            }
            Ok(traversal)
        }
        .await;

        match normal_result {
            Ok(traversal) => Ok(traversal),
            Err(err)
                if private_assist_enabled
                    && !private_assist_tried
                    && Self::should_try_private_assist_after_nat_error(&err) =>
            {
                debug!(
                    peer = %peer_short,
                    error = %err,
                    "traversal: normal NAT traversal failed; trying private assist"
                );
                self.connect_peer_via_private_assist(peer_config, target_pubkey, &relays)
                    .await
            }
            Err(err) => Err(err),
        }
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
        self.peer_assist
            .insert_grant_waiter(request.nonce.clone(), grant_tx)
            .await;
        let request_event = match self.send_signal(relays, target_pubkey, &request).await {
            Ok(event) => event,
            Err(err) => {
                self.peer_assist.remove_grant_waiter(&request.nonce).await;
                return Err(err);
            }
        };

        let grant = match tokio::time::timeout(
            Duration::from_secs(self.config.peer_assist.grant_ttl_secs),
            grant_rx,
        )
        .await
        {
            Ok(Ok(grant)) => grant,
            Ok(Err(_)) => {
                self.peer_assist.remove_grant_waiter(&request.nonce).await;
                return Err(BootstrapError::Protocol(
                    "assist grant channel closed".to_string(),
                ));
            }
            Err(_) => {
                self.peer_assist.remove_grant_waiter(&request.nonce).await;
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
        self.peer_assist
            .insert_observed_waiter(grant.payload.nonce.clone(), observed_tx)
            .await;

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
                self.peer_assist
                    .remove_observed_waiter(&grant.payload.nonce)
                    .await;
                probe_task.abort();
                return Err(BootstrapError::Protocol(
                    "assist observed channel closed".to_string(),
                ));
            }
            Err(_) => {
                self.peer_assist
                    .remove_observed_waiter(&grant.payload.nonce)
                    .await;
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
                .with_observed_endpoint(observed_endpoint)
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
        let peer_short = short_npub(&sender_npub);
        debug!(
            peer = %peer_short,
            session = %short_id(&offer.session_id),
            reflexive = %offer.reflexive_address.as_ref().map(|a| format!("{}:{}", a.ip, a.port)).unwrap_or_else(|| "-".into()),
            local = offer.local_addresses.len(),
            "traversal: offer received"
        );
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
        let (reflexive_address, local_addresses, stun_server) = observe_traversal_addresses(
            &base_socket,
            &self.config.stun_servers,
            self.config.share_local_candidates,
        )
        .await?;
        let accepted = reflexive_address.is_some() || !local_addresses.is_empty();
        debug!(
            peer = %peer_short,
            session = %short_id(&offer.session_id),
            accepted = accepted,
            reflexive = %reflexive_address.as_ref().map(|a| format!("{}:{}", a.ip, a.port)).unwrap_or_else(|| "-".into()),
            local = local_addresses.len(),
            "traversal: responder STUN observed"
        );
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
        debug!(
            peer = %peer_short,
            session = %short_id(&offer.session_id),
            accepted = accepted,
            relays = relays.len(),
            event = %short_id(&answer_event.id.to_string()),
            "traversal: answer sent"
        );
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
            debug!(
                peer = %peer_short,
                session = %short_id(&offer.session_id),
                remote = %remote_addr,
                "traversal: responder punch succeeded"
            );
            let mut traversal = EstablishedTraversal::new(
                offer.session_id,
                offer.sender_npub,
                remote_addr,
                base_socket,
            )
            .with_transport_name("nostr-nat");
            if let Some(observed_endpoint) = reflexive_address
                .as_ref()
                .and_then(Self::traversal_address_to_socket)
            {
                traversal = traversal.with_observed_endpoint(observed_endpoint);
            }
            let _ = self
                .event_tx
                .send(BootstrapEvent::Established { traversal });
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
        if !self.config.peer_assist.helper_enabled() {
            self.send_assist_rejection(&relays, sender, &request, "peer-assist disabled")
                .await?;
            return Ok(());
        }

        if !self.requester_allowed(&sender_npub).await {
            self.send_assist_rejection(
                &relays,
                sender,
                &request,
                "peer-assist requester not allowed",
            )
            .await?;
            return Ok(());
        }

        let Some(helper_addr) = self.peer_assist.first_helper_endpoint().await else {
            self.send_assist_rejection(&relays, sender, &request, "no eligible helper transport")
                .await?;
            return Ok(());
        };

        let now = now_ms();
        let grant_id = nonce();
        let grant_nonce = nonce();
        let probe_token = nonce();
        let pending_grant = PendingPrivateAssistGrant {
            request_id: request.request_id.clone(),
            grant_id: grant_id.clone(),
            grant_nonce: grant_nonce.clone(),
            probe_token: probe_token.clone(),
            sender_pubkey: sender,
            sender_npub: sender_npub.clone(),
            helper_addr,
            relays: relays.clone(),
            expires_at: now + self.config.peer_assist.grant_ttl_secs * 1000,
        };
        if !self
            .peer_assist
            .try_insert_private_grant(
                pending_grant,
                now,
                self.config.peer_assist.helper.max_pending_requests,
            )
            .await
        {
            self.send_assist_rejection(
                &relays,
                sender,
                &request,
                "peer-assist helper is at capacity",
            )
            .await?;
            return Ok(());
        }

        let grant = create_assist_grant(
            request.request_id.clone(),
            grant_id.clone(),
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            grant_nonce,
            self.npub.clone(),
            request.sender_npub.clone(),
            request.nonce.clone(),
            true,
            Some(helper_addr.to_string()),
            Some(probe_token),
            Some(1),
            None,
        );
        if let Err(err) = self.send_signal(&relays, sender, &grant).await {
            self.peer_assist.remove_private_grant(&grant_id).await;
            return Err(err);
        }
        Ok(())
    }

    async fn send_assist_rejection(
        &self,
        relays: &[String],
        sender: PublicKey,
        request: &AssistRequest,
        reason: &str,
    ) -> Result<(), BootstrapError> {
        let grant = create_assist_grant(
            request.request_id.clone(),
            nonce(),
            now_ms(),
            self.config.peer_assist.grant_ttl_secs * 1000,
            nonce(),
            self.npub.clone(),
            request.sender_npub.clone(),
            request.nonce.clone(),
            false,
            None,
            None,
            None,
            Some(reason.to_string()),
        );
        let _ = self.send_signal(relays, sender, &grant).await?;
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
        let Some(pending) = self
            .peer_assist
            .matching_private_grant(&probe.grant_id, &probe.token, helper_addr, now)
            .await
        else {
            return false;
        };

        let grant_id = pending.grant_id.clone();
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
            Ok(_) => {
                self.peer_assist.remove_private_grant(&grant_id).await;
                true
            }
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
        let authorized = match self.config.peer_assist.helper.request_policy {
            PeerAssistRequestPolicy::Allowlist => self
                .config
                .peer_assist
                .helper
                .request_allowlist
                .iter()
                .any(|allowed| allowed == sender_npub),
            PeerAssistRequestPolicy::OpenRateLimited => true,
        };
        if !authorized {
            return false;
        }

        let now = now_ms();
        let window_ms = self
            .config
            .peer_assist
            .helper
            .request_window_secs
            .saturating_mul(1000);
        self.peer_assist
            .request_allowed_in_window(
                sender_npub,
                now,
                window_ms,
                self.config
                    .peer_assist
                    .helper
                    .max_requests_per_peer_per_window,
            )
            .await
    }

    async fn traversal_offer_allowed(&self, sender_npub: &str) -> bool {
        let now = now_ms();
        let window_ms = self.config.offer_window_secs.saturating_mul(1000);
        let mut windows = self.traversal_offer_windows.lock().await;
        allow_in_rate_window(
            &mut windows,
            sender_npub,
            now,
            window_ms,
            self.config.max_offers_per_peer_per_window,
        )
    }

    fn should_try_private_assist_after_nat_error(err: &BootstrapError) -> bool {
        matches!(
            err,
            BootstrapError::Protocol(_)
                | BootstrapError::PunchTimeout(_)
                | BootstrapError::SignalTimeout(_)
                | BootstrapError::Stun(_)
        )
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
            debug!(
                peer = %short_npub(peer_npub),
                source = "cache",
                endpoints = %endpoint_summary(&cached.advert.endpoints),
                "advert: resolved"
            );
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
            let author_npub = event.pubkey.to_bech32().expect("infallible");
            if author_npub != peer_npub {
                continue;
            }
            let replace = best
                .as_ref()
                .map(|current| event.created_at.as_secs() >= current.created_at)
                .unwrap_or(true);
            if replace {
                best = Some(CachedOverlayAdvert {
                    author_npub,
                    advert,
                    created_at: event.created_at.as_secs(),
                    valid_until_ms,
                });
            }
        }

        let cached = best.ok_or_else(|| BootstrapError::MissingAdvert(peer_npub.to_string()))?;
        debug!(
            peer = %short_npub(peer_npub),
            source = "relay-fetch",
            endpoints = %endpoint_summary(&cached.advert.endpoints),
            "advert: resolved"
        );
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
        if let Some(advert) = advert
            && let Some(relays) = advert.signal_relays.as_ref()
        {
            for relay in relays {
                if !merged.contains(relay) {
                    merged.push(relay.clone());
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
                        Timestamp::now().as_secs().saturating_sub(30 * 24 * 60 * 60),
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
        let newest = events.iter().max_by_key(|event| event.created_at.as_secs());
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
        if cache.len() <= self.config.advert_cache_max_entries {
            return;
        }

        let mut oldest = cache
            .iter()
            .map(|(npub, entry)| (npub.clone(), entry.valid_until_ms))
            .collect::<Vec<_>>();
        oldest.sort_by_key(|(_, ts)| *ts);
        let overflow = cache
            .len()
            .saturating_sub(self.config.advert_cache_max_entries);
        for (npub, _) in oldest.into_iter().take(overflow) {
            cache.remove(&npub);
        }
        debug!(
            evicted = overflow,
            retained = cache.len(),
            cap = self.config.advert_cache_max_entries,
            "advert cache overflow; evicted oldest entries"
        );
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

        let created_ms = event.created_at.as_secs().saturating_mul(1000);
        let created_window_until = created_ms.saturating_add(advert_max_age_ms);
        if created_window_until <= now_ms {
            return None;
        }

        let expires_ms = event
            .tags
            .expiration()
            .map(|timestamp| timestamp.as_secs().saturating_mul(1000));
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
            .send_event_to(relays.to_vec(), &signal)
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        Ok(signal)
    }

    async fn publish_delete<I>(&self, relays: &[String], ids: I) -> Result<(), BootstrapError>
    where
        I: IntoIterator<Item = EventId>,
    {
        let ids = ids.into_iter().collect::<Vec<_>>();
        #[cfg(test)]
        if self.test_capture_outgoing {
            self.test_sent_deletes
                .lock()
                .await
                .push(ids.iter().map(ToString::to_string).collect());
            return Ok(());
        }

        let event = EventBuilder::delete(nostr::nips::nip09::EventDeletionRequest::new().ids(ids))
            .sign_with_keys(&self.keys)
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        self.client
            .send_event_to(relays.to_vec(), &event)
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
        if seen.len() > self.config.seen_sessions_max_entries {
            let mut oldest = seen
                .iter()
                .map(|(session, expires_at)| (session.clone(), *expires_at))
                .collect::<Vec<_>>();
            oldest.sort_by_key(|(_, expires_at)| *expires_at);
            let overflow = seen
                .len()
                .saturating_sub(self.config.seen_sessions_max_entries);
            for (session, _) in oldest.into_iter().take(overflow) {
                seen.remove(&session);
            }
            debug!(
                evicted = overflow,
                retained = seen.len(),
                cap = self.config.seen_sessions_max_entries,
                "seen-sessions cache overflow; evicted oldest entries"
            );
        }
        Ok(())
    }
}
