//! Minimal STUN Binding responder for public UDP FIPS nodes.
//!
//! This intentionally implements only RFC 5389/8489 Binding Request handling:
//! enough for peers to discover their server-reflexive address without turning
//! the FIPS transport into a general-purpose STUN/TURN stack.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

const STUN_HEADER_LEN: usize = 20;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS_RESPONSE: u16 = 0x0101;
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpStunServerConfig {
    pub enabled: bool,
    pub rate_limit_per_ip_per_minute: u32,
}

impl Default for UdpStunServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rate_limit_per_ip_per_minute: 120,
        }
    }
}

pub(super) fn is_stun_packet(packet: &[u8]) -> bool {
    packet.len() >= STUN_HEADER_LEN
        && packet[0] & 0b1100_0000 == 0
        && u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]) == STUN_MAGIC_COOKIE
}

pub(super) fn binding_response_for(request: &[u8], remote_addr: SocketAddr) -> Option<Vec<u8>> {
    if !is_stun_packet(request) {
        return None;
    }

    let message_type = u16::from_be_bytes([request[0], request[1]]);
    if message_type != STUN_BINDING_REQUEST {
        return None;
    }

    let declared_len = u16::from_be_bytes([request[2], request[3]]) as usize;
    if request.len() < STUN_HEADER_LEN + declared_len {
        return None;
    }

    let mut value = Vec::new();
    value.push(0);
    match remote_addr.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01);
            let port = remote_addr.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
            value.extend_from_slice(&port.to_be_bytes());
            let ip_octets = ip.octets();
            let cookie_octets = STUN_MAGIC_COOKIE.to_be_bytes();
            for (addr_byte, cookie_byte) in ip_octets.iter().zip(cookie_octets.iter()) {
                value.push(addr_byte ^ cookie_byte);
            }
        }
        IpAddr::V6(ip) => {
            value.push(0x02);
            let port = remote_addr.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
            value.extend_from_slice(&port.to_be_bytes());
            let ip_octets = ip.octets();
            let cookie_octets = STUN_MAGIC_COOKIE.to_be_bytes();
            for i in 0..16 {
                let xor_byte = if i < 4 {
                    cookie_octets[i]
                } else {
                    request[8 + (i - 4)]
                };
                value.push(ip_octets[i] ^ xor_byte);
            }
        }
    }

    let attr_len = value.len() as u16;
    let message_len = 4 + value.len();
    let mut response = Vec::with_capacity(STUN_HEADER_LEN + message_len);
    response.extend_from_slice(&STUN_BINDING_SUCCESS_RESPONSE.to_be_bytes());
    response.extend_from_slice(&(message_len as u16).to_be_bytes());
    response.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    response.extend_from_slice(&request[8..20]);
    response.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    response.extend_from_slice(&attr_len.to_be_bytes());
    response.extend_from_slice(&value);
    while response.len() % 4 != 0 {
        response.push(0);
    }
    Some(response)
}

pub(super) struct StunRateLimiter {
    limit: u32,
    windows: HashMap<IpAddr, (Instant, u32)>,
}

impl StunRateLimiter {
    pub(super) fn new(limit: u32) -> Self {
        Self {
            limit,
            windows: HashMap::new(),
        }
    }

    pub(super) fn allow(&mut self, ip: IpAddr) -> bool {
        if self.limit == 0 {
            return true;
        }

        let now = Instant::now();
        let entry = self.windows.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= RATE_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn binding_request() -> Vec<u8> {
        let mut request = Vec::new();
        request.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
        request.extend_from_slice(&0u16.to_be_bytes());
        request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        request.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        request
    }

    #[test]
    fn binding_response_contains_xor_mapped_ipv4_address() {
        let request = binding_request();
        let remote = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 9), 54321));
        let response = binding_response_for(&request, remote).expect("response");

        assert_eq!(
            u16::from_be_bytes([response[0], response[1]]),
            STUN_BINDING_SUCCESS_RESPONSE
        );
        assert_eq!(&response[8..20], &request[8..20]);
        assert_eq!(
            u16::from_be_bytes([response[20], response[21]]),
            ATTR_XOR_MAPPED_ADDRESS
        );
        assert_eq!(u16::from_be_bytes([response[22], response[23]]), 8);
        assert_eq!(response[25], 0x01);

        let mapped_port =
            u16::from_be_bytes([response[26], response[27]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
        assert_eq!(mapped_port, remote.port());

        let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
        let mapped_ip = Ipv4Addr::new(
            response[28] ^ cookie[0],
            response[29] ^ cookie[1],
            response[30] ^ cookie[2],
            response[31] ^ cookie[3],
        );
        assert_eq!(mapped_ip, Ipv4Addr::new(203, 0, 113, 9));
    }

    #[test]
    fn binding_response_contains_xor_mapped_ipv6_address() {
        let request = binding_request();
        let remote = SocketAddr::from((Ipv6Addr::LOCALHOST, 23456));
        let response = binding_response_for(&request, remote).expect("response");

        assert_eq!(u16::from_be_bytes([response[22], response[23]]), 20);
        assert_eq!(response[25], 0x02);
        let mapped_port =
            u16::from_be_bytes([response[26], response[27]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
        assert_eq!(mapped_port, remote.port());
    }

    #[test]
    fn ignores_non_stun_and_non_binding_packets() {
        assert!(!is_stun_packet(b"not stun"));
        assert!(binding_response_for(b"not stun", "127.0.0.1:1".parse().unwrap()).is_none());

        let mut request = binding_request();
        request[1] = 0x02;
        assert!(binding_response_for(&request, "127.0.0.1:1".parse().unwrap()).is_none());
    }

    #[test]
    fn rate_limiter_enforces_per_ip_limit() {
        let mut limiter = StunRateLimiter::new(2);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(!limiter.allow(ip));
    }
}
