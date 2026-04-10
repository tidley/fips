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
    SignalEnvelope, build_signal_event, create_traversal_answer, create_traversal_offer,
    unwrap_signal_event, validate_offer_freshness, validate_traversal_answer_for_offer,
};
use super::stun::observe_traversal_addresses;
use super::traversal::{nonce, now_ms, planned_remote_endpoints, run_punch_attempt};
use super::types::{
    ADVERT_KIND, BootstrapError, BootstrapEvent, PROTOCOL_VERSION, PunchHint, SIGNAL_KIND,
    TraversalAdvert, TraversalAnswer, TraversalOffer,
};
use crate::bootstrap::EstablishedTraversal;
use crate::config::{NostrBootstrapConfig, PeerConfig};

const MAX_CONCURRENT_INCOMING_OFFERS: usize = 16;

pub struct NostrBootstrap {
    client: Client,
    keys: nostr::Keys,
    pubkey: PublicKey,
    npub: String,
    config: NostrBootstrapConfig,
    advert_cache: RwLock<HashMap<String, TraversalAdvert>>,
    current_advert_event_id: RwLock<Option<EventId>>,
    pending_answers: Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<TraversalAnswer>>>>,
    active_initiators: Mutex<HashSet<String>>,
    seen_sessions: Mutex<HashMap<String, u64>>,
    offer_slots: Arc<Semaphore>,
    event_tx: mpsc::UnboundedSender<BootstrapEvent>,
    event_rx: Mutex<mpsc::UnboundedReceiver<BootstrapEvent>>,
    notify_task: Mutex<Option<JoinHandle<()>>>,
    advertise_task: Mutex<Option<JoinHandle<()>>>,
}

impl NostrBootstrap {
    pub async fn start(
        identity: &crate::Identity,
        config: NostrBootstrapConfig,
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
            current_advert_event_id: RwLock::new(None),
            pending_answers: Mutex::new(HashMap::new()),
            active_initiators: Mutex::new(HashSet::new()),
            seen_sessions: Mutex::new(HashMap::new()),
            offer_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_INCOMING_OFFERS)),
            event_tx,
            event_rx: Mutex::new(event_rx),
            notify_task: Mutex::new(None),
            advertise_task: Mutex::new(None),
        });

        runtime.subscribe().await?;
        runtime.publish_inbox_relays().await?;
        if runtime.config.advertise {
            runtime.publish_advert().await?;
            *runtime.advertise_task.lock().await = Some(runtime.clone().spawn_advertise_loop());
        }
        *runtime.notify_task.lock().await = Some(runtime.clone().spawn_notify_loop());

        Ok(runtime)
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
                        if let Ok(advert) = serde_json::from_str::<TraversalAdvert>(&event.content)
                            && advert.expires_at > now_ms()
                        {
                            self.advert_cache
                                .write()
                                .await
                                .insert(advert.publisher_npub.clone(), advert);
                        }
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
                Filter::new().kind(Kind::Custom(ADVERT_KIND)),
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
        let now = now_ms();
        let advert = TraversalAdvert {
            app: self.config.app.clone(),
            event_kind: ADVERT_KIND,
            protocol: self.config.app.clone(),
            publisher_npub: self.npub.clone(),
            published_at: now,
            expires_at: now + self.config.advert_ttl_secs * 1000,
            sequence: now,
            relays: self.config.dm_relays.clone(),
            stun_servers: self.config.stun_servers.clone(),
            transports: vec!["udp".to_string()],
            endpoint_hint: None,
        };

        let mut tags = vec![
            Tag::identifier(format!("udp-service-v1/{}", self.config.app)),
            Tag::custom(TagKind::custom("protocol"), [self.config.app.clone()]),
            Tag::custom(TagKind::custom("version"), [PROTOCOL_VERSION.to_string()]),
            Tag::expiration(Timestamp::from((advert.expires_at / 1000).max(1))),
        ];
        tags.push(Tag::custom(
            TagKind::custom("relays"),
            self.config.dm_relays.clone(),
        ));
        tags.push(Tag::custom(
            TagKind::custom("stun"),
            self.config.stun_servers.clone(),
        ));

        let event = EventBuilder::new(Kind::Custom(ADVERT_KIND), serde_json::to_string(&advert)?)
            .tags(tags)
            .sign_with_keys(&self.keys)
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
        self.client
            .send_event_to(self.config.advert_relays.clone(), event.clone())
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
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

        let base_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
        base_socket.set_nonblocking(true)?;

        let (reflexive_address, local_addresses, stun_server) =
            observe_traversal_addresses(&base_socket, &self.config.stun_servers).await?;
        let session_id = nonce();
        let offer = create_traversal_offer(
            self.config.app.clone(),
            session_id.clone(),
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            session_id.clone(),
            self.npub.clone(),
            peer_config.npub.clone(),
            reflexive_address,
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
                .with_transport_name("nostr-nat"),
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
            &self.config.app,
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
            self.config.app.clone(),
            offer.session_id.clone(),
            now_ms(),
            self.config.signal_ttl_secs * 1000,
            nonce(),
            self.npub.clone(),
            offer.sender_npub.clone(),
            offer.nonce.clone(),
            accepted,
            reflexive_address,
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
                .with_transport_name("nostr-nat"),
            });
        }

        let _ = self.publish_delete(&relays, [answer_event.id]).await;
        Ok(())
    }

    async fn fetch_advert(
        &self,
        peer_npub: &str,
        target_pubkey: PublicKey,
    ) -> Result<TraversalAdvert, BootstrapError> {
        if let Some(cached) = self.advert_cache.read().await.get(peer_npub).cloned()
            && cached.expires_at > now_ms()
        {
            return Ok(cached);
        }

        let events = self
            .client
            .fetch_events_from(
                self.config.advert_relays.clone(),
                Filter::new()
                    .author(target_pubkey)
                    .kind(Kind::Custom(ADVERT_KIND))
                    .identifier(format!("udp-service-v1/{}", self.config.app)),
                Duration::from_secs(2),
            )
            .await
            .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

        let mut best: Option<TraversalAdvert> = None;
        for event in events.iter() {
            let Ok(advert) = serde_json::from_str::<TraversalAdvert>(&event.content) else {
                continue;
            };
            if advert.expires_at <= now_ms() {
                continue;
            }
            if advert.publisher_npub != peer_npub {
                continue;
            }
            let replace = best
                .as_ref()
                .map(|current| advert.published_at >= current.published_at)
                .unwrap_or(true);
            if replace {
                best = Some(advert);
            }
        }

        let advert = best.ok_or_else(|| BootstrapError::MissingAdvert(peer_npub.to_string()))?;
        self.advert_cache
            .write()
            .await
            .insert(peer_npub.to_string(), advert.clone());
        Ok(advert)
    }

    async fn preferred_signal_relays(
        &self,
        target_pubkey: PublicKey,
        advert: Option<&TraversalAdvert>,
    ) -> Result<Vec<String>, BootstrapError> {
        let mut merged = self.find_recipient_inbox_relays(target_pubkey).await?;
        if let Some(advert) = advert {
            for relay in &advert.relays {
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
        Ok(())
    }
}
