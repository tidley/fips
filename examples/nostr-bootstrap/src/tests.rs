use crate::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum NatType {
    RestrictedCone,
    PortRestricted,
    Symmetric,
}

fn attempt_direct_probe(
    local_targets: &[PlannedPunchTarget],
    remote_targets: &[PlannedPunchTarget],
    local_nat: NatType,
    remote_nat: NatType,
    max_attempts: usize,
) -> bool {
    let max_attempts = max_attempts.max(1);
    for _ in 0..max_attempts {
        for local in local_targets {
            for remote in remote_targets {
                if is_pair_reachable(&local.local, &remote.remote, local_nat, remote_nat) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_pair_reachable(
    local: &TraversalAddress,
    remote: &TraversalAddress,
    local_nat: NatType,
    remote_nat: NatType,
) -> bool {
    if local.protocol != "udp" || remote.protocol != "udp" {
        return false;
    }
    if local_nat == NatType::Symmetric || remote_nat == NatType::Symmetric {
        return false;
    }
    if local_nat == NatType::PortRestricted && remote_nat == NatType::PortRestricted {
        return false;
    }
    true
}

fn address(ip: &str, port: u16) -> TraversalAddress {
    TraversalAddress {
        protocol: "udp".to_owned(),
        ip: ip.to_owned(),
        port,
    }
}

#[test]
fn parses_stun_urls() {
    let parsed = parse_stun_url("stun:fips.tomdwyer.uk:3478").unwrap();
    assert_eq!(parsed.host, "fips.tomdwyer.uk");
    assert_eq!(parsed.port, 3478);
}

#[test]
fn rejects_bad_stun_urls() {
    assert_eq!(
        parse_stun_url("stun:bad"),
        Err(RendezvousError::InvalidStunUrl("stun:bad".to_owned()))
    );
}

#[test]
fn builds_and_parses_probe_packets() {
    let packet = build_punch_packet(PunchPacketKind::Probe, "sess-1");
    let parsed = parse_punch_packet(&packet).unwrap();
    assert_eq!(parsed.kind, PunchPacketKind::Probe);
    assert_eq!(parsed.session_hash, session_hash("sess-1"));
}

#[test]
fn builds_and_parses_ack_packets() {
    let packet = build_punch_packet(PunchPacketKind::Ack, "sess-1");
    let parsed = parse_punch_packet(&packet).unwrap();
    assert_eq!(parsed.kind, PunchPacketKind::Ack);
    assert_eq!(parsed.session_hash, session_hash("sess-1"));
}

#[test]
fn serializes_advert_schema() {
    let advert = TraversalAdvert {
        app: "fips.nat.traversal.v1".to_owned(),
        event_kind: ADVERT_KIND,
        protocol: "fips.nat.traversal.v1".to_owned(),
        publisher_npub: "npub1server".to_owned(),
        published_at: 1,
        expires_at: 2,
        sequence: 1,
        relays: DEFAULT_DM_RELAYS
            .iter()
            .map(|relay| relay.to_string())
            .collect(),
        stun_servers: DEFAULT_STUN_SERVERS
            .iter()
            .map(|server| server.to_string())
            .collect(),
        transports: vec!["udp".to_owned()],
        endpoint_hint: Some(EndpointHint {
            host: "203.0.113.10".to_owned(),
            port: 9999,
        }),
    };

    let json = serde_json::to_string(&advert).unwrap();
    assert!(json.contains("\"eventKind\":30078"));
    assert!(json.contains("\"stunServers\""));
}

#[test]
fn encodes_and_decodes_session_frames() {
    let frame = SessionFrame {
        session_id: "sess-1".to_owned(),
        frame_type: "data".to_owned(),
        channel: Some("shell".to_owned()),
        payload: serde_json::json!({"cmd": "pwd"}),
        at: 42,
    };
    let encoded = encode_session_frame(&frame).unwrap();
    let decoded = decode_session_frame(&encoded).unwrap();
    assert_eq!(decoded.session_id, "sess-1");
    assert_eq!(decoded.channel.as_deref(), Some("shell"));
    assert_eq!(decoded.payload["cmd"], "pwd");
}

#[test]
fn creates_and_validates_matching_offer_answer() {
    let offer = create_traversal_offer(
        "sess-1".to_owned(),
        1_700_000_000_000,
        60_000,
        "offer-1".to_owned(),
        "npub1client".to_owned(),
        "npub1server".to_owned(),
        Some(address("203.0.113.10", 62000)),
        vec![address("192.168.1.10", 62000)],
    );
    let answer = create_traversal_answer(
        "sess-1".to_owned(),
        1_700_000_000_500,
        60_000,
        "answer-1".to_owned(),
        "npub1server".to_owned(),
        "npub1client".to_owned(),
        "offer-1".to_owned(),
        true,
        Some(address("198.51.100.20", 63000)),
        vec![address("192.168.1.20", 63000)],
        Some(PunchHint {
            start_at_ms: 1_700_000_000_800,
            interval_ms: 300,
            duration_ms: 30_000,
        }),
        None,
    );

    assert_eq!(offer.message_type, "offer");
    assert_eq!(answer.message_type, "answer");
    assert_eq!(offer.app, TRAVERSAL_SIGNAL_APP);
    assert!(validate_traversal_answer_for_offer(&offer, &answer, 1_700_000_000_900).is_ok());
}

#[test]
fn rejects_answer_without_addresses_when_accepted() {
    let offer = create_traversal_offer(
        "sess-1".to_owned(),
        1_700_000_000_000,
        60_000,
        "offer-1".to_owned(),
        "npub1client".to_owned(),
        "npub1server".to_owned(),
        Some(address("203.0.113.10", 62000)),
        vec![],
    );
    let answer = create_traversal_answer(
        "sess-1".to_owned(),
        1_700_000_000_500,
        60_000,
        "answer-1".to_owned(),
        "npub1server".to_owned(),
        "npub1client".to_owned(),
        "offer-1".to_owned(),
        true,
        None,
        vec![],
        None,
        None,
    );

    assert_eq!(
        validate_traversal_answer_for_offer(&offer, &answer, 1_700_000_000_900),
        Err("missing-addresses")
    );
}

#[test]
fn plans_lan_then_reflexive_then_fallback_targets() {
    let planned = plan_punch_targets(
        &[address("192.168.1.10", 62000)],
        Some(&address("203.0.113.10", 62000)),
        &[address("192.168.1.20", 63000)],
        Some(&address("198.51.100.20", 63000)),
    );

    assert_eq!(planned[0].strategy, PunchStrategy::Lan);
    assert_eq!(planned[1].strategy, PunchStrategy::Reflexive);
    assert!(planned
        .iter()
        .any(|target| target.strategy == PunchStrategy::Mixed));
    assert!(planned
        .iter()
        .any(|target| target.strategy == PunchStrategy::Local));
}

#[test]
fn negotiates_window_and_builds_bounded_schedule() {
    let window = negotiate_punch_window(1_700_000_000_000, 1_000, 2_000, 150, 300, 10_000, 30_000);
    assert_eq!(
        window,
        PunchWindow {
            start_at_ms: 1_700_000_002_000,
            interval_ms: 300,
            duration_ms: 30_000,
        }
    );
    let schedule = build_punch_attempt_schedule(window, 4);
    assert_eq!(
        schedule,
        vec![
            1_700_000_002_000,
            1_700_000_002_300,
            1_700_000_002_600,
            1_700_000_002_900
        ]
    );
}

#[test]
fn simulated_lan_scenario_prefers_lan_and_establishes() {
    let offer = create_traversal_offer(
        "sess-lan".to_owned(),
        1_700_000_000_000,
        60_000,
        "offer-lan".to_owned(),
        "npub1client".to_owned(),
        "npub1server".to_owned(),
        Some(address("203.0.113.10", 62000)),
        vec![address("192.168.1.10", 62000)],
    );
    let answer = create_traversal_answer(
        "sess-lan".to_owned(),
        1_700_000_000_500,
        60_000,
        "answer-lan".to_owned(),
        "npub1server".to_owned(),
        "npub1client".to_owned(),
        "offer-lan".to_owned(),
        true,
        Some(address("198.51.100.20", 63000)),
        vec![address("192.168.1.20", 63000)],
        None,
        None,
    );
    let targets = plan_punch_targets(
        &offer.local_addresses,
        offer.reflexive_address.as_ref(),
        &answer.local_addresses,
        answer.reflexive_address.as_ref(),
    );
    let window = negotiate_punch_window(1_700_000_000_600, 1_000, 1_000, 200, 200, 10_000, 10_000);
    let schedule = build_punch_attempt_schedule(window, 4);

    assert_eq!(targets[0].strategy, PunchStrategy::Lan);
    assert!(attempt_direct_probe(
        &targets,
        &targets,
        NatType::RestrictedCone,
        NatType::RestrictedCone,
        schedule.len()
    ));
}

#[test]
fn simulated_symmetric_nat_scenario_requires_fallback() {
    let offer = create_traversal_offer(
        "sess-symmetric".to_owned(),
        1_700_000_000_000,
        60_000,
        "offer-symmetric".to_owned(),
        "npub1client".to_owned(),
        "npub1server".to_owned(),
        Some(address("203.0.113.10", 62000)),
        vec![address("192.168.1.10", 62000)],
    );
    let answer = create_traversal_answer(
        "sess-symmetric".to_owned(),
        1_700_000_000_500,
        60_000,
        "answer-symmetric".to_owned(),
        "npub1server".to_owned(),
        "npub1client".to_owned(),
        "offer-symmetric".to_owned(),
        true,
        Some(address("198.51.100.20", 63000)),
        vec![address("192.168.1.20", 63000)],
        None,
        None,
    );
    let targets = plan_punch_targets(
        &offer.local_addresses,
        offer.reflexive_address.as_ref(),
        &answer.local_addresses,
        answer.reflexive_address.as_ref(),
    );
    let window = negotiate_punch_window(1_700_000_000_600, 1_000, 1_000, 200, 200, 10_000, 10_000);
    let schedule = build_punch_attempt_schedule(window, 4);

    assert!(!attempt_direct_probe(
        &targets,
        &targets,
        NatType::Symmetric,
        NatType::Symmetric,
        schedule.len()
    ));
}
