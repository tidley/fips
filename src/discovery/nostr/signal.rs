use nostr::EventId;
use nostr::nips::{nip44, nip59};
use nostr::prelude::{
    Event, EventBuilder, JsonUtil, Kind, NostrSigner, PublicKey, Tag, Timestamp, UnsignedEvent,
};

use super::types::{
    AssistGrant, AssistObserved, AssistRequest, BootstrapError, PEER_ASSIST_MAGIC, PeerAssistProbe,
    PunchHint, SIGNAL_KIND, TraversalAnswer, TraversalOffer,
};

pub(super) struct SignalEnvelope<T> {
    pub(super) payload: T,
    pub(super) event_id: EventId,
    pub(super) sender_npub: String,
}

pub(super) struct UnwrappedSignal {
    pub(super) sender: PublicKey,
    pub(super) rumor: UnsignedEvent,
}

pub(super) async fn build_signal_event(
    signer: &nostr::Keys,
    receiver: PublicKey,
    rumor: UnsignedEvent,
    expiration: Timestamp,
) -> Result<Event, BootstrapError> {
    let seal = nip59::make_seal(signer, &receiver, rumor)
        .await
        .map_err(|e| BootstrapError::Nostr(e.to_string()))?
        .sign(signer)
        .await
        .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

    let ephemeral = nostr::Keys::generate();
    let content = nip44::encrypt(
        ephemeral.secret_key(),
        &receiver,
        seal.as_json(),
        nip44::Version::default(),
    )
    .map_err(|e| BootstrapError::Nostr(e.to_string()))?;

    EventBuilder::new(Kind::Custom(SIGNAL_KIND), content)
        .tags([Tag::public_key(receiver), Tag::expiration(expiration)])
        .sign_with_keys(&ephemeral)
        .map_err(|e| BootstrapError::Nostr(e.to_string()))
}

pub(super) async fn unwrap_signal_event(
    signer: &nostr::Keys,
    event: &Event,
) -> Result<UnwrappedSignal, BootstrapError> {
    if event.kind != Kind::Custom(SIGNAL_KIND) {
        return Err(BootstrapError::Protocol(
            "not a traversal signal".to_string(),
        ));
    }

    let seal_json = signer
        .nip44_decrypt(&event.pubkey, &event.content)
        .await
        .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
    let seal =
        Event::from_json(seal_json).map_err(|e| BootstrapError::EventParse(e.to_string()))?;
    seal.verify()
        .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
    let rumor_json = signer
        .nip44_decrypt(&seal.pubkey, &seal.content)
        .await
        .map_err(|e| BootstrapError::Nostr(e.to_string()))?;
    let rumor = UnsignedEvent::from_json(rumor_json)
        .map_err(|e| BootstrapError::EventParse(e.to_string()))?;
    Ok(UnwrappedSignal {
        sender: seal.pubkey,
        rumor,
    })
}

pub(super) fn validate_offer_freshness(
    offer: &TraversalOffer,
    now: u64,
    signal_ttl_ms: u64,
    actual_sender_npub: &str,
    local_npub: &str,
) -> Result<(), BootstrapError> {
    if offer.message_type != "offer" {
        return Err(BootstrapError::Protocol("invalid-offer".to_string()));
    }
    if offer.expires_at <= now || now.saturating_sub(offer.issued_at) > signal_ttl_ms {
        return Err(BootstrapError::Protocol("expired-offer".to_string()));
    }
    if offer.sender_npub != actual_sender_npub || offer.recipient_npub != local_npub {
        return Err(BootstrapError::Protocol("identity-mismatch".to_string()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_traversal_offer(
    session_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    reflexive_address: Option<super::TraversalAddress>,
    local_addresses: Vec<super::TraversalAddress>,
    stun_server: Option<String>,
) -> TraversalOffer {
    TraversalOffer {
        message_type: "offer".to_string(),
        session_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
        reflexive_address,
        local_addresses,
        stun_server,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_traversal_answer(
    session_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    in_reply_to: String,
    accepted: bool,
    reflexive_address: Option<super::TraversalAddress>,
    local_addresses: Vec<super::TraversalAddress>,
    stun_server: Option<String>,
    punch: Option<PunchHint>,
    reason: Option<String>,
) -> TraversalAnswer {
    TraversalAnswer {
        message_type: "answer".to_string(),
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
        stun_server,
        punch,
        reason,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_assist_request(
    request_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
) -> AssistRequest {
    AssistRequest {
        message_type: "assist-request".to_string(),
        request_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_assist_grant(
    request_id: String,
    grant_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    in_reply_to: String,
    accepted: bool,
    helper_addr: Option<String>,
    probe_token: Option<String>,
    max_uses: Option<u8>,
    reason: Option<String>,
) -> AssistGrant {
    AssistGrant {
        message_type: "assist-grant".to_string(),
        request_id,
        grant_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
        in_reply_to,
        accepted,
        helper_addr,
        probe_token,
        max_uses,
        reason,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create_assist_observed(
    request_id: String,
    grant_id: String,
    issued_at: u64,
    ttl_ms: u64,
    nonce: String,
    sender_npub: String,
    recipient_npub: String,
    in_reply_to: String,
    accepted: bool,
    helper_addr: String,
    observed_address: Option<super::TraversalAddress>,
    reason: Option<String>,
) -> AssistObserved {
    AssistObserved {
        message_type: "assist-observed".to_string(),
        request_id,
        grant_id,
        issued_at,
        expires_at: issued_at + ttl_ms,
        nonce,
        sender_npub,
        recipient_npub,
        in_reply_to,
        accepted,
        helper_addr,
        observed_address,
        reason,
    }
}

pub(super) fn validate_traversal_answer_for_offer(
    offer: &TraversalOffer,
    answer: &TraversalAnswer,
    now: u64,
    signal_ttl_ms: u64,
    actual_sender_npub: &str,
    local_npub: &str,
) -> Result<(), BootstrapError> {
    if answer.message_type != "answer" {
        return Err(BootstrapError::Protocol("invalid-answer".to_string()));
    }
    if offer.expires_at <= now
        || answer.expires_at <= now
        || now.saturating_sub(answer.issued_at) > signal_ttl_ms
    {
        return Err(BootstrapError::Protocol("expired-answer".to_string()));
    }
    if offer.session_id != answer.session_id || answer.in_reply_to != offer.nonce {
        return Err(BootstrapError::Protocol("session-mismatch".to_string()));
    }
    if offer.sender_npub != local_npub
        || offer.recipient_npub != actual_sender_npub
        || answer.sender_npub != actual_sender_npub
        || answer.recipient_npub != local_npub
    {
        return Err(BootstrapError::Protocol("identity-mismatch".to_string()));
    }
    if answer.accepted && answer.reflexive_address.is_none() && answer.local_addresses.is_empty() {
        return Err(BootstrapError::Protocol("missing-addresses".to_string()));
    }
    if !answer.accepted && answer.reason.as_deref().unwrap_or_default().is_empty() {
        return Err(BootstrapError::Protocol(
            "missing-rejection-reason".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_assist_request_freshness(
    request: &AssistRequest,
    now: u64,
    signal_ttl_ms: u64,
    actual_sender_npub: &str,
    local_npub: &str,
) -> Result<(), BootstrapError> {
    if request.message_type != "assist-request" {
        return Err(BootstrapError::Protocol(
            "invalid-assist-request".to_string(),
        ));
    }
    if request.expires_at <= now || now.saturating_sub(request.issued_at) > signal_ttl_ms {
        return Err(BootstrapError::Protocol(
            "expired-assist-request".to_string(),
        ));
    }
    if request.sender_npub != actual_sender_npub || request.recipient_npub != local_npub {
        return Err(BootstrapError::Protocol("identity-mismatch".to_string()));
    }
    if request.request_id.trim().is_empty() || request.nonce.trim().is_empty() {
        return Err(BootstrapError::Protocol(
            "missing-assist-request-fields".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_assist_grant_for_request(
    request: &AssistRequest,
    grant: &AssistGrant,
    now: u64,
    signal_ttl_ms: u64,
    actual_sender_npub: &str,
    local_npub: &str,
) -> Result<(), BootstrapError> {
    if grant.message_type != "assist-grant" {
        return Err(BootstrapError::Protocol("invalid-assist-grant".to_string()));
    }
    if grant.expires_at <= now || now.saturating_sub(grant.issued_at) > signal_ttl_ms {
        return Err(BootstrapError::Protocol("expired-assist-grant".to_string()));
    }
    if grant.request_id != request.request_id || grant.in_reply_to != request.nonce {
        return Err(BootstrapError::Protocol("session-mismatch".to_string()));
    }
    if grant.sender_npub != actual_sender_npub
        || grant.recipient_npub != local_npub
        || request.sender_npub != local_npub
        || request.recipient_npub != actual_sender_npub
    {
        return Err(BootstrapError::Protocol("identity-mismatch".to_string()));
    }
    if grant.accepted
        && (grant
            .helper_addr
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
            || grant
                .probe_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()))
    {
        return Err(BootstrapError::Protocol(
            "missing-grant-parameters".to_string(),
        ));
    }
    if !grant.accepted && grant.reason.as_deref().unwrap_or_default().is_empty() {
        return Err(BootstrapError::Protocol(
            "missing-rejection-reason".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_assist_observed_for_grant(
    grant: &AssistGrant,
    observed: &AssistObserved,
    now: u64,
    signal_ttl_ms: u64,
    actual_sender_npub: &str,
    local_npub: &str,
) -> Result<(), BootstrapError> {
    if observed.message_type != "assist-observed" {
        return Err(BootstrapError::Protocol(
            "invalid-assist-observed".to_string(),
        ));
    }
    if observed.expires_at <= now || now.saturating_sub(observed.issued_at) > signal_ttl_ms {
        return Err(BootstrapError::Protocol(
            "expired-assist-observed".to_string(),
        ));
    }
    if observed.request_id != grant.request_id
        || observed.grant_id != grant.grant_id
        || observed.in_reply_to != grant.nonce
    {
        return Err(BootstrapError::Protocol("session-mismatch".to_string()));
    }
    if observed.sender_npub != actual_sender_npub
        || observed.recipient_npub != local_npub
        || grant.sender_npub != actual_sender_npub
        || grant.recipient_npub != local_npub
    {
        return Err(BootstrapError::Protocol("identity-mismatch".to_string()));
    }
    if observed.accepted && observed.observed_address.is_none() {
        return Err(BootstrapError::Protocol(
            "missing-observed-address".to_string(),
        ));
    }
    if !observed.accepted && observed.reason.as_deref().unwrap_or_default().is_empty() {
        return Err(BootstrapError::Protocol(
            "missing-rejection-reason".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn build_peer_assist_probe(grant_id: &str, token: &str) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8 + grant_id.len() + token.len());
    packet.extend_from_slice(&PEER_ASSIST_MAGIC.to_be_bytes());
    packet.extend_from_slice(&(grant_id.len() as u16).to_be_bytes());
    packet.extend_from_slice(&(token.len() as u16).to_be_bytes());
    packet.extend_from_slice(grant_id.as_bytes());
    packet.extend_from_slice(token.as_bytes());
    packet
}

pub(super) fn parse_peer_assist_probe(data: &[u8]) -> Option<PeerAssistProbe> {
    if data.len() < 8 {
        return None;
    }
    if u32::from_be_bytes(data[0..4].try_into().ok()?) != PEER_ASSIST_MAGIC {
        return None;
    }
    let grant_id_len = u16::from_be_bytes(data[4..6].try_into().ok()?) as usize;
    let token_len = u16::from_be_bytes(data[6..8].try_into().ok()?) as usize;
    if grant_id_len == 0 || token_len == 0 || data.len() != 8 + grant_id_len + token_len {
        return None;
    }
    let grant_id = std::str::from_utf8(&data[8..8 + grant_id_len])
        .ok()?
        .to_string();
    let token = std::str::from_utf8(&data[8 + grant_id_len..])
        .ok()?
        .to_string();
    Some(PeerAssistProbe { grant_id, token })
}
