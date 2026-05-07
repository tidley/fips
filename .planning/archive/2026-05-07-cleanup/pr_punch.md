## Introduction

`#37` added NAT traversal using per-peer punch sockets coordinated over Nostr and public STUN.

This branch extends that work so an already-connected FIPS node can help onboard further NATed peers **without those further peers needing their own public STUN**. The design has been tightened away from public helper endpoint advertisement and toward **private, opt-in assist negotiation**.

The current implementation target is:

- Alice uses dedicated STUN and becomes reachable.
- Bob joins via Alice using private assist.
- Colin joins via Bob using private assist.
- Dave joins via Colin using private assist.
- Routed traffic works end-to-end across that chain.

## Product direction

The direction is now explicit:

- **keep** Nostr as the coordination plane
- **keep** per-peer adopted UDP sockets from `#37`
- **remove** public helper endpoint advertisement from overlay adverts
- **use** private NIP-17/Nostr DM negotiation for assist
- **use** short-lived grants and single-use probe tokens
- **make** peer assist opt-in and policy-controlled

This is the right tradeoff. It reduces endpoint leakage and turns helper behavior into explicit, consented coordination instead of passive public exposure.

## Protocol sketch

### 1. AssistRequest

Requester -> helper over private Nostr DM.

Fields:
- `requestId`
- `createdAtMs`
- `ttlMs`
- `senderNpub`
- `targetNpub`
- `nonce`

Purpose:
- ask a specific helper to assist onboarding
- avoid public helper endpoint broadcast

### 2. AssistGrant

Helper -> requester over private Nostr DM.

Fields:
- `requestId`
- `grantId`
- `createdAtMs`
- `ttlMs`
- `senderNpub`
- `targetNpub`
- `nonce`
- `accepted`
- `helperAddr`
- `probeToken`
- `reason`

Purpose:
- explicit consent decision
- short-lived rendezvous grant
- single-use probe token
- private disclosure of helper UDP endpoint

### 3. UDP assist probe

Requester -> helper over UDP.

Fields:
- `grantId`
- `token`

Purpose:
- prove possession of the private grant
- let helper observe requester public `IP:port`
- reuse the helper's existing live UDP socket

### 4. AssistObserved

Helper -> requester over private Nostr DM.

Fields:
- `grantId`
- `createdAtMs`
- `ttlMs`
- `senderNpub`
- `targetNpub`
- `nonce`
- `accepted`
- `observedAddress`
- `reason`

Purpose:
- report the observed requester public endpoint
- confirm the helper saw the authenticated probe on the granted socket
- allow the requester to complete adopted-socket handoff into normal FIPS handshake

## Policy model

### Node-level policy

`node.discovery.nostr.peer_assist` now controls private assist:

- `dial_mode`
  - `disabled`
  - `fallback_private`
  - `prefer_private`
- `grant_ttl_secs`
- `helper.enabled`
- `helper.request_policy`
  - `open_rate_limited`
  - `allowlist`
- `helper.request_allowlist`
- `helper.max_pending_requests`
- `helper.max_requests_per_peer_per_window`
- `helper.request_window_secs`

Traversal offers are rate-limited separately under:

- `node.discovery.nostr.max_offers_per_peer_per_window`
- `node.discovery.nostr.offer_window_secs`

### Transport-level policy

`transports.udp.*.peer_assist` controls whether a specific UDP transport may be used as a helper surface.

This matters because not every UDP listener should become a rendezvous helper.

## Security / privacy model

Compared with the earlier public-helper design, the current model:

- does **not** publish helper public endpoints in overlay adverts
- requires explicit helper consent before endpoint disclosure
- limits helper disclosure to the intended requester
- uses short-lived grants
- uses single-use probe tokens
- supports allowlisting and rate limiting

What remains true:

- helper still learns the requester's observed public endpoint
- relays still see timing / relationship metadata at the DM layer
- helper UDP sockets still gain some abuse surface and need policy/rate limits

This is acceptable as an opt-in feature and materially better than always-public helper advertisement.

## Current implementation status

Implemented:

1. **Protocol messages and state machine**
- `AssistRequest`
- `AssistGrant`
- `AssistObserved`
- single-use probe token flow
- pending grant / observed state tracking in the Nostr runtime

2. **Policy controls**
- node-level private assist config
- request policy: allowlist or open+rate-limited
- grant TTL
- max pending grants
- per-peer per-window rate limiting
- transport-level `udp.peer_assist`

3. **Public helper advert removal**
- removed `helperEndpoints` from overlay adverts
- removed public helper endpoint consumption from advert parsing
- helper endpoints now stay private inside runtime/helper state

4. **Live helper endpoint sources**
- adopted traversal sockets can become helper-capable when their observed public endpoint is known
- base non-public UDP transports can be STUN-observed on the live socket itself when the node is a helper root

5. **Handshake / adoption path**
- requester uses private assist to obtain a helper grant
- requester sends authenticated UDP probe to helper
- helper sends observed address privately back
- requester adopts the socket as `nostr-assist`
- normal FIPS handshake/promotion follows

## Test coverage

### In-process runtime/bootstrap coverage

Passing targeted tests:

```bash
cargo test -q --no-default-features --features nostr-discovery node::tests::bootstrap::
cargo test -q --no-default-features --features nostr-discovery discovery::nostr::tests::
```

These cover:
- private assist request/grant/observed validation
- probe packet build/parse
- adopted socket handoff
- handshake promotion after assist
- helper endpoint privacy regression

### Relay-backed NAT harness

Passing harness:

```bash
./testing/scripts/build.sh
./testing/nat/scripts/nat-test.sh assist
```

This now proves a **4-node chained private-assist topology**:

- Alice is the **only** node using dedicated STUN
- Bob has `stun_servers: []` and joins via Alice
- Colin has `stun_servers: []` and joins via Bob
- Dave has `stun_servers: []` and joins via Colin

Verified outcomes:
- `A <-> B` connected
- `B <-> C` connected
- `C <-> D` connected
- Bob transport includes `nostr-assist`
- Colin transport includes `nostr-assist`
- Dave transport includes `nostr-assist`
- Alice can ping Colin
- Colin can ping Alice
- Alice can ping Dave
- Dave can ping Alice

So the branch now demonstrates:

> one public-STUN root can recursively onboard further NATed peers through existing FIPS peers, using private assist negotiation rather than public helper endpoint advertisement.

## Exact remaining deltas

The core transport/protocol work is now in place. Remaining work is mostly hardening and productisation:

1. improve diagnostics for rejected/expired grants
2. make policy defaults and docs explicit
3. decide whether `prefer_private` should remain or whether fallback-only is the safer default
4. add more negative-path tests:
   - expired grant
   - reused token
   - allowlist rejection
   - helper at capacity
5. decide whether private assist should be separately surfaced in user-facing docs/config examples before proposing upstream

## Commands used for verification

Run from `/home/tom/code/fips`:

```bash
cargo fmt --check
cargo check -q --no-default-features --features nostr-discovery
cargo test -q --no-default-features --features nostr-discovery node::tests::bootstrap::
cargo test -q --no-default-features --features nostr-discovery discovery::nostr::tests::
./testing/scripts/build.sh
./testing/nat/scripts/nat-test.sh assist
```

All of the above pass on the current branch state.
