# DECISIONS

## 2026-05-04 - Android FIPS Drop PoC direction

The Android FIPS Drop proof of concept should use embedded, app-owned FIPS communication rather than a separate Android FIPS/VPN app or system-wide gateway.

Implications:

- TUN/VPN is not part of the Android MVP path.
- Nostr advert/signaling bootstrap is the intended initial connection path.
- The app should use the already-tested Nostr/STUN UDP hole-punch flow to establish direct phone-to-node UDP connectivity.
- The punched UDP socket should then be adopted into FIPS and used for the normal FIPS handshake/session layer.
- Android should call Rust through `flutter_rust_bridge`, matching Pushstr's existing mobile stack.
- FIPS Drop app payloads should ride a FIPS service-port channel after the FIPS session is ready.
- Blossom/file metadata comes after direct FIPS payload transfer works.

## 2026-05-04 - Product PoC sequence

For Pushstr/FIPS Drop, prove the direct app path first:

1. embedded FIPS node with TUN disabled
2. Nostr bootstrap/signaling and STUN-assisted UDP hole punch
3. `adopt_established_traversal` into FIPS
4. FIPS peer/session readiness over the punched UDP path
5. service-port payload send/receive
6. Pi4ssd receiver agent
7. Android Flutter/Rust bridge
8. Pushstr-style file send/share UI
9. Blossom/Nostr manifest alignment

## 2026-05-04 - Repo harnesses are not the focus for this track

Existing FIPS harnesses remain useful for other validation, but this product PoC should not be driven by sidecar, NAT, static, or gateway demo examples. The focus is Android app -> embedded FIPS library -> Nostr UDP bootstrap/holepunch -> FIPS node communication.
