use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use nostr::nips::nip59;
use nostr::{Filter, Kind};
use nostr_sdk::prelude::RelayPoolNotification;
use tokio::task::JoinHandle;

use super::*;
use crate::common::{now_ms, parse_stun_binding_success};
use crate::{
    build_punch_packet, parse_punch_packet, LegacyEndpoint, PunchPacketKind, TraversalAdvert,
    ADVERT_KIND,
};

impl ClientRuntimeCore {
    pub fn spawn_udp_loop<F, Fut>(self: Arc<Self>, extra_handler: F) -> JoinHandle<Result<()>>
    where
        F: Fn(Vec<u8>, SocketAddr) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<bool>> + Send + 'static,
    {
        let mut udp_shutdown_rx = self.udp_shutdown.subscribe();
        let extra_handler = Arc::new(extra_handler);
        tokio::spawn(async move {
            let mut buf = vec![0_u8; 64 * 1024];
            loop {
                let (len, remote) = tokio::select! {
                    changed = udp_shutdown_rx.changed() => {
                        match changed {
                            Ok(()) if *udp_shutdown_rx.borrow() => break,
                            Ok(()) => continue,
                            Err(_) => break,
                        }
                    }
                    recv = self.udp_socket.recv_from(&mut buf) => recv?,
                };
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
                        let hashes = self.punch_hashes.lock().await;
                        hashes.get(&punch.session_hash).cloned()
                    };
                    if let Some(session_id) = session_id {
                        if punch.kind == PunchPacketKind::Probe {
                            let ack = build_punch_packet(PunchPacketKind::Ack, &session_id);
                            let _ = self.udp_socket.send_to(&ack, remote).await;
                        }
                        if let Some(tx) = self.pending_punch.lock().await.remove(&session_id) {
                            let _ = tx.send(LegacyEndpoint {
                                host: remote.ip().to_string(),
                                port: remote.port(),
                            });
                        }
                        continue;
                    }
                }

                if (*extra_handler)(packet.to_vec(), remote).await? {
                    continue;
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        })
    }

    pub fn spawn_notify_loop(self: Arc<Self>) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let mut notifications = self.client.notifications();
            while let Ok(notification) = notifications.recv().await {
                match notification {
                    RelayPoolNotification::Event { event, .. } if event.kind == Kind::GiftWrap => {
                        if let Ok(unwrapped) = nip59::extract_rumor(&self.keys, &event).await {
                            if unwrapped.rumor.kind != Kind::PrivateDirectMessage {
                                continue;
                            }
                            if let Ok(msg) =
                                serde_json::from_str::<TraversalAnswer>(&unwrapped.rumor.content)
                            {
                                if msg.message_type == "answer" {
                                    if let Some(tx) =
                                        self.pending_answer.lock().await.remove(&msg.in_reply_to)
                                    {
                                        let _ = tx.send(msg);
                                        continue;
                                    }
                                }
                            }
                            if let Ok(msg) = serde_json::from_str::<LegacyServerInfoMessage>(
                                &unwrapped.rumor.content,
                            ) {
                                if msg.message_type == "fips.rendezvous.server-info" {
                                    if let Some(tx) =
                                        self.pending_server_info.lock().await.remove(&msg.nonce)
                                    {
                                        let _ = tx.send(msg);
                                    }
                                }
                            }
                        }
                    }
                    RelayPoolNotification::Event { event, .. }
                        if event.kind == Kind::Custom(ADVERT_KIND) =>
                    {
                        if let Ok(advert) = serde_json::from_str::<TraversalAdvert>(&event.content)
                        {
                            if advert.expires_at > now_ms() {
                                let mut cache = self.advert_cache.write().await;
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
                    }
                    _ => {}
                }
            }
            Ok::<(), anyhow::Error>(())
        })
    }

    pub fn spawn_subscriptions(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self
                .client
                .subscribe_to(
                    self.dm_relays.clone(),
                    Filter::new()
                        .kind(Kind::GiftWrap)
                        .pubkey(self.pubkey)
                        .limit(0),
                    None,
                )
                .await
            {
                tracing::error!("failed to subscribe to DM relays: {err:#}");
            }

            if let Err(err) = self
                .client
                .subscribe_to(
                    self.advert_relays.clone(),
                    Filter::new().kind(Kind::Custom(ADVERT_KIND)),
                    None,
                )
                .await
            {
                tracing::error!("failed to subscribe to advert relays: {err:#}");
            }
        })
    }
}
