## 2026-04-28 11:32 UTC
- `fips.key` contained a real-looking `nsec1...` and was tracked in this branch history before cleanup at tip. We removed key files from current tree/PR diff and added ignores, but **history still contains exposed key material**.
- Blocker for secure release posture: rotate/regenerate any potentially used key and (if required by policy) do authorized history rewrite in a separate approved operation.

## Android FIPS mobile via Nostr UDP holepunch blockers

- Android build compatibility is unproven for the current `fips` crate. The target must compile with daemon/TUN/gateway-only pieces disabled or gated.
- Current embedded API gap: no single library surface wraps Nostr discovery -> traversal event -> `adopt_established_traversal` -> session readiness.
- App-service API gap: `send_session_data` is internal and inbound non-IPv6 service ports are dropped instead of delivered to app callbacks/queues.
- Mobile runtime constraints: app lifecycle, Doze/background limits, network switching, and UDP socket lifetime may interrupt traversal/session state.
- NAT reality: Nostr/STUN UDP punching is proven but not universal; carrier-grade/symmetric NAT or restricted mobile networks may require peer-assist/helper fallback.
- Pushstr bridge/package gap: no Flutter Rust bridge bindings or Android NDK build pipeline exists for embedded FIPS yet.
- Pi node readiness: target nodes need stable identity, Nostr advert/signaling config, STUN/relay config, and receiver-agent/service-port handling.
