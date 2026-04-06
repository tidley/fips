use crate::{
    AddressSource, PlannedPunchTarget, PunchHint, PunchStrategy, PunchWindow, TraversalAddress,
    TraversalAnswer, TraversalOffer, TRAVERSAL_SIGNAL_APP, TRAVERSAL_SIGNAL_KIND,
};

fn same_subnet_24(left: &TraversalAddress, right: &TraversalAddress) -> bool {
    let left_parts = left.ip.split('.').collect::<Vec<_>>();
    let right_parts = right.ip.split('.').collect::<Vec<_>>();
    left_parts.len() == 4 && right_parts.len() == 4 && left_parts[..3] == right_parts[..3]
}

pub fn create_traversal_offer(
    session_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    reflexive_address: Option<TraversalAddress>,
    local_addresses: Vec<TraversalAddress>,
) -> TraversalOffer {
    TraversalOffer {
        app: TRAVERSAL_SIGNAL_APP.to_owned(),
        event_kind: TRAVERSAL_SIGNAL_KIND,
        message_type: "offer".to_owned(),
        session_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
        reflexive_address,
        local_addresses,
    }
}

pub fn create_traversal_answer(
    session_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    in_reply_to: String,
    accepted: bool,
    reflexive_address: Option<TraversalAddress>,
    local_addresses: Vec<TraversalAddress>,
    punch: Option<PunchHint>,
    reason: Option<String>,
) -> TraversalAnswer {
    TraversalAnswer {
        app: TRAVERSAL_SIGNAL_APP.to_owned(),
        event_kind: TRAVERSAL_SIGNAL_KIND,
        message_type: "answer".to_owned(),
        session_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
        in_reply_to,
        accepted,
        reflexive_address,
        local_addresses,
        punch,
        reason,
    }
}

pub fn validate_traversal_answer_for_offer(
    offer: &TraversalOffer,
    answer: &TraversalAnswer,
    now: u64,
) -> Result<(), &'static str> {
    if offer.app != TRAVERSAL_SIGNAL_APP || answer.app != TRAVERSAL_SIGNAL_APP {
        return Err("unsupported-app");
    }
    if offer.event_kind != TRAVERSAL_SIGNAL_KIND || answer.event_kind != TRAVERSAL_SIGNAL_KIND {
        return Err("unsupported-event-kind");
    }
    if offer.message_type != "offer" || answer.message_type != "answer" {
        return Err("invalid-type");
    }
    if offer.expires_at <= now || answer.expires_at <= now {
        return Err("expired-signal");
    }
    if offer.session_id != answer.session_id || answer.in_reply_to != offer.nonce {
        return Err("session-mismatch");
    }
    if offer.sender_npub != answer.recipient_npub || offer.recipient_npub != answer.sender_npub {
        return Err("identity-mismatch");
    }
    if answer.issued_at < offer.issued_at {
        return Err("answer-precedes-offer");
    }
    if answer.accepted && answer.reflexive_address.is_none() && answer.local_addresses.is_empty() {
        return Err("missing-addresses");
    }
    if !answer.accepted && answer.reason.as_deref().unwrap_or_default().is_empty() {
        return Err("missing-rejection-reason");
    }
    Ok(())
}

pub fn plan_punch_targets(
    local_addresses: &[TraversalAddress],
    local_reflexive_address: Option<&TraversalAddress>,
    remote_addresses: &[TraversalAddress],
    remote_reflexive_address: Option<&TraversalAddress>,
) -> Vec<PlannedPunchTarget> {
    let mut planned = Vec::new();

    let mut push_unique = |target: PlannedPunchTarget| {
        if !planned.iter().any(|existing| existing == &target) {
            planned.push(target);
        }
    };

    for local in local_addresses {
        for remote in remote_addresses {
            if same_subnet_24(local, remote) {
                push_unique(PlannedPunchTarget {
                    strategy: PunchStrategy::Lan,
                    local_source: AddressSource::Local,
                    remote_source: AddressSource::Local,
                    local: local.clone(),
                    remote: remote.clone(),
                });
            }
        }
    }

    if let (Some(local), Some(remote)) = (local_reflexive_address, remote_reflexive_address) {
        push_unique(PlannedPunchTarget {
            strategy: PunchStrategy::Reflexive,
            local_source: AddressSource::Reflexive,
            remote_source: AddressSource::Reflexive,
            local: local.clone(),
            remote: remote.clone(),
        });
    }

    if let Some(remote) = remote_reflexive_address {
        for local in local_addresses {
            push_unique(PlannedPunchTarget {
                strategy: PunchStrategy::Mixed,
                local_source: AddressSource::Local,
                remote_source: AddressSource::Reflexive,
                local: local.clone(),
                remote: remote.clone(),
            });
        }
    }

    if let Some(local) = local_reflexive_address {
        for remote in remote_addresses {
            push_unique(PlannedPunchTarget {
                strategy: PunchStrategy::Mixed,
                local_source: AddressSource::Reflexive,
                remote_source: AddressSource::Local,
                local: local.clone(),
                remote: remote.clone(),
            });
        }
    }

    for local in local_addresses {
        for remote in remote_addresses {
            push_unique(PlannedPunchTarget {
                strategy: PunchStrategy::Local,
                local_source: AddressSource::Local,
                remote_source: AddressSource::Local,
                local: local.clone(),
                remote: remote.clone(),
            });
        }
    }

    planned
}

pub fn negotiate_punch_window(
    now_ms: u64,
    local_lead_ms: u64,
    remote_lead_ms: u64,
    local_interval_ms: u64,
    remote_interval_ms: u64,
    local_duration_ms: u64,
    remote_duration_ms: u64,
) -> PunchWindow {
    PunchWindow {
        start_at_ms: now_ms + local_lead_ms.max(remote_lead_ms),
        interval_ms: local_interval_ms.max(remote_interval_ms).max(20),
        duration_ms: local_duration_ms.max(remote_duration_ms).max(1),
    }
}

pub fn build_punch_attempt_schedule(window: PunchWindow, max_attempts: usize) -> Vec<u64> {
    let mut attempts = Vec::new();
    let max_attempts = max_attempts.max(1);
    let interval_ms = window.interval_ms.max(1);
    let cutoff = window.start_at_ms + window.duration_ms.max(1);
    let mut at = window.start_at_ms;

    while attempts.len() < max_attempts && (attempts.is_empty() || at < cutoff) {
        attempts.push(at);
        at += interval_ms;
    }

    attempts
}
