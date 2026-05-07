# FIPS Embedded Client API

## Goal

Expose FIPS as an in-process Rust client suitable for mobile apps such as Pushstr. The client should let an app connect to private FIPS services without creating a kernel TUN device or Android `VpnService`.

The first target is:

```text
Pushstr Android -> embedded FIPS client -> Pi4ssd Blossom service
```

## Product Boundary

FIPS should provide private transport and service primitives. Pushstr should own Blossom and file-transfer semantics.

Good FIPS primitives:

```text
init runtime
connect peer
ensure end-to-end session
resolve advertised service
open service stream/request
send service payload
receive service payload
report status
shutdown
```

Avoid baking these into FIPS:

```text
Blossom upload policy
Pushstr file manifests
NIP-17 message history
UI transfer state
```

## Existing Hook

FSP `DataPacket` already carries a 16-bit port header inside the end-to-end encrypted payload:

```text
[src_port:2 LE][dst_port:2 LE][service payload...]
```

Port `256` is reserved for the IPv6 shim. This can become the in-process service boundary for embedded clients.

## Initial FIPS-Side API

The first FIPS-side surface should be intentionally small:

```rust
let mut rx = node.register_service_port(port, queue_depth)?;
node.ensure_service_session(&peer_identity).await?;
while !node.has_established_service_session(peer_identity.node_addr()) {
    // Drive/poll the FIPS runtime until the handshake completes.
}
node.send_service_data(peer_identity.node_addr(), src_port, dst_port, bytes).await?;
```

Incoming encrypted payloads are delivered as:

```rust
ServicePacket {
    src_addr,
    src_port,
    dst_port,
    payload,
}
```

This is datagram-shaped. A later stream or HTTP helper can be built on top once framing, backpressure, and request correlation are clear.

## Mobile Library Crate

The supported Rust package boundary for app wrappers is `crates/fips-mobile`.
It re-exports the embedded client from `src/mobile.rs` and builds `rlib`,
`staticlib`, and `cdylib` artifacts so Android and later iOS bindings can target
a mobile-specific crate instead of the root daemon package.

Initial Android builds should use:

```bash
cargo ndk -t arm64-v8a build \
  -p fips-mobile \
  --release \
  --no-default-features \
  --features nostr-discovery
```

This crate deliberately keeps the current PoC API compatible while adding
FIPS Drop product-name aliases. The Pushstr Flutter/Rust bridge now depends on
this crate and exposes a product-named `fipsMobileSendFipsDropBlob` binding.
The older `dropbox` binding remains as a wrapper until downstream callers no
longer need it.

## Pushstr Shape

Pushstr can wrap the FIPS-side API behind its own `FipsClient` facade:

```text
fips_init(identity, relays, config)
fips_connect_peer(npub)
fips_resolve_service("pi4ssd-storage")
fips_open_service("pi4ssd-storage", "blossom")
fips_upload_blob(target, file_stream, metadata)
fips_status()
fips_shutdown()
```

For the MVP, Pushstr may implement HTTP-ish request/response framing over the datagram service port. If that becomes awkward, FIPS should add a stream abstraction rather than special-casing Blossom.

## Identity

MVP can use the same Nostr key for Pushstr and the embedded FIPS node.

Longer term:

```text
Pushstr user identity
  signs binding to
FIPS device identity
```

That allows a phone's FIPS device npub to be rotated or revoked without changing the user's social/app identity.

## Non-Goals For MVP

- Whole-device VPN routing.
- Android `VpnService`.
- Per-app routing.
- Public internet egress.
- LAN-wide gateway access.

Those can be layered later once the app-scoped service path works.
