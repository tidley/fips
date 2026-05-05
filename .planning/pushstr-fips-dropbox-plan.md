# Pushstr + FIPS Dropbox Plan

## Goal

Build a Pushstr-styled, multi-platform file-drop experience that uses the FIPS network as the private transport. The first proof of concept is:

```text
Android phone -> FIPS network -> Pi4ssd Blossom storage
```

The Android app should feel like Pushstr, keep Pushstr's Nostr identity/relay model, and initially send a selected file to Pi4ssd. Pi4ssd acts as the private storage anchor. The second Pi4 and Pi3 provide relay redundancy and mesh resilience.

A parallel first-class goal is to remove the current VPS-as-WireGuard-proxy dependency. The home Pis should expose private services over FIPS so phone/laptop/homelab access no longer depends on a permanent VPS. LNVPS can remain useful as an optional rendezvous/public seed, but the system should keep working without it once devices are peered.

A strong companion proof of concept is a Flutter mobile media app with a Rust FFI FIPS core. The app connects directly over UDP to a trusted homebase/routing node, then asks that node to fetch music, video, or other media from known, already-established FIPS peers. This demonstrates FIPS as a private service fabric, not only a file upload path.

## Device Roles

### Pi4ssd

Primary storage node.

- FIPS node with persistent identity.
- Blossom-compatible HTTP server backed by the 1 TB SSD.
- Regular Nostr relay with NIP-17/NIP-59-friendly behavior.
- Private upload endpoint reachable over FIPS, not exposed directly to the public internet.
- Long-term location for received files, metadata, thumbnails, and optional backups.

### Pi4

Application and observability node.

- FIPS node with persistent identity.
- Regular Nostr relay with NIP-17 support.
- Grafana-style dashboard host.
- Uses Pi4ssd for durable metrics/storage where useful.
- Monitors FIPS node health, relay health, Blossom storage, disk usage, and transfer status.

### Pi3B

Low-power support node.

- FIPS node with persistent identity.
- Lightweight Nostr relay with NIP-17 support if performance is acceptable.
- Support mesh node for resilience and low-barrier demos.
- Should remain simple: no large storage or heavy dashboards.

### LNVPS

Optional public bootstrap.

- Useful while available for public rendezvous and normal network access.
- Not required for local service availability once the Pis are peered.
- Plans and tests should include "LNVPS absent" mode.
- Should not remain the required WireGuard-style proxy for reaching home services.

## WireGuard Proxy Replacement Goal

Current pain:

```text
phone/laptop -> VPS WireGuard proxy -> home services
```

Desired shape:

```text
phone/laptop -> FIPS -> Pi/home services
```

The first replacement should be deliberately narrow:

- Reach Pi4ssd private storage from phone/laptop without WireGuard.
- Reach a small admin/status page on Pi4 or Pi4ssd without WireGuard.
- Keep LNVPS optional for discovery/rendezvous only.
- Do not expose Pi services directly to the public internet.

This is not initially a full "route all traffic through home" VPN replacement. It is a private-service access replacement. Full egress routing can be a later project.

### Replacement Modes

#### Mode A: FIPS-Native Services

Services bind to FIPS addresses or `.fips` hostnames.

Examples:

- Blossom on Pi4ssd.
- Grafana/status on Pi4.
- Nostr relays on Pi4ssd/Pi4/Pi3.
- Git/file services later.

Pros:

- Cleanest security boundary.
- No LAN-wide routing required.
- Good fit for mobile app and Pushstr.

Cons:

- Each service needs explicit FIPS address/DNS support.
- Existing clients may need `.fips` names or app config changes.

#### Mode B: FIPS Gateway To LAN

One Pi acts as a gateway from FIPS into selected LAN services.

Pros:

- Closer replacement for "VPN into home network".
- Existing homelab/proxmox services can stay where they are.

Cons:

- Larger security surface.
- Requires routing/firewall policy.
- Easier to accidentally expose too much.

Recommended sequence:

```text
Phase 1: FIPS-native services only.
Phase 2: narrowly-scoped FIPS gateway for selected homelab services.
```

## Product Shape

### Android MVP

Start from Pushstr's mobile product direction:

- Flutter UI and styling.
- Rust core via `flutter_rust_bridge`.
- Existing Nostr identity model.
- Existing Blossom attachment concepts.
- Android share-sheet integration.

Add a new "FIPS Drop" workflow:

```text
Share file from Android
  -> choose "Send to Pi4ssd"
  -> app confirms target/storage path
  -> file uploads to Pi4ssd Blossom endpoint over FIPS
  -> app records a Nostr event/DM with the file manifest
  -> upload appears in history
```

The first version does not need full chat, sync, or general internet routing. It only needs reliable file transfer from Android to Pi4ssd.

### Homebase Media PoC

A second mobile PoC can use the same embedded FIPS core, Rust FFI, and Flutter UI pattern without starting from Pushstr's product surface.

Shape:

```text
Flutter media app
  -> Rust FFI FIPS client
  -> direct UDP/FIPS connection to homebase node
  -> homebase routes/fetches media from established FIPS peers
  -> app streams or downloads the selected media
```

The homebase node is not a generic public relay or unrestricted internet proxy. It is a trusted private router with a known FIPS identity, explicit ACLs, and stable connections to media-serving peers such as Pi4ssd, homelab servers, or later dedicated media nodes.

Initial user workflow:

```text
Open mobile app
  -> connect to homebase by npub/service advert
  -> browse a small catalog
  -> tap a track/video
  -> stream or download over FIPS
```

This PoC is useful because it exercises:

- mobile Rust FFI without needing whole-device VPN mode
- direct UDP connection to a homebase node
- service discovery and service requests over FIPS
- FIPS routing through a trusted node to already-connected media peers
- realistic large-object transfer and streaming pressure
- a more compelling demo than only file upload

### Desktop/Laptop Later

Keep the design compatible with Pushstr's Linux desktop and browser-extension model:

- Same Nostr identity/profile.
- Same target Pi4ssd storage server.
- Same manifest format.
- Same relay list.
- Same Blossom upload/download logic.

## Proposed Architecture

```text
Android Pushstr-FIPS app
  Flutter UI
  Pushstr Rust Nostr/Blossom core
  Android platform share intent
  FIPS connectivity provider

FIPS connectivity provider
  Phase 1: depend on separately running FIPS Android/VPN app, if fastest
  Phase 2: embed FIPS mobile core directly into Pushstr-FIPS

Pi4ssd
  FIPS node
  Blossom server bound to FIPS-reachable address
  Nostr relay
  SSD-backed object store

Pi4/Pi3
  FIPS nodes
  Nostr relays
  observability/resilience roles
```

## Multi-Identity Security Boundaries

FIPS npubs are cheap enough that a single physical device can expose multiple logical identities. Use this as a security boundary where it removes real risk:

```text
Pi4ssd physical device
  pi4ssd-storage identity
  pi4ssd-admin identity
  pi4ssd-relay identity
```

Each identity should have its own:

- FIPS keypair/npub.
- Config file.
- Control socket.
- UDP bind if required.
- Systemd unit.
- Service allowlist.
- Logs.

This is similar in spirit to separate logins or containers: compromise or over-permissioning of one identity should not automatically grant access to every service on the box.

### Initial Identity Split

Start with three identities on Pi4ssd:

#### `pi4ssd-storage`

- Purpose: Blossom upload/download.
- Exposed to phone, laptop, and selected homelab devices.
- Can write to SSD-backed blob storage.
- No admin dashboard or relay-management permissions.

#### `pi4ssd-admin`

- Purpose: operator access to dashboards/status/admin endpoints.
- Exposed only to trusted operator devices.
- Should not be used for casual file drop.
- Can reach Grafana/status and later selected host management endpoints.

#### `pi4ssd-relay`

- Purpose: Nostr relay service identity.
- Publicly or semi-publicly advertised depending on policy.
- No write access to Blossom storage except relay database paths.

Pi4 and Pi3 can start with one identity each. Split them later only if a real boundary appears.

### Implementation POC

The pragmatic near-term implementation is multiple FIPS daemon instances on the same Pi:

```text
/etc/fips/storage.yaml      -> /run/fips-storage/control.sock
/etc/fips/admin.yaml        -> /run/fips-admin/control.sock
/etc/fips/relay.yaml        -> /run/fips-relay/control.sock
```

Systemd units:

```text
fips-storage.service
fips-admin.service
fips-relay.service
```

Service binding:

- Blossom binds only on the `pi4ssd-storage` FIPS address.
- Grafana/status binds only on the `pi4ssd-admin` FIPS address.
- Nostr relay binds only on the `pi4ssd-relay` FIPS address.

If binding to a specific FIPS address is awkward initially, use loopback ports plus local firewall or reverse proxies that enforce the intended FIPS-side access policy.

### Operational Tradeoffs

Pros:

- Clear blast-radius reduction.
- Easy key rotation and revocation by removing one npub.
- Different trust groups can be given different npubs.
- Logs and metrics naturally separate by purpose.

Cons:

- More configs and units.
- More sockets and memory use.
- More ways to misconfigure routing.
- More identities to document and back up.

Guardrail:

```text
Do not create an identity for every tiny service.
Create one only when it represents a meaningful trust boundary.
```

## Transfer Design

### First Pass

Use Blossom as the blob storage protocol and Nostr as the metadata/control protocol.

1. Android selects a file.
2. Android computes:
   - SHA-256
   - size
   - MIME type
   - original filename
3. Android signs Blossom/NIP-98-style upload authorization with the user's Nostr key.
4. Android uploads to:

```text
https://pi4ssd.fips/<blossom upload path>
```

or the equivalent FIPS IPv6/HTTP endpoint.

5. Pi4ssd stores the blob on SSD.
6. Android emits a private NIP-17 message, self-note, or app-specific event containing:
   - blob URL
   - hash
   - size
   - MIME type
   - original filename
   - upload timestamp
   - optional encrypted file key if client-side encryption is enabled

### Encryption Choices

MVP can use one of two modes:

- **Transport/private-network first:** rely on FIPS transport encryption plus server-side access control.
- **End-to-end file encryption:** encrypt file before Blossom upload and store only ciphertext on Pi4ssd.

Recommended MVP:

```text
Use client-side encryption if Pushstr attachment encryption is easy to reuse.
Otherwise ship transport/private-network first, then add E2EE in phase 2.
```

## FIPS Connectivity Options

### Chosen Path: Embedded In-Process FIPS Client

Pushstr embeds the FIPS mobile core directly and calls it from Pushstr's Rust runtime. The app does not create an Android VPN, does not route the whole phone, and does not ask Android for `VpnService` permission in the MVP.

Shape:

```text
Pushstr Flutter UI
  -> flutter_rust_bridge
  -> Pushstr Rust runtime
      -> Pushstr Nostr/events
      -> Pushstr Blossom metadata/upload logic
      -> FipsClient
          -> FIPS identity
          -> Nostr discovery/signalling
          -> NAT traversal
          -> encrypted FIPS links
          -> in-process service client API
```

Initial Pushstr-facing API:

```text
fips_init(identity, relays, config)
fips_connect_peer(npub)
fips_resolve_service("pi4ssd-storage")
fips_open_service("pi4ssd-storage", "blossom")
fips_upload_blob(target, file_stream, metadata)
fips_status()
fips_shutdown()
```

Important boundary:

```text
FIPS should expose service/stream/client primitives.
Pushstr should own Blossom semantics.
```

`fips_upload_blob` is useful as a Pushstr convenience wrapper, but the reusable FIPS primitive should be closer to:

```text
fips_open_stream(service, protocol)
```

or:

```text
fips_http_request(service, request)
```

That keeps FIPS from becoming Blossom-specific and leaves room for Grafana/status/admin services later.

Pros:

- Single app.
- No Android VPN permission for the first file-drop use case.
- Pushstr can present FIPS connection state inline.
- Better fit for Pushstr's existing Rust core and `flutter_rust_bridge` shape.
- Avoids pretending that file drop requires whole-device routing.

Cons:

- Requires a clean FIPS library API instead of only daemon/TUN behavior.
- Requires Android-compatible FIPS runtime lifecycle.
- Native build integration is harder than calling an external daemon.
- iOS remains separate later work because background networking rules differ.

### Fallback/Debug Path: External FIPS Environment

For development only, keep the ability to test Pi4ssd upload from a device that already has FIPS connectivity through another route, such as a devbox, test Android provider, or manually running FIPS daemon.

Pros:

- Useful for isolating Pushstr file UX from FIPS mobile bugs.
- Lets Pi4ssd Blossom and metadata work start before Android embedding is complete.

Cons:

- Not the product path.
- Must not become the long-term required setup.

### Default Peer Endpoint Observation

FIPS nodes should behave as lightweight STUN-like endpoints by default on their normal UDP transport. When Alice tries to connect directly to Bob and a UDP probe reaches Bob, Bob can immediately observe Alice's public source IP:port from that packet.

Bob should then:

- record Alice's observed source endpoint
- send Alice a UDP punch/probe back to that observed endpoint
- send Alice a signed/encrypted Nostr signal saying what endpoint Bob observed
- continue normal FIPS encrypted handshake once packets are flowing both ways

The intended flow is:

```text
Alice -> Bob UDP probe
Bob observes Alice as public_ip:public_port
Bob -> Alice UDP punch/probe to public_ip:public_port
Bob -> Alice Nostr signal: "I saw you as public_ip:public_port"
Alice and Bob continue the FIPS handshake over UDP
```

This makes any FIPS node that can receive a probe act as a useful endpoint observer for its peers, instead of requiring every joining node to use a public STUN service for every attempt.

Public STUN remains useful for first bootstrap, unreachable peers, or restrictive NAT/firewall cases, but ordinary FIPS nodes should provide this peer-observation behavior automatically once they are online.

### Later Path: Android VPN/Tor-Style Provider

After Pushstr file drop works, a separate FIPS provider app or VpnService mode can exist for routing other selected apps through FIPS. That belongs after the in-process service client is proven.

## Phases

### Phase 0: Baseline Inventory

Deliverables:

- Record Pi4ssd, Pi4, Pi3 npubs and FIPS IPv6 addresses.
- Confirm each node is on upstream master and has persistent identity.
- Confirm Pi4ssd can be reached over FIPS from Android or another client.
- Inventory current WireGuard/VPS use:
  - which devices connect through it
  - which services are reached through it
  - which ones need replacing first
- Choose Blossom server implementation.
- Choose relay implementation for Pi nodes.

Acceptance:

- `fipsctl show peers` shows the three Pis peered directly or via known parent.
- Pi4ssd has stable storage mount path.
- Android can resolve/reach a simple HTTP test service on Pi4ssd over FIPS.
- At least one current WireGuard-proxied service is selected as the first FIPS replacement target.

### Phase 0.5: WireGuard Proxy Exit Ramp

Deliverables:

- Document the current WireGuard proxy topology.
- List services currently accessed through the VPS.
- Classify each service:
  - FIPS-native now
  - FIPS gateway later
  - keep public/VPS for now
- Pick one low-risk service for first replacement, preferably Pi4ssd file drop or Pi4 status page.
- Define rollback: WireGuard remains available until FIPS replacement is verified.

Acceptance:

- From a phone or laptop off-LAN, reach the selected service over FIPS without using WireGuard.
- Stop WireGuard on the client and confirm the FIPS path still works.
- Stop LNVPS after peering and confirm already-peered local access still works where expected.

### Phase 1: Pi4ssd Storage Anchor

Deliverables:

- Install and configure Blossom server on Pi4ssd.
- Create the first dedicated FIPS identity: `pi4ssd-storage`.
- Bind Blossom only to FIPS-reachable interface/address where possible.
- Store blobs under SSD path, for example:

```text
/srv/fips/blossom/blobs
/srv/fips/blossom/meta
```

- Add systemd unit.
- Add basic NIP-98/Nostr auth policy.
- Add test upload/download commands from devbox over FIPS.

Acceptance:

- Upload a test file over FIPS.
- Download file over FIPS.
- File survives reboot.
- Direct LAN/public access is blocked unless explicitly intended.
- Phone/laptop can use this path without the WireGuard VPS proxy.

### Phase 2: Multi-Identity Pi4ssd Split

Deliverables:

- Add `pi4ssd-admin` FIPS identity and service unit.
- Add `pi4ssd-relay` FIPS identity and service unit.
- Bind/admin-proxy Grafana/status only through `pi4ssd-admin`.
- Bind/proxy the Nostr relay only through `pi4ssd-relay`.
- Document all three npubs and their allowed use.

Acceptance:

- A storage client cannot reach admin-only endpoints.
- A relay client cannot write arbitrary files to Blossom storage.
- Revoking/removing one npub does not break unrelated roles.

### Phase 3: Pi Relay Layer

Deliverables:

- Run Nostr relay on Pi4ssd.
- Run Nostr relay on Pi4.
- Run lightweight relay on Pi3 if acceptable.
- Ensure NIP-17/NIP-59 event kinds are accepted/stored.
- Configure Pushstr/FIPS clients to use the Pi relays plus selected public relays.

Acceptance:

- Publish and fetch NIP-17 events across Pi relays.
- Turn off one Pi relay and verify another still works.
- Confirm relays do not require LNVPS.

### Phase 4: Android File-Send MVP

Deliverables:

- Fork/branch Pushstr.
- Embed a minimal FIPS client facade into Pushstr's Rust runtime.
- Add a "FIPS Drop" target in the mobile UI.
- Add Android share intent handling for files.
- Add settings for:
  - Pi4ssd target npub/alias
  - Pi4ssd service name, initially `pi4ssd-storage`
  - relay list
  - optional encryption toggle
- Reuse Pushstr's Blossom upload code path over the embedded FIPS service client.
- Store upload history locally.

Acceptance:

- Share a photo/document from Android to the app.
- Upload completes over FIPS.
- File appears on Pi4ssd SSD.
- App shows success/failure with retry option.

### Phase 5: Metadata and Multi-Device Sync

Deliverables:

- Define a file manifest schema.
- Send manifest via NIP-17 to self or a configured device group.
- Display upload history from Nostr events, not only local state.
- Let laptop/desktop Pushstr see/download files from Pi4ssd.

Acceptance:

- Android upload appears on laptop.
- Laptop downloads the uploaded file over FIPS.
- New Android install can restore history from Nostr relays.

### Phase 5.5: Homebase Media Router PoC

Deliverables:

- Create a small Flutter mobile app, separate from Pushstr if that is faster.
- Embed the same Rust FFI FIPS client facade used by the file-drop work.
- Configure one homebase/routing node with a persistent FIPS identity.
- Add a simple media service protocol over FIPS service ports:
  - list catalog
  - fetch metadata
  - request byte range or chunk
  - cancel stream
- Let homebase fetch media from known established FIPS peers, initially Pi4ssd or a homelab media server.
- Add minimal playback/download UI:
  - music first if video streaming is too much for the first pass
  - video once chunking/backpressure is stable
- Add ACLs so only allowlisted mobile/user npubs can request media.

Acceptance:

- Mobile app connects directly to homebase over UDP/FIPS using Nostr discovery or a service advert.
- App lists at least three media items served by a downstream FIPS peer.
- App plays or downloads one music file through homebase.
- Homebase can fetch from an already-connected media peer without exposing that peer publicly.
- Turning off LNVPS does not break playback once homebase and media peers are already peered.

### Phase 6: Harden Embedded FIPS Mobile

Deliverables:

- Add in-app FIPS node lifecycle:
  - start/stop
  - status
  - peer list
  - relay/Nostr config
- Add Android background/foreground service behavior for long uploads.
- Add pairing UX for Pi4ssd service npub and service adverts.
- Add better status and retry telemetry.
- Enable default peer endpoint observation/punch-back behavior so the mobile node can use already-known FIPS peers as lightweight STUN-like observers.
- Android first.
- Keep iOS design notes but do not block Android MVP on iOS.

Acceptance:

- Single Android app starts FIPS and uploads to Pi4ssd without separate VPN app.
- App reports FIPS connected/disconnected state.
- Uploads fail clearly when FIPS is unavailable.

### Phase 6.5: Optional Android FIPS Provider

Deliverables:

- Decide whether a separate Android `VpnService` provider is still useful.
- If yes, scope it to selected apps/routes only.
- Reuse the same FIPS mobile core and identity handling as Pushstr where possible.

Acceptance:

- A non-Pushstr app can reach one allowlisted FIPS service without routing the whole phone.
- Pushstr continues to work without requiring the provider.

### Phase 7: Resilience Tests

Tests:

- Power off LNVPS: local Pi file upload still works.
- Power off Pi4: Pi4ssd upload still works.
- Power off Pi3: no impact on primary upload.
- Power off Pi4ssd: app queues upload and retries later.
- Disable one relay: NIP-17 manifest still publishes/fetches via another.
- Reboot Android: pending upload state is preserved.

Acceptance:

- A one-page test log proves each degraded mode.

## Future Work: Rich Per-Service Auth And ACLs

The multiple-daemon model is useful for an early POC, but it should not be the only long-term answer. Future FIPS work should support richer service-level authorization without requiring one full node per role.

Desired capabilities:

- Per-service identities or capability keys attached to one FIPS node.
- ACLs by peer npub, service name, route, port, or advertised endpoint.
- Declarative service adverts:

```text
service=blossom role=storage access=allowlist
service=grafana role=admin access=operator-only
service=nostr-relay role=relay access=public-or-policy
```

- Separate audit logs per service.
- Runtime `fipsctl` commands to grant/revoke service access.
- Optional quota/rate limits per peer and service.
- Integration with app-layer auth such as Blossom/NIP-98 and Nostr relay auth.

Long-term target:

```text
One physical device can run one FIPS daemon while exposing multiple isolated service surfaces.
Multiple FIPS daemons remain available for hard isolation, but are no longer required for every security boundary.
```

## Open Questions

- Which Blossom server implementation should be standardized on Pi4ssd?
- Should Pi4ssd enforce upload authorization by allowlisted npubs only?
- Should files be client-side encrypted in MVP, or phase 2?
- Should Pushstr use the same Nostr identity as its FIPS device identity for MVP, or bind a separate FIPS device npub to the user's Pushstr/Nostr identity?
- Should the FIPS in-process client expose HTTP request primitives first, or lower-level stream primitives first?
- Should peer endpoint observation be implemented as part of the existing Nostr NAT traversal messages, or as a smaller always-on UDP probe/response plus Nostr observation signal?
- Should the homebase media PoC use HTTP-compatible range requests over FIPS, or a custom chunked media protocol over service ports?
- Should homebase cache media locally, or only route/fetch from downstream FIPS peers?
- What hostname convention should be used: `pi4ssd.fips`, `dropbox.fips`, or raw FIPS IPv6?
- Should relay storage live on Pi4ssd only, or should Pi4/Pi3 keep relay persistence too?

## Immediate Next Steps

- [x] Add a FIPS-side design note for an embedded `FipsClient` API.
- [x] Identify the smallest existing FIPS library hooks for:
   - creating a node without TUN
   - connecting to a peer by npub/Nostr
   - sending app-owned bytes without exposing a kernel TUN
   - receiving app-owned bytes in-process
- [x] Add an initial Rust facade/skeleton if the hooks are already clean enough.
- [x] Add and test embedded Nostr bootstrap wrapper.
- [x] Add and test in-process FSP service ports.
- [x] Add and test Dropbox-style port `4242` protocol and Pi4ssd receiver logic.
- [ ] Specify the default peer endpoint observation/punch-back handshake.
- [ ] Configure Pi4ssd FIPS node and record npub/FIPS IPv6.
- [ ] Pick and install a Blossom server on Pi4ssd.
- [ ] Confirm a basic upload/download path to Pi4ssd over FIPS from devbox.
- [ ] In Pushstr, make a small Android-only branch that calls the embedded facade rather than relying on a separate VPN.
