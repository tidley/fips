use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nostr::nips::nip19::ToBech32;
use nostr::nips::nip59;
use nostr::{Filter, Kind};
use nostr_sdk::prelude::RelayPoolNotification;
use serde_json::json;
use tokio::task::JoinHandle;

use super::*;
use crate::common::{now_ms, parse_stun_binding_success};
use crate::{
    build_punch_packet, create_traversal_answer, parse_punch_packet, LegacyHelloMessage,
    LegacyPunch, LegacyServerInfoMessage, LegacyStunInfo, PunchHint, PunchPacketKind,
    TraversalOffer,
};

impl ServerRuntimeCore {
    pub fn spawn_udp_loop<F, Fut>(self: Arc<Self>, on_handoff: F) -> JoinHandle<Result<()>>
    where
        F: Fn(String, String, SocketAddr) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let on_handoff = Arc::new(on_handoff);
        tokio::spawn(async move {
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                let (len, remote) = self.udp_socket.recv_from(&mut buf).await?;
                let packet = &buf[..len];

                if packet.len() >= 20 {
                    let maybe_txn = &packet[8..20];
                    if let Ok(txn_id) = <[u8; 12]>::try_from(maybe_txn) {
                        if let Some(mapped) = parse_stun_binding_success(packet, &txn_id) {
                            if let Some(tx) = self.pending_stun.lock().await.remove(&txn_id) {
                                let _ = tx.send(mapped);
                                continue;
                            }
                        }
                    }
                }

                if let Ok(punch) = parse_punch_packet(packet) {
                    let session_id = {
                        let hashes = self.session_hashes.lock().await;
                        hashes.get(&punch.session_hash).cloned()
                    };
                    if let Some(session_id) = session_id {
                        if punch.kind == PunchPacketKind::Probe {
                            let ack = build_punch_packet(PunchPacketKind::Ack, &session_id);
                            let _ = self.udp_socket.send_to(&ack, remote).await;
                        }
                        let peer_npub = self.pending_handoffs.lock().await.remove(&session_id);
                        if let Some(peer_npub) = peer_npub {
                            (*on_handoff)(session_id.clone(), peer_npub, remote).await?;
                            break;
                        }
                        continue;
                    }
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        })
    }

    pub fn spawn_advertise_loop(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(self.advertise_interval_ms));
            loop {
                interval.tick().await;
                let _ = self.publish_advert().await;
            }
        })
    }

    pub async fn subscribe_dm(&self) -> Result<()> {
        self.client
            .subscribe_to(
                self.dm_relays.clone(),
                Filter::new()
                    .kind(Kind::GiftWrap)
                    .pubkey(self.pubkey)
                    .limit(0),
                None,
            )
            .await?;
        Ok(())
    }

    pub fn spawn_notify_loop(self: Arc<Self>) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let mut notifications = self.client.notifications();
            while let Ok(notification) = notifications.recv().await {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if event.kind != Kind::GiftWrap {
                        continue;
                    }

                    let unwrapped = match nip59::extract_rumor(&self.keys, &event).await {
                        Ok(unwrapped) => unwrapped,
                        Err(_) => continue,
                    };
                    let rumor = unwrapped.rumor;
                    let sender = unwrapped.sender;
                    if rumor.kind != Kind::PrivateDirectMessage {
                        continue;
                    }

                    let from_npub = sender.to_bech32()?;
                    if !self.trusted_npubs.is_empty() && !self.trusted_npubs.contains(&from_npub) {
                        println!(
                            "[reject] {}",
                            serde_json::to_string(
                                &json!({"reason":"untrusted-npub","fromNpub":from_npub})
                            )?
                        );
                        continue;
                    }

                    if let Ok(offer) = serde_json::from_str::<TraversalOffer>(&rumor.content) {
                        if offer.message_type == "offer"
                            && offer.recipient_npub == self.npub
                            && offer.expires_at > now_ms()
                        {
                            let now = now_ms();
                            let (reflexive_address, local_addresses) =
                                self.local_traversal_addresses().await?;
                            let accepted =
                                reflexive_address.is_some() || !local_addresses.is_empty();
                            let punch = PunchHint {
                                start_at_ms: now + self.punch_start_delay_ms,
                                interval_ms: self.punch_interval_ms,
                                duration_ms: self.punch_duration_ms,
                            };
                            let answer = create_traversal_answer(
                                offer.session_id.clone(),
                                now,
                                60_000,
                                format!("{}-answer", offer.session_id),
                                self.npub.clone(),
                                offer.sender_npub.clone(),
                                offer.nonce.clone(),
                                accepted,
                                reflexive_address,
                                local_addresses,
                                accepted.then_some(punch.clone()),
                                (!accepted).then_some("no-usable-addresses".to_owned()),
                            );

                            println!(
                                "[rendezvous] offer received {}",
                                serde_json::to_string(&json!({
                                    "fromNpub": from_npub,
                                    "sessionId": offer.session_id,
                                    "nonce": offer.nonce,
                                    "reflexiveAddress": offer.reflexive_address,
                                    "localAddresses": offer.local_addresses,
                                }))?
                            );

                            let reply_relays = self.preferred_dm_relays(sender).await?;
                            self.send_dm_to(reply_relays, sender, &answer, "answer")
                                .await?;
                            self.pending_handoffs
                                .lock()
                                .await
                                .insert(offer.session_id.clone(), offer.sender_npub.clone());

                            println!(
                                "[rendezvous] answer published {}",
                                serde_json::to_string(&json!({
                                    "toPubkey": sender.to_hex(),
                                    "sessionId": answer.session_id,
                                    "nonce": answer.nonce,
                                    "inReplyTo": answer.in_reply_to,
                                    "accepted": answer.accepted,
                                    "hasPunch": answer.punch.is_some(),
                                }))?
                            );

                            if accepted {
                                let punch = LegacyPunch {
                                    start_at_ms: punch.start_at_ms,
                                    interval_ms: punch.interval_ms,
                                    duration_ms: punch.duration_ms,
                                };
                                let remotes = Self::planned_remote_endpoints_from_offer_answer(
                                    &offer,
                                    answer.reflexive_address.as_ref(),
                                    &answer.local_addresses,
                                );
                                self.start_punch_plan(offer.session_id.clone(), remotes, punch)
                                    .await?;
                            }
                            continue;
                        }
                    }

                    if let Ok(hello) = serde_json::from_str::<LegacyHelloMessage>(&rumor.content) {
                        if hello.message_type != "fips.rendezvous.hello" || hello.nonce.is_empty() {
                            continue;
                        }

                        let endpoint = self.resolve_traversal_endpoint().await?;
                        let reply = LegacyServerInfoMessage {
                            message_type: "fips.rendezvous.server-info".to_owned(),
                            version: "1.0".to_owned(),
                            session_id: hello.session_id.clone(),
                            nonce: hello.nonce.clone(),
                            issued_at: now_ms(),
                            endpoint: endpoint.clone(),
                            punch: Some(LegacyPunch {
                                start_at_ms: now_ms() + self.punch_start_delay_ms,
                                interval_ms: self.punch_interval_ms,
                                duration_ms: self.punch_duration_ms,
                            }),
                            stun: self.stun_servers.first().map(|uri| LegacyStunInfo {
                                uri: uri.clone(),
                                metadata_tag: None,
                            }),
                        };
                        let reply_relays = self.preferred_dm_relays(sender).await?;
                        self.send_dm_to(reply_relays, sender, &reply, "server-info")
                            .await?;

                        println!(
                            "[rendezvous] server-info published {}",
                            serde_json::to_string(&json!({
                                "toPubkey": sender.to_hex(),
                                "sessionId": reply.session_id,
                                "nonce": reply.nonce,
                                "endpoint": reply.endpoint,
                                "hasPunch": reply.punch.is_some(),
                            }))?
                        );

                        if let (Some(client_endpoint), Some(punch)) =
                            (hello.client_endpoint.clone(), reply.punch.clone())
                        {
                            self.pending_handoffs
                                .lock()
                                .await
                                .insert(hello.nonce.clone(), from_npub.clone());
                            self.start_punch(hello.nonce.clone(), client_endpoint, punch)
                                .await?;
                        }
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        })
    }
}
