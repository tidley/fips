use std::collections::HashMap;
use std::net::SocketAddr;

use nostr::prelude::PublicKey;
use tokio::sync::{Mutex, RwLock, oneshot};

use super::signal::SignalEnvelope;
use super::types::{AssistGrant, AssistObserved};

pub(super) const RATE_LIMIT_MAX_KEYS: usize = 4096;

pub(super) type RateLimitWindow = HashMap<String, Vec<u64>>;

#[derive(Debug, Clone)]
pub(super) struct PendingPrivateAssistGrant {
    pub(super) request_id: String,
    pub(super) grant_id: String,
    pub(super) grant_nonce: String,
    pub(super) probe_token: String,
    pub(super) sender_pubkey: PublicKey,
    pub(super) sender_npub: String,
    pub(super) helper_addr: SocketAddr,
    pub(super) relays: Vec<String>,
    pub(super) expires_at: u64,
}

pub(super) struct PeerAssistState {
    pending_grants: Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<AssistGrant>>>>,
    pending_observed: Mutex<HashMap<String, oneshot::Sender<SignalEnvelope<AssistObserved>>>>,
    pending_private_grants: Mutex<HashMap<String, PendingPrivateAssistGrant>>,
    helper_endpoints: RwLock<Vec<SocketAddr>>,
    request_windows: Mutex<RateLimitWindow>,
}

impl PeerAssistState {
    pub(super) fn new() -> Self {
        Self {
            pending_grants: Mutex::new(HashMap::new()),
            pending_observed: Mutex::new(HashMap::new()),
            pending_private_grants: Mutex::new(HashMap::new()),
            helper_endpoints: RwLock::new(Vec::new()),
            request_windows: Mutex::new(HashMap::new()),
        }
    }

    pub(super) async fn update_helper_endpoints(&self, mut endpoints: Vec<SocketAddr>) {
        endpoints.sort();
        endpoints.dedup();
        *self.helper_endpoints.write().await = endpoints;
    }

    pub(super) async fn first_helper_endpoint(&self) -> Option<SocketAddr> {
        self.helper_endpoints.read().await.first().copied()
    }

    pub(super) async fn insert_grant_waiter(
        &self,
        nonce: String,
        tx: oneshot::Sender<SignalEnvelope<AssistGrant>>,
    ) {
        self.pending_grants.lock().await.insert(nonce, tx);
    }

    pub(super) async fn remove_grant_waiter(&self, nonce: &str) {
        self.pending_grants.lock().await.remove(nonce);
    }

    pub(super) async fn complete_grant(
        &self,
        in_reply_to: &str,
        envelope: SignalEnvelope<AssistGrant>,
    ) {
        if let Some(tx) = self.pending_grants.lock().await.remove(in_reply_to) {
            let _ = tx.send(envelope);
        }
    }

    pub(super) async fn insert_observed_waiter(
        &self,
        nonce: String,
        tx: oneshot::Sender<SignalEnvelope<AssistObserved>>,
    ) {
        self.pending_observed.lock().await.insert(nonce, tx);
    }

    pub(super) async fn remove_observed_waiter(&self, nonce: &str) {
        self.pending_observed.lock().await.remove(nonce);
    }

    pub(super) async fn complete_observed(
        &self,
        in_reply_to: &str,
        envelope: SignalEnvelope<AssistObserved>,
    ) {
        if let Some(tx) = self.pending_observed.lock().await.remove(in_reply_to) {
            let _ = tx.send(envelope);
        }
    }

    pub(super) async fn try_insert_private_grant(
        &self,
        grant: PendingPrivateAssistGrant,
        now: u64,
        max_pending: usize,
    ) -> bool {
        let mut pending = self.pending_private_grants.lock().await;
        prune_expired_private_grants(&mut pending, now);
        if pending.len() >= max_pending {
            return false;
        }
        pending.insert(grant.grant_id.clone(), grant);
        true
    }

    pub(super) async fn remove_private_grant(&self, grant_id: &str) {
        self.pending_private_grants.lock().await.remove(grant_id);
    }

    pub(super) async fn matching_private_grant(
        &self,
        grant_id: &str,
        token: &str,
        helper_addr: SocketAddr,
        now: u64,
    ) -> Option<PendingPrivateAssistGrant> {
        let mut pending = self.pending_private_grants.lock().await;
        prune_expired_private_grants(&mut pending, now);
        let entry = pending.get(grant_id)?;
        if entry.helper_addr != helper_addr || entry.probe_token != token {
            return None;
        }
        Some(entry.clone())
    }

    pub(super) async fn request_allowed_in_window(
        &self,
        sender_npub: &str,
        now: u64,
        window_ms: u64,
        max_requests: usize,
    ) -> bool {
        let mut windows = self.request_windows.lock().await;
        allow_in_rate_window(&mut windows, sender_npub, now, window_ms, max_requests)
    }
}

fn prune_expired_private_grants(
    pending: &mut HashMap<String, PendingPrivateAssistGrant>,
    now: u64,
) {
    pending.retain(|_, entry| entry.expires_at > now);
}

pub(super) fn allow_in_rate_window(
    windows: &mut RateLimitWindow,
    key: &str,
    now: u64,
    window_ms: u64,
    max_requests: usize,
) -> bool {
    if max_requests == 0 {
        return false;
    }

    windows.retain(|_, timestamps| {
        timestamps.retain(|timestamp| now.saturating_sub(*timestamp) <= window_ms);
        !timestamps.is_empty()
    });

    if !windows.contains_key(key)
        && windows.len() >= RATE_LIMIT_MAX_KEYS
        && let Some(oldest_key) = windows
            .iter()
            .min_by_key(|(_, timestamps)| timestamps.first().copied().unwrap_or(u64::MAX))
            .map(|(key, _)| key.clone())
    {
        windows.remove(&oldest_key);
    }

    let entry = windows.entry(key.to_string()).or_default();
    if entry.len() >= max_requests {
        return false;
    }
    entry.push(now);
    true
}
