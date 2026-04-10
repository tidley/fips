use sha2::{Digest, Sha256};

use crate::{
    PunchPacket, PunchPacketKind, RendezvousError, SessionFrame, StunEndpoint, PUNCH_ACK_MAGIC,
    PUNCH_MAGIC,
};

pub fn parse_stun_url(input: &str) -> Result<StunEndpoint, RendezvousError> {
    let raw = input.strip_prefix("stun:").unwrap_or(input);
    let Some((host, port)) = raw.rsplit_once(':') else {
        return Err(RendezvousError::InvalidStunUrl(input.to_owned()));
    };
    let port = port
        .parse::<u16>()
        .map_err(|_| RendezvousError::InvalidStunUrl(input.to_owned()))?;
    if host.is_empty() {
        return Err(RendezvousError::InvalidStunUrl(input.to_owned()));
    }
    Ok(StunEndpoint {
        host: host.to_owned(),
        port,
    })
}

pub fn session_hash(session_id: &str) -> [u8; 16] {
    let digest = Sha256::digest(session_id.as_bytes());
    let mut output = [0_u8; 16];
    output.copy_from_slice(&digest[..16]);
    output
}

pub fn build_punch_packet(kind: PunchPacketKind, session_id: &str) -> [u8; 20] {
    let magic = match kind {
        PunchPacketKind::Probe => PUNCH_MAGIC,
        PunchPacketKind::Ack => PUNCH_ACK_MAGIC,
    };
    let mut packet = [0_u8; 20];
    packet[..4].copy_from_slice(&magic.to_be_bytes());
    packet[4..].copy_from_slice(&session_hash(session_id));
    packet
}

pub fn parse_punch_packet(bytes: &[u8]) -> Result<PunchPacket, RendezvousError> {
    if bytes.len() < 20 {
        return Err(RendezvousError::InvalidPunchPacketLength);
    }
    let magic = u32::from_be_bytes(bytes[..4].try_into().expect("fixed slice length"));
    let kind = match magic {
        PUNCH_MAGIC => PunchPacketKind::Probe,
        PUNCH_ACK_MAGIC => PunchPacketKind::Ack,
        _ => return Err(RendezvousError::InvalidPunchPacketMagic),
    };
    let mut session_hash = [0_u8; 16];
    session_hash.copy_from_slice(&bytes[4..20]);
    Ok(PunchPacket { kind, session_hash })
}

pub fn encode_session_frame(frame: &SessionFrame) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"FIPS1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(frame)?);
    Ok(bytes)
}

pub fn decode_session_frame(bytes: &[u8]) -> Result<SessionFrame, RendezvousError> {
    if bytes.len() < 5 {
        return Err(RendezvousError::InvalidPunchPacketLength);
    }
    if &bytes[..5] != b"FIPS1" {
        return Err(RendezvousError::InvalidPunchPacketMagic);
    }
    serde_json::from_slice(&bytes[5..]).map_err(|_| RendezvousError::InvalidPunchPacketMagic)
}
