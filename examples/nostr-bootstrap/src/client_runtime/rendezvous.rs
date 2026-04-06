use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nostr::nips::nip17;
use nostr::{
    EventBuilder, Filter, Kind, PublicKey, RelayUrl, SingleLetterTag, Tag, TagKind, Timestamp,
};
use tokio::time::{sleep, timeout, Instant};

use super::*;
use crate::common::{nonce, now_ms};
use crate::{
    build_punch_packet, create_traversal_offer, plan_punch_targets,
    validate_traversal_answer_for_offer, LegacyHelloMessage, LegacyPunch, LegacyWants,
    PunchPacketKind, TraversalAddress, TraversalOffer,
};

impl ClientRuntimeCore {
    pub async fn publish_inbox_relays(&self) -> Result<()> {
        let tags = self
            .dm_relays
            .iter()
            .filter_map(|relay| RelayUrl::parse(relay).ok())
            .map(|relay| {
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(nostr::Alphabet::R)),
                    [relay.to_string()],
                )
            })
            .collect::<Vec<_>>();
        let event = EventBuilder::new(Kind::InboxRelays, "")
            .tags(tags)
            .sign_with_keys(&self.keys)?;
        let _ = self
            .client
            .send_event_to(self.dm_relays.clone(), event)
            .await?;
        Ok(())
    }

    pub async fn refresh_advert_cache(&self, wait_ms: u64) -> Result<()> {
        let filter = Filter::new()
            .kind(Kind::Custom(crate::ADVERT_KIND))
            .since(Timestamp::from(
                Timestamp::now().as_u64().saturating_sub(3 * 24 * 60 * 60),
            ));
        let events = self
            .client
            .fetch_events_from(
                self.advert_relays.clone(),
                filter,
                Duration::from_millis(wait_ms),
            )
            .await?;
        let mut cache = self.advert_cache.write().await;
        for event in events.iter() {
            if let Ok(advert) = serde_json::from_str::<TraversalAdvert>(&event.content) {
                if advert.expires_at <= now_ms() {
                    continue;
                }
                let replace = cache
                    .get(&advert.publisher_npub)
                    .map(|existing| {
                        advert.published_at > existing.published_at
                            || (advert.published_at == existing.published_at
                                && advert.sequence >= existing.sequence)
                    })
                    .unwrap_or(true);
                if replace {
                    cache.insert(advert.publisher_npub.clone(), advert);
                }
            }
        }
        Ok(())
    }

    pub async fn list_advertised_peers(&self, max_peers: usize) -> Result<Vec<TraversalAdvert>> {
        if self.discovery_enabled {
            let _ = self.refresh_advert_cache(2_000).await;
        }
        let cache = self.advert_cache.read().await;
        let mut adverts = cache
            .values()
            .filter(|advert| advert.publisher_npub != self.npub && advert.expires_at > now_ms())
            .cloned()
            .collect::<Vec<_>>();
        adverts.sort_by(|a, b| {
            b.published_at
                .cmp(&a.published_at)
                .then_with(|| b.sequence.cmp(&a.sequence))
        });
        adverts.truncate(max_peers);
        Ok(adverts)
    }

    pub async fn find_advertised_peer(&self, target_npub: &str) -> Result<Option<TraversalAdvert>> {
        self.refresh_advert_cache(2_000).await.ok();
        Ok(self
            .advert_cache
            .read()
            .await
            .get(target_npub)
            .filter(|advert| advert.expires_at > now_ms())
            .cloned())
    }

    pub async fn find_recipient_inbox_relays(
        &self,
        target_pubkey: PublicKey,
    ) -> Result<Vec<String>> {
        let filter = Filter::new()
            .author(target_pubkey)
            .kind(Kind::InboxRelays)
            .since(Timestamp::from(
                Timestamp::now().as_u64().saturating_sub(30 * 24 * 60 * 60),
            ));
        let events = match self
            .client
            .fetch_events_from(
                self.inbox_lookup_relays.clone(),
                filter,
                Duration::from_millis(1_500),
            )
            .await
        {
            Ok(events) => events,
            Err(_) => return Ok(self.dm_relays.clone()),
        };
        let mut newest: Option<nostr::Event> = None;
        for event in events.iter() {
            if newest
                .as_ref()
                .map(|current| event.created_at >= current.created_at)
                .unwrap_or(true)
            {
                newest = Some(event.clone());
            }
        }
        if let Some(event) = newest {
            let relays = nip17::extract_relay_list(&event)
                .map(|relay| relay.to_string())
                .collect::<Vec<_>>();
            if !relays.is_empty() {
                return Ok(relays);
            }
        }
        Ok(self.dm_relays.clone())
    }

    pub async fn preferred_dm_relays(
        &self,
        target_pubkey: PublicKey,
        advert: Option<&TraversalAdvert>,
    ) -> Result<Vec<String>> {
        let mut merged = Vec::new();
        for relay in self.find_recipient_inbox_relays(target_pubkey).await? {
            if !merged.contains(&relay) {
                merged.push(relay);
            }
        }
        if let Some(advert) = advert {
            for relay in &advert.relays {
                if !merged.contains(relay) {
                    merged.push(relay.clone());
                }
            }
        }
        for relay in &self.dm_relays {
            if !merged.contains(relay) {
                merged.push(relay.clone());
            }
        }
        Ok(merged)
    }

    pub async fn send_hello(
        &self,
        relays: Vec<String>,
        target_pubkey: PublicKey,
        target_npub: String,
    ) -> Result<(LegacyServerInfoMessage, Option<TraversalAdvert>)> {
        let discovered_advert = self.find_advertised_peer(&target_npub).await?;
        let endpoint = self.local_client_endpoint().await?;
        let session_id = nonce();
        println!(
            "[rendezvous] hello prepared {}",
            serde_json::to_string(&json!({
                "targetNpub": target_npub,
                "sessionId": session_id,
                "clientEndpoint": endpoint,
                "relays": relays,
                "discoveredAdvertRelays": discovered_advert.as_ref().map(|advert| advert.relays.clone()),
            }))
            .unwrap_or_else(|_| "{\"kind\":\"log-error\"}".to_owned())
        );
        let hello = LegacyHelloMessage {
            message_type: "fips.rendezvous.hello".to_owned(),
            version: "1.0".to_owned(),
            session_id: session_id.clone(),
            nonce: session_id.clone(),
            issued_at: now_ms(),
            wants: LegacyWants {
                stun_info: true,
                fips_connect: true,
            },
            client_endpoint: Some(endpoint),
        };

        let (tx, mut rx) = oneshot::channel();
        self.pending_server_info
            .lock()
            .await
            .insert(session_id.clone(), tx);

        let start = Instant::now();
        let wait_ms = 60_000u64;
        let retry_ms = 5_000u64;
        loop {
            let event = EventBuilder::private_msg(
                &self.keys,
                target_pubkey,
                serde_json::to_string(&hello)?,
                [Tag::public_key(target_pubkey)],
            )
            .await?;
            let _ = self.client.send_event_to(relays.clone(), event).await?;
            match timeout(Duration::from_millis(retry_ms), &mut rx).await {
                Ok(Ok(reply)) => return Ok((reply, discovered_advert)),
                Ok(Err(_)) => return Err(anyhow!("pending reply channel closed")),
                Err(_) if start.elapsed() >= Duration::from_millis(wait_ms) => {
                    self.pending_server_info.lock().await.remove(&session_id);
                    return Err(anyhow!("timed out waiting for traversal answer"));
                }
                Err(_) => continue,
            }
        }
    }

    pub async fn send_offer(
        &self,
        relays: Vec<String>,
        target_pubkey: PublicKey,
        target_npub: String,
    ) -> Result<(TraversalOffer, TraversalAnswer, Option<TraversalAdvert>)> {
        let discovered_advert = self.find_advertised_peer(&target_npub).await?;
        let (reflexive_address, local_addresses) = self.local_traversal_addresses().await?;
        let session_id = nonce();
        let offer = create_traversal_offer(
            session_id.clone(),
            now_ms(),
            60_000,
            session_id.clone(),
            self.npub.clone(),
            target_npub.clone(),
            reflexive_address,
            local_addresses,
        );
        println!(
            "[rendezvous] offer prepared {}",
            serde_json::to_string(&json!({
                "targetNpub": target_npub,
                "sessionId": offer.session_id,
                "nonce": offer.nonce,
                "reflexiveAddress": offer.reflexive_address,
                "localAddresses": offer.local_addresses,
                "relays": relays,
                "discoveredAdvertRelays": discovered_advert.as_ref().map(|advert| advert.relays.clone()),
            }))
            .unwrap_or_else(|_| "{\"kind\":\"log-error\"}".to_owned())
        );

        let (tx, mut rx) = oneshot::channel();
        self.pending_answer
            .lock()
            .await
            .insert(offer.nonce.clone(), tx);

        let start = Instant::now();
        let wait_ms = 15_000u64;
        let retry_ms = 3_000u64;
        loop {
            let event = EventBuilder::private_msg(
                &self.keys,
                target_pubkey,
                serde_json::to_string(&offer)?,
                [Tag::public_key(target_pubkey)],
            )
            .await?;
            let _ = self.client.send_event_to(relays.clone(), event).await?;
            match timeout(Duration::from_millis(retry_ms), &mut rx).await {
                Ok(Ok(answer)) => {
                    validate_traversal_answer_for_offer(&offer, &answer, now_ms())
                        .map_err(|reason| anyhow!("invalid traversal answer: {reason}"))?;
                    return Ok((offer, answer, discovered_advert));
                }
                Ok(Err(_)) => return Err(anyhow!("pending answer channel closed")),
                Err(_) if start.elapsed() >= Duration::from_millis(wait_ms) => {
                    self.pending_answer.lock().await.remove(&offer.nonce);
                    return Err(anyhow!("timed out waiting for traversal answer"));
                }
                Err(_) => continue,
            }
        }
    }

    fn endpoint_from_traversal_address(address: &TraversalAddress) -> LegacyEndpoint {
        LegacyEndpoint {
            host: address.ip.clone(),
            port: address.port,
        }
    }

    fn select_remote_endpoint_from_answer(answer: &TraversalAnswer) -> Option<LegacyEndpoint> {
        answer
            .reflexive_address
            .as_ref()
            .map(Self::endpoint_from_traversal_address)
            .or_else(|| {
                answer
                    .local_addresses
                    .first()
                    .map(Self::endpoint_from_traversal_address)
            })
    }

    pub fn planned_remote_endpoints_from_offer_answer(
        offer: &TraversalOffer,
        answer: &TraversalAnswer,
    ) -> Vec<LegacyEndpoint> {
        let targets = plan_punch_targets(
            &offer.local_addresses,
            offer.reflexive_address.as_ref(),
            &answer.local_addresses,
            answer.reflexive_address.as_ref(),
        );
        let mut remotes = Vec::new();
        for target in targets {
            let endpoint = Self::endpoint_from_traversal_address(&target.remote);
            if !remotes.iter().any(|existing: &LegacyEndpoint| {
                existing.host == endpoint.host && existing.port == endpoint.port
            }) {
                remotes.push(endpoint);
            }
        }
        if remotes.is_empty() {
            if let Some(endpoint) = Self::select_remote_endpoint_from_answer(answer) {
                remotes.push(endpoint);
            }
        }
        remotes
    }

    pub async fn start_punch_and_wait(
        &self,
        session_id: String,
        remote: LegacyEndpoint,
        punch: LegacyPunch,
    ) -> Result<LegacyEndpoint> {
        let remote_addr = SocketAddr::new(remote.host.parse()?, remote.port);
        let (tx, rx) = oneshot::channel();
        self.pending_punch
            .lock()
            .await
            .insert(session_id.clone(), tx);
        self.punch_hashes
            .lock()
            .await
            .insert(crate::session_hash(&session_id), session_id.clone());

        let socket = self.udp_socket.clone();
        let delay_ms = punch.start_at_ms.saturating_sub(now_ms());
        let interval_ms = punch.interval_ms;
        let duration_ms = punch.duration_ms;
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(duration_ms) {
                let packet = build_punch_packet(PunchPacketKind::Probe, &session_id);
                let _ = socket.send_to(&packet, remote_addr).await;
                sleep(Duration::from_millis(interval_ms)).await;
            }
        });

        timeout(Duration::from_millis(duration_ms + 5_000), rx)
            .await
            .context("timed out waiting for UDP hole punch")?
            .map_err(|_| anyhow!("punch channel dropped"))
    }

    pub async fn start_punch_plan_and_wait(
        &self,
        session_id: String,
        remotes: Vec<LegacyEndpoint>,
        punch: LegacyPunch,
    ) -> Result<LegacyEndpoint> {
        if remotes.is_empty() {
            return Err(anyhow!("no punch targets planned"));
        }
        let remote_addrs = remotes
            .iter()
            .map(|remote| Ok(SocketAddr::new(remote.host.parse()?, remote.port)))
            .collect::<Result<Vec<_>>>()?;
        let (tx, rx) = oneshot::channel();
        self.pending_punch
            .lock()
            .await
            .insert(session_id.clone(), tx);
        self.punch_hashes
            .lock()
            .await
            .insert(crate::session_hash(&session_id), session_id.clone());

        let socket = self.udp_socket.clone();
        let delay_ms = punch.start_at_ms.saturating_sub(now_ms());
        let interval_ms = punch.interval_ms;
        let duration_ms = punch.duration_ms;
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(duration_ms) {
                let packet = build_punch_packet(PunchPacketKind::Probe, &session_id);
                for remote in &remote_addrs {
                    let _ = socket.send_to(&packet, remote).await;
                }
                sleep(Duration::from_millis(interval_ms)).await;
            }
        });

        timeout(Duration::from_millis(duration_ms + 5_000), rx)
            .await
            .context("timed out waiting for UDP hole punch")?
            .map_err(|_| anyhow!("punch channel dropped"))
    }

    pub async fn connect_via_rendezvous(
        &self,
        requested_npub: Option<String>,
        log_prefix: &str,
    ) -> Result<ConnectOutcome> {
        let npub = requested_npub.unwrap_or_default().trim().to_owned();
        println!(
            "[{log_prefix}] connect request {}",
            serde_json::to_string(&json!({
                "target": if npub.is_empty() { "(first-discovered-peer)" } else { npub.as_str() },
                "mode": if npub.is_empty() { "advert-discovery" } else { "explicit-npub-direct" },
            }))
            .unwrap_or_default()
        );

        let (target_npub, discovered_advert) = if npub.is_empty() {
            let peers = self.list_advertised_peers(1).await?;
            let advert = peers
                .into_iter()
                .next()
                .context("timed out waiting for any traversal advert after 60000ms")?;
            (advert.publisher_npub.clone(), Some(advert))
        } else {
            let advert = if self.discovery_enabled {
                self.find_advertised_peer(&npub).await?
            } else {
                None
            };
            (npub.clone(), advert)
        };

        if target_npub == self.npub {
            return Err(anyhow!("refusing to connect to self"));
        }
        let decoded = nostr::nips::nip19::FromBech32::from_bech32(&target_npub)?;
        let target_pubkey = match decoded {
            nostr::nips::nip19::Nip19::Pubkey(pubkey) => pubkey,
            _ => return Err(anyhow!("target must be npub")),
        };
        let dm_relays = self
            .preferred_dm_relays(target_pubkey, discovered_advert.as_ref())
            .await?;

        let (session_id, established_remote) = match self
            .send_offer(dm_relays.clone(), target_pubkey, target_npub.clone())
            .await
        {
            Ok((offer, answer, _)) => {
                if !answer.accepted {
                    return Err(anyhow!(
                        "{}",
                        answer
                            .reason
                            .clone()
                            .unwrap_or_else(|| "traversal answer rejected".to_owned())
                    ));
                }
                let remotes = Self::planned_remote_endpoints_from_offer_answer(&offer, &answer);
                let punch = answer
                    .punch
                    .clone()
                    .map(|punch| LegacyPunch {
                        start_at_ms: punch.start_at_ms,
                        interval_ms: punch.interval_ms,
                        duration_ms: punch.duration_ms,
                    })
                    .unwrap_or(LegacyPunch {
                        start_at_ms: now_ms() + self.punch_start_delay_ms,
                        interval_ms: self.punch_interval_ms,
                        duration_ms: self.punch_duration_ms,
                    });
                let established_remote = self
                    .start_punch_plan_and_wait(offer.session_id.clone(), remotes, punch)
                    .await?;
                (offer.session_id, established_remote)
            }
            Err(offer_err) => {
                println!(
                    "[{log_prefix}] offer fallback {}",
                    serde_json::to_string(&json!({
                        "target": target_npub,
                        "error": offer_err.to_string(),
                        "fallback": "legacy-hello",
                    }))
                    .unwrap_or_default()
                );
                let (reply, _) = self
                    .send_hello(dm_relays, target_pubkey, target_npub.clone())
                    .await?;
                let remote = reply.endpoint.clone();
                let punch = reply.punch.clone().unwrap_or(LegacyPunch {
                    start_at_ms: now_ms() + self.punch_start_delay_ms,
                    interval_ms: self.punch_interval_ms,
                    duration_ms: self.punch_duration_ms,
                });
                let established_remote = self
                    .start_punch_and_wait(reply.nonce.clone(), remote, punch)
                    .await?;
                (reply.nonce, established_remote)
            }
        };

        Ok(ConnectOutcome {
            target_npub,
            discovered_advert,
            session_id,
            established_remote,
        })
    }
}
