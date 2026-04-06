use std::env;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{LegacyEndpoint, DEFAULT_ADVERT_RELAYS, DEFAULT_DM_RELAYS, DEFAULT_STUN_SERVERS};

pub fn default_advert_relays() -> Vec<String> {
    DEFAULT_ADVERT_RELAYS
        .iter()
        .map(|value| value.to_string())
        .collect()
}

pub fn default_dm_relays() -> Vec<String> {
    DEFAULT_DM_RELAYS
        .iter()
        .map(|value| value.to_string())
        .collect()
}

pub fn default_stun_servers() -> Vec<String> {
    DEFAULT_STUN_SERVERS
        .iter()
        .map(|value| value.to_string())
        .collect()
}

pub fn parse_csv_env_list(name: &str) -> Option<Vec<String>> {
    let raw = env::var(name).ok()?;
    let values = raw
        .split(',')
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn nonce() -> String {
    format!("{}-{:x}", now_ms(), rand::random::<u64>())
}

pub fn local_ipv4_hint() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        _ => None,
    }
}

pub fn create_stun_binding_request(txn_id: [u8; 12]) -> [u8; 20] {
    const STUN_BINDING_REQUEST: u16 = 0x0001;
    const STUN_MAGIC_COOKIE: u32 = 0x2112_a442;
    let mut packet = [0_u8; 20];
    packet[..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    packet[2..4].copy_from_slice(&0_u16.to_be_bytes());
    packet[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    packet[8..20].copy_from_slice(&txn_id);
    packet
}

fn parse_mapped_address(value: &[u8]) -> Option<LegacyEndpoint> {
    if value.len() < 8 || value[1] != 0x01 {
        return None;
    }
    Some(LegacyEndpoint {
        host: Ipv4Addr::new(value[4], value[5], value[6], value[7]).to_string(),
        port: u16::from_be_bytes([value[2], value[3]]),
    })
}

fn parse_xor_mapped_address(value: &[u8]) -> Option<LegacyEndpoint> {
    const STUN_MAGIC_COOKIE: u32 = 0x2112_a442;
    if value.len() < 8 || value[1] != 0x01 {
        return None;
    }
    let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
    let ip = Ipv4Addr::new(
        value[4] ^ cookie[0],
        value[5] ^ cookie[1],
        value[6] ^ cookie[2],
        value[7] ^ cookie[3],
    );
    Some(LegacyEndpoint {
        host: ip.to_string(),
        port: u16::from_be_bytes([value[2], value[3]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16),
    })
}

pub fn parse_stun_binding_success(packet: &[u8], txn_id: &[u8; 12]) -> Option<LegacyEndpoint> {
    const STUN_BINDING_SUCCESS: u16 = 0x0101;
    const STUN_MAGIC_COOKIE: u32 = 0x2112_a442;
    const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;
    const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

    if packet.len() < 20 {
        return None;
    }
    if u16::from_be_bytes(packet[..2].try_into().ok()?) != STUN_BINDING_SUCCESS {
        return None;
    }
    if u32::from_be_bytes(packet[4..8].try_into().ok()?) != STUN_MAGIC_COOKIE {
        return None;
    }
    if &packet[8..20] != txn_id {
        return None;
    }

    let message_length = u16::from_be_bytes(packet[2..4].try_into().ok()?) as usize;
    let mut offset = 20usize;
    let max_offset = packet.len().min(20 + message_length);

    while offset + 4 <= max_offset {
        let attr_type = u16::from_be_bytes(packet[offset..offset + 2].try_into().ok()?);
        let attr_len = u16::from_be_bytes(packet[offset + 2..offset + 4].try_into().ok()?) as usize;
        let value_start = offset + 4;
        let value_end = value_start + attr_len;
        if value_end > packet.len() {
            break;
        }
        let value = &packet[value_start..value_end];
        let parsed = match attr_type {
            STUN_ATTR_XOR_MAPPED_ADDRESS => parse_xor_mapped_address(value),
            STUN_ATTR_MAPPED_ADDRESS => parse_mapped_address(value),
            _ => None,
        };
        if parsed.is_some() {
            return parsed;
        }
        offset = value_end + ((4 - (attr_len % 4)) % 4);
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StunObservation {
    pub server: String,
    pub reflexive_address: Option<LegacyEndpoint>,
    pub local_port: u16,
    pub local_interface_addresses: Vec<String>,
}

pub fn log_traversal_observation(role: &str, observation: Option<&StunObservation>) {
    if let Some(observation) = observation {
        println!(
            "[traversal] observation {}",
            serde_json::to_string(&json!({
                "role": role,
                "server": observation.server,
                "reflexiveAddress": observation.reflexive_address,
                "localPort": observation.local_port,
                "localInterfaceAddresses": observation.local_interface_addresses,
            }))
            .unwrap_or_else(|_| "{\"role\":\"log-error\"}".to_owned())
        );
        if observation.reflexive_address.is_none() {
            println!(
                "[traversal] warning {}",
                serde_json::to_string(&json!({
                    "role": role,
                    "warning": "no reflexive address discovered; traversal may require LAN or configured public host fallback",
                }))
                .unwrap_or_else(|_| "{\"role\":\"log-error\"}".to_owned())
            );
        }
    }
}

pub fn log_stun_attempt(
    role: &str,
    stun_url: &str,
    local_port: u16,
    local_interface_addresses: &[String],
) {
    println!(
        "[traversal] stun-attempt {}",
        serde_json::to_string(&json!({
            "role": role,
            "server": stun_url,
            "localPort": local_port,
            "localInterfaceAddresses": local_interface_addresses,
        }))
        .unwrap_or_else(|_| "{\"role\":\"log-error\"}".to_owned())
    );
}

pub fn log_stun_result(
    role: &str,
    stun_url: &str,
    local_port: u16,
    local_interface_addresses: &[String],
    result: Result<&LegacyEndpoint, &str>,
) {
    match result {
        Ok(reflexive) => println!(
            "[traversal] stun-result {}",
            serde_json::to_string(&json!({
                "role": role,
                "server": stun_url,
                "localPort": local_port,
                "localInterfaceAddresses": local_interface_addresses,
                "reflexiveAddress": reflexive,
                "status": "ok",
            }))
            .unwrap_or_else(|_| "{\"role\":\"log-error\"}".to_owned())
        ),
        Err(error) => println!(
            "[traversal] stun-result {}",
            serde_json::to_string(&json!({
                "role": role,
                "server": stun_url,
                "localPort": local_port,
                "localInterfaceAddresses": local_interface_addresses,
                "error": error,
                "status": "error",
            }))
            .unwrap_or_else(|_| "{\"role\":\"log-error\"}".to_owned())
        ),
    }
}

pub fn log_publish_outcome(
    kind: &str,
    target: &str,
    success: &std::collections::HashSet<nostr::RelayUrl>,
    failed: &std::collections::HashMap<nostr::RelayUrl, String>,
) {
    println!(
        "[rendezvous] publish outcomes {}",
        serde_json::to_string(&json!({
            "logContext": {"kind": kind, "target": target},
            "summary": success
                .iter()
                .map(|relay| json!({"relay": relay.to_string(), "status": "fulfilled"}))
                .chain(failed.iter().map(|(relay, reason)| json!({"relay": relay.to_string(), "status": "rejected", "reason": reason})))
                .collect::<Vec<_>>(),
        }))
        .unwrap_or_else(|_| "{\"kind\":\"log-error\"}".to_owned())
    );
}
