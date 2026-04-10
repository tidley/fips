use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use nostr::nips::nip17;
use nostr::{EventBuilder, Filter, Kind, PublicKey, RelayUrl, Tag, TagKind, Timestamp};
use tokio::time::{sleep, Instant};

use super::*;
use crate::common::{log_publish_outcome, now_ms};
use crate::{
    build_punch_packet, plan_punch_targets, EndpointHint, LegacyPunch, PunchPacketKind,
    TraversalAddress, TraversalAdvert, TraversalOffer, ADVERT_KIND,
};

impl ServerRuntimeCore {
    fn endpoint_from_traversal_address(address: &TraversalAddress) -> LegacyEndpoint {
        LegacyEndpoint {
            host: address.ip.clone(),
            port: address.port,
        }
    }

    fn select_remote_endpoint_from_offer(offer: &TraversalOffer) -> Option<LegacyEndpoint> {
        offer
            .reflexive_address
            .as_ref()
            .map(Self::endpoint_from_traversal_address)
            .or_else(|| {
                offer
                    .local_addresses
                    .first()
                    .map(Self::endpoint_from_traversal_address)
            })
    }

    pub(crate) fn planned_remote_endpoints_from_offer_answer(
        offer: &TraversalOffer,
        answer_reflexive_address: Option<&TraversalAddress>,
        answer_local_addresses: &[TraversalAddress],
    ) -> Vec<LegacyEndpoint> {
        let targets = plan_punch_targets(
            answer_local_addresses,
            answer_reflexive_address,
            &offer.local_addresses,
            offer.reflexive_address.as_ref(),
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
            if let Some(endpoint) = Self::select_remote_endpoint_from_offer(offer) {
                remotes.push(endpoint);
            }
        }
        remotes
    }

    pub async fn publish_advert(&self) -> Result<()> {
        let endpoint = self.resolve_traversal_endpoint().await?;
        let now = now_ms();
        let advert = TraversalAdvert {
            app: "fips.nat.traversal.v1".to_owned(),
            event_kind: ADVERT_KIND,
            protocol: "fips.nat.traversal.v1".to_owned(),
            publisher_npub: self.npub.clone(),
            published_at: now,
            expires_at: now + self.advertise_ttl_ms,
            sequence: now,
            relays: self.dm_relays.clone(),
            stun_servers: self.stun_servers.clone(),
            transports: vec!["udp".to_owned()],
            endpoint_hint: Some(EndpointHint {
                host: endpoint.host,
                port: endpoint.port,
            }),
        };

        let event = EventBuilder::new(Kind::Custom(ADVERT_KIND), serde_json::to_string(&advert)?)
            .tags([
                Tag::identifier(format!("fips-traversal:{}", self.npub)),
                Tag::hashtag("fips"),
                Tag::hashtag("traversal"),
                Tag::expiration(Timestamp::from((now + self.advertise_ttl_ms) / 1000)),
            ])
            .sign_with_keys(&self.keys)?;

        let output = self
            .client
            .send_event_to(self.advert_relays.clone(), event)
            .await?;
        log_publish_outcome("advert", &self.npub, &output.success, &output.failed);
        Ok(())
    }

    pub async fn publish_inbox_relays(&self) -> Result<()> {
        let relay_tags = self
            .dm_relays
            .iter()
            .filter_map(|relay| RelayUrl::parse(relay).ok())
            .map(|relay| {
                Tag::custom(
                    TagKind::SingleLetter(nostr::SingleLetterTag::lowercase(nostr::Alphabet::R)),
                    [relay.to_string()],
                )
            })
            .collect::<Vec<_>>();
        let event = EventBuilder::new(Kind::InboxRelays, "")
            .tags(relay_tags)
            .sign_with_keys(&self.keys)?;
        let output = self
            .client
            .send_event_to(self.dm_relays.clone(), event)
            .await?;
        log_publish_outcome("inbox-relays", &self.npub, &output.success, &output.failed);
        Ok(())
    }

    async fn find_recipient_inbox_relays(&self, target_pubkey: PublicKey) -> Result<Vec<String>> {
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

    pub async fn preferred_dm_relays(&self, target_pubkey: PublicKey) -> Result<Vec<String>> {
        let mut merged = Vec::new();
        for relay in self.find_recipient_inbox_relays(target_pubkey).await? {
            if !merged.contains(&relay) {
                merged.push(relay);
            }
        }
        for relay in &self.dm_relays {
            if !merged.contains(relay) {
                merged.push(relay.clone());
            }
        }
        Ok(merged)
    }

    pub(crate) async fn send_dm_to(
        &self,
        relays: Vec<String>,
        receiver: PublicKey,
        obj: &impl serde::Serialize,
        kind: &str,
    ) -> Result<()> {
        let content = serde_json::to_string(obj)?;
        let event = EventBuilder::private_msg(&self.keys, receiver, content, []).await?;
        let output = self.client.send_event_to(relays, event).await?;
        log_publish_outcome(kind, &receiver.to_hex(), &output.success, &output.failed);
        Ok(())
    }

    pub(crate) async fn start_punch(
        &self,
        session_id: String,
        remote: LegacyEndpoint,
        punch: LegacyPunch,
    ) -> Result<()> {
        let socket = self.udp_socket.clone();
        let target = SocketAddr::new(remote.host.parse()?, remote.port);
        self.session_hashes
            .lock()
            .await
            .insert(crate::session_hash(&session_id), session_id.clone());
        let interval_ms = punch.interval_ms;
        let duration_ms = punch.duration_ms;
        let delay_ms = punch.start_at_ms.saturating_sub(now_ms());
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(duration_ms) {
                let packet = build_punch_packet(PunchPacketKind::Probe, &session_id);
                let _ = socket.send_to(&packet, target).await;
                sleep(Duration::from_millis(interval_ms)).await;
            }
        });
        Ok(())
    }

    pub(crate) async fn start_punch_plan(
        &self,
        session_id: String,
        remotes: Vec<LegacyEndpoint>,
        punch: LegacyPunch,
    ) -> Result<()> {
        if remotes.is_empty() {
            return Ok(());
        }
        let socket = self.udp_socket.clone();
        let targets = remotes
            .iter()
            .map(|remote| Ok(SocketAddr::new(remote.host.parse()?, remote.port)))
            .collect::<Result<Vec<_>>>()?;
        self.session_hashes
            .lock()
            .await
            .insert(crate::session_hash(&session_id), session_id.clone());
        let interval_ms = punch.interval_ms;
        let duration_ms = punch.duration_ms;
        let delay_ms = punch.start_at_ms.saturating_sub(now_ms());
        tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(duration_ms) {
                let packet = build_punch_packet(PunchPacketKind::Probe, &session_id);
                for target in &targets {
                    let _ = socket.send_to(&packet, target).await;
                }
                sleep(Duration::from_millis(interval_ms)).await;
            }
        });
        Ok(())
    }
}
