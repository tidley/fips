# Android FIPS Drop PoC

## Objective

Build a Pushstr-style Android proof of concept that can send, receive, and communicate directly with a FIPS node using an embedded app-owned FIPS library, not a system-wide VPN/TUN gateway.

Primary proof:

```text
Android app
  -> embedded FIPS node/library
  -> Nostr advert/signaling bootstrap
  -> UDP STUN/hole-punch traversal
  -> FIPS session/app-service channel
  -> Pi4ssd storage/agent node
```

The PoC should prove the product path, not the existing repo demo harnesses.

## Key Planning Correction

The previous Dropbox plan listed a separate Android FIPS/VPN app as the fastest Phase 1. That is not the desired direction for this PoC.

For this track, treat embedded FIPS as the first-class Phase 1:

- no Android system-wide VPN dependency
- no requirement for a FIPS TUN interface on Android
- no sidecar/gateway assumption for the Android client
- FIPS communication lives inside a Rust library called from Flutter via `flutter_rust_bridge`
- Nostr discovery/signaling is still the intended bootstrap path for creating the UDP traversal
- after the UDP socket is punched/adopted, file/message traffic rides FIPS sessions, not normal internet HTTP

## Current Technical Read

### Pushstr side

Local Pushstr already has useful mobile foundations:

- Flutter app at `/home/tom/code/pushstr/mobile`
- Rust FFI crate at `/home/tom/code/pushstr/pushstr_rust`
- `flutter_rust_bridge` already in use
- Blossom upload helpers already exist in Rust
- file picker/share-oriented dependencies are already present

Important limitation:

- current Blossom upload target is hard-coded to `https://blossom.primal.net`
- FIPS transport is not integrated
- uploads currently use normal HTTP, not FIPS session transport

### FIPS side

Local FIPS is already a Rust library plus daemon binaries, but the app-facing API is not ready yet.

Useful base:

- `fips` crate exposes `Node`, `Config`, identity and transport types
- `Node` can be started with TUN disabled in config
- end-to-end session data already supports service ports internally
- Nostr discovery can advertise NAT UDP endpoints and signal traversal offers/answers
- `NostrDiscovery::request_connect(...)` can initiate bootstrap against a peer configured with `via_nostr`
- `NostrDiscovery::drain_events()` returns established traversal events
- `Node::adopt_established_traversal(...)` hands a punched UDP socket into normal FIPS handshake flow

Important gaps for Android embedded use:

- no Android/mobile crate or generated Flutter bridge
- no public app-service API for `send_session_data`
- inbound non-IPv6 service ports are currently dropped as unknown
- no callback/queue for app payloads delivered to a service port
- no Android build profile/feature set proving `fips` compiles without daemon/TUN assumptions
- no single embedded API that wraps: Nostr bootstrap -> UDP traversal adoption -> FIPS session readiness -> app payload send/receive

## Recommended PoC Shape

Do not start by making Blossom-over-HTTP work through a VPN-like FIPS interface.

Instead, build the same connection shape the product needs:

```text
Android embedded FIPS
  1. use Nostr relays for endpoint adverts/signaling
  2. perform STUN-assisted UDP hole punch to the target node
  3. adopt the punched UDP socket into FIPS
  4. establish the normal FIPS peer/session layer
  5. send FIPS Drop app payloads over a reserved FSP service port
```

Then layer Blossom/file semantics on that proven direct FIPS path.

### PoC Protocol v0

Use one reserved FSP service port for FIPS Drop control/data, for example:

```text
port 4242 = fips-drop-v0
```

Message envelope:

```json
{
  "type": "hello" | "put" | "put_chunk" | "put_done" | "get" | "ack" | "error",
  "id": "uuid-or-random-id",
  "name": "filename.ext",
  "mime": "image/jpeg",
  "sha256": "...",
  "size": 12345,
  "chunk_index": 0,
  "chunk_count": 10,
  "data_b64": "..."
}
```

For the first proof, keep it deliberately simple:

1. Android sends `hello` to Pi4ssd agent.
2. Pi4ssd replies `ack`.
3. Android sends a small file as one `put` or chunked `put_chunk` messages.
4. Pi4ssd writes the file under a test directory.
5. Pi4ssd sends `put_done` with hash/status.
6. Android shows success and stores local history.

Blossom compatibility can come next by mapping the stored object and manifest onto Blossom/Nostr semantics after the FIPS app-channel is proven.

## Implementation Plan

### Phase 1 - Embedded Nostr bootstrap + UDP traversal API

Deliverables in `/home/tom/code/fips`:

- Add a public embedded bootstrap API that wraps the already-tested Nostr traversal flow:
  - start embedded node with TUN disabled
  - configure Nostr discovery relays/STUN servers
  - request connection to a peer by npub/peer config using Nostr adverts/signaling
  - drain bootstrap events
  - adopt established UDP traversal into the node
  - report peer/session readiness
- Keep this API library-friendly: no daemon assumptions, no shelling out to `fipsctl`, no Android VPN/TUN requirement.
- Add tests around the wrapper using existing Nostr traversal primitives/mocks where possible.

Acceptance:

- a Rust test or small example can drive: `request_connect` -> established traversal event -> `adopt_established_traversal` -> FIPS peer handshake/session readiness
- the path runs with TUN disabled

### Phase 2 - FIPS embedded app-service API

Deliverables in `/home/tom/code/fips`:

- Add a public app-service API around the existing session layer:
  - use the Phase 1 bootstrap API to connect/initiate session to a known node
  - send payload to a service port
  - receive payloads for registered service ports through a callback/channel
  - expose status/peer/session state
- Add service-port dispatch for non-IPv6 ports instead of dropping them.
- Add tests proving two in-process FIPS nodes can exchange payloads on port `4242` with no TUN.

Acceptance:

- a Rust test or small example starts two nodes, establishes a session, sends `hello` on port `4242`, receives `ack`, and shuts down cleanly
- no TUN/device/VPN requirement in the test path

### Phase 3 - Mobile bridge crate

Deliverables:

- Create a small FFI/mobile surface, either:
  - inside Pushstr's existing `pushstr_rust` crate by adding `fips` as a path dependency, or
  - as a dedicated `fips_mobile` crate consumed by Pushstr
- Prefer `flutter_rust_bridge` to match Pushstr's existing stack.
- Expose minimal functions:

```text
fips_init(config_json) -> handle/status
fips_start() -> status
fips_stop() -> status
fips_status() -> json
fips_send_message(target_npub_or_addr, bytes/json) -> result
fips_poll_events() -> [events]
fips_send_file(target, path/bytes, metadata) -> transfer_id
```

Acceptance:

- generated Dart bindings compile
- Android target builds the Rust library without requiring VPN/TUN privileges

### Phase 4 - Pi4ssd receiver agent

Deliverables:

- A minimal Rust receiver binary or mode on Pi4ssd using the same FIPS app-service API.
- It listens on service port `4242` and stores received files under a configured directory.
- It returns acknowledgements and hash verification results.

Acceptance:

- devbox or Android client can send `hello` and one small file to Pi4ssd over FIPS session transport
- received file hash matches sender hash

### Phase 5 - Android Pushstr-style UI

Deliverables in Pushstr mobile:

- Settings screen for:
  - local FIPS identity/config
  - target Pi4ssd npub/node address/alias
  - optional relay/discovery config
- Send screen and Android share intent path:
  - choose/share a file
  - show target
  - send via embedded FIPS
  - show progress/status
- Basic receive/history list:
  - sent files
  - received acknowledgements
  - incoming messages/files if enabled

Acceptance:

- install APK
- start embedded FIPS from app
- send file to Pi4ssd node
- Pi4ssd receives file
- app receives ack/status
- no separate VPN/FIPS Android app is required

### Phase 6 - Blossom/Nostr alignment

Only after Phase 1-5 prove direct FIPS app messaging:

- map stored files to Blossom-compatible paths/metadata
- reuse Pushstr media descriptor format
- emit Nostr/NIP-17 manifest events for sync/history
- add optional client-side encryption before transfer

## Immediate Next Course of Action

1. Done: build a library-friendly embedded bootstrap wrapper around Nostr discovery + STUN/UDP traversal + `adopt_established_traversal`.
2. Done: prove the wrapper with TUN disabled and a normal FIPS session established over the adopted traversal path.
3. Done: build the app-service API on top of that connection path.
4. Done: create the Pi4ssd receiver-agent logic against that API.
5. Done: package the receiver as a small runnable `fips-dropbox-agent` binary.
6. Done: add a Rust `mobile` facade for Android/Flutter wrappers:
   - start embedded FIPS with TUN/DNS/control disabled,
   - request Nostr traversal to a known npub,
   - initiate/wait for FSP service sessions,
   - send a FIPS Drop blob to service port `4242`,
   - receive ACK/status service packets.
7. Done: bridge `fips::mobile::FipsMobileClient` into Pushstr's Flutter/Rust mobile stack.
8. Done: install/run `fips-dropbox-agent` on Pi and send a file from Android over both Wi-Fi and 4G.
9. Done: document the reproducible physical PoC in `docs/pocs/fips-drop-android-pi.md`.
10. Next: add Blossom/Nostr file metadata now that raw direct FIPS transfer works.

The critical first slice is not UI and not Blossom. It is:

```text
embedded FIPS node + Nostr bootstrap + UDP hole punch + adopted FIPS session + no TUN
```

That slice now exists in Rust tests and has a runnable Pi-side receiver binary. Android now becomes a Flutter/FRB integration task plus real-device traversal testing rather than a networking architecture gamble.

## Current Local Test Entry Points

Pi4ssd receiver:

```bash
cargo run --bin fips-dropbox-agent --features nostr-discovery -- \
  --config /path/to/pi4ssd-dropbox-fips.yaml \
  --storage-root /tmp/fips-dropbox-inbox \
  --port 4242
```

Android/Flutter bridge target:

- Rust facade: `fips::mobile::FipsMobileClient`
- Start: `FipsMobileClient::start_from_yaml(...)`
- Connect: `connect_npub(pi4ssd_npub)`
- Session: `ensure_session_npub(...)` then `wait_for_session_npub(...)`
- Upload: `send_dropbox_blob_to_npub(pi4ssd_npub, name, mime, bytes)`
- ACK/status receive: `recv_service_packet()`

## Open Decisions

- Should the app-service API live in the main `fips` crate or a sibling crate such as `fips-app` / `fips-mobile`?
- Should the first Android bridge add `fips` directly to Pushstr's `pushstr_rust`, or should it consume a separate `fips_mobile` library?
- Which identity should be used for the Android app: existing Pushstr Nostr key, a dedicated FIPS key, or derived/separate keys linked by profile metadata?
