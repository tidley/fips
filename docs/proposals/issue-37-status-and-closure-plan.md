# Issue #37 PR Explainer

Issue reference: [#37 UDP NAT traversal via Nostr relay signaling](https://github.com/jmcorgan/fips/issues/37)

## What This Change Adds

Issue `#37` is now implemented as an in-tree bootstrap layer that can:

- publish Nostr service adverts
- exchange encrypted Nostr offer/answer signaling
- perform STUN-based reflexive address discovery
- run UDP hole punching with probe/ack traffic
- hand the established UDP socket into the normal FIPS transport stack
- retry or fall back through the normal node connection flow when traversal fails

This is not a parallel transport stack. The bootstrap layer exists only to get
two peers to a live UDP socket and confirmed remote endpoint; after that, the
existing UDP transport and normal Noise/FMP/FSP machinery take over.
The in-tree bootstrap is compiled behind the cargo feature
`nostr-discovery`.

## Main Pieces

### Bootstrap handoff boundary

- [`src/discovery.rs`](../../src/discovery.rs)
  - defines `EstablishedTraversal` and `BootstrapHandoffResult`
  - records the key invariant that the adopted UDP socket must be the same
    socket used during STUN and punching

### Core bootstrap runtime

The original monolithic `src/bootstrap/nostr.rs` has been split into focused
files:

- [`src/discovery/nostr/runtime.rs`](../../src/discovery/nostr/runtime.rs)
  - runtime state, relay subscriptions, advert publication, connect flow,
    incoming-offer handling, and shutdown
- [`src/discovery/nostr/signal.rs`](../../src/discovery/nostr/signal.rs)
  - gift-wrap construction/unwrapping and offer/answer validation
- [`src/discovery/nostr/stun.rs`](../../src/discovery/nostr/stun.rs)
  - STUN parsing and reflexive address observation
- [`src/discovery/nostr/traversal.rs`](../../src/discovery/nostr/traversal.rs)
  - punch planning, punch packet formats, and punch execution
- [`src/discovery/nostr/types.rs`](../../src/discovery/nostr/types.rs)
  - shared types, constants, and errors
- [`src/discovery/nostr/tests.rs`](../../src/discovery/nostr/tests.rs)
  - unit coverage for protocol helpers

### UDP transport socket adoption

- [`src/transport/udp/mod.rs`](../../src/transport/udp/mod.rs)
- [`src/transport/udp/socket.rs`](../../src/transport/udp/socket.rs)

These changes let the normal UDP transport adopt an already-bound socket so the
NAT mapping created during traversal is preserved.

### Node integration

- [`src/node/lifecycle.rs`](../../src/node/lifecycle.rs)
  - starts the bootstrap runtime when configured
  - routes `udp:nat` peers into bootstrap
  - adopts successful traversal sockets
  - rolls back adopted transports if the follow-on FIPS handshake setup fails
  - shuts bootstrap down and withdraws adverts on node stop

### Config and operator surface

- [`src/config/node.rs`](../../src/config/node.rs)
  - adds `node.discovery.nostr.*`
- [`packaging/common/fips.yaml`](../../packaging/common/fips.yaml)
  - example/default config entries
- [`docs/design/fips-configuration.md`](../design/fips-configuration.md)
  - documents the config surface and defaults

### NAT lab and CI coverage

- [`testing/nat/`](../../testing/nat/)
  - Docker NAT lab with cone, symmetric, and LAN scenarios
- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)
  - CI coverage for the NAT scenarios

## Important Design Decisions

### Per-peer, per-attempt punch sockets

The implementation uses:

- one fresh UDP socket per traversal attempt
- one remote peer per such socket
- the same socket for STUN, punch traffic, and post-success UDP transport adoption

This is the socket lifecycle recorded in
[`docs/proposals/nostr-udp-hole-punch-protocol.md`](./nostr-udp-hole-punch-protocol.md).

### Multi-port, not one-port-multiplexed

This is a **multi-port** traversal design.

The long-lived application UDP listener is not reused as the punch socket.
Instead, traversal allocates ephemeral sockets per attempt and adopts the
winning socket into FIPS after success.

That choice keeps NAT mappings isolated per peer/attempt and avoids coupling
the main listener to traversal-specific state.

### Bootstrap hands off to normal FIPS transport

The traversal layer stops at socket establishment. It does not try to duplicate
transport, handshake, or session logic. After success, the adopted socket is
promoted into the normal UDP transport and the standard FIPS stack proceeds as
usual.

### Multiple STUN defaults with full override

The main `node.discovery.nostr.stun_servers` defaults are now:

- `stun:stun.l.google.com:19302`
- `stun:global.stun.twilio.com:3478`
- `stun:openrelay.metered.ca:80`

Maintainer note:

- `wss://strfry.bitsbytom.com` is run by a contributor, not by the project

The list is fully configurable. If `stun_servers` is set in YAML, that list
replaces the built-in defaults entirely.
Initiators use only their locally configured list for outbound STUN queries;
peer adverts can report STUN choices for diagnostics, but they are not treated
as arbitrary egress targets.
The current in-tree STUN parser now handles IPv4 and IPv6 mapped-address
attributes. Local traversal candidates include active non-loopback private
interface addresses (RFC1918 IPv4 and IPv6 ULA), and punch planning attempts
private-subnet and reflexive paths in parallel when both are available.

### Gift-wrap sender identity is now bound to payload identity

The in-tree bootstrap now validates that:

- `offer.senderNpub` matches the actual Nostr pubkey that sent the gift wrap
- `offer.recipientNpub` matches the local node
- `answer.senderNpub` matches the actual Nostr pubkey that sent the answer
- `answer.recipientNpub` matches the original initiator

That closes the gap where payload identity fields could be spoofed independently
of the Nostr sender key that delivered the signal.

### Relay lookup is resilient

Inbox-relay discovery now:

- queries across the configured DM and advert relay sets
- falls back to the local DM relay list if remote inbox-relay metadata cannot be fetched

That keeps bootstrap attempts from failing purely because one relay set is
transiently unavailable.

## Evidence That The Core Scope Landed

| Area | Status | Main evidence |
|---|---|---|
| Protocol draft | Implemented and updated | [`nostr-udp-hole-punch-protocol.md`](./nostr-udp-hole-punch-protocol.md) |
| STUN discovery | Implemented | [`src/discovery/nostr/stun.rs`](../../src/discovery/nostr/stun.rs) |
| Offer/answer signaling | Implemented | [`src/discovery/nostr/signal.rs`](../../src/discovery/nostr/signal.rs), [`src/discovery/nostr/runtime.rs`](../../src/discovery/nostr/runtime.rs) |
| Punch/ack exchange | Implemented | [`src/discovery/nostr/traversal.rs`](../../src/discovery/nostr/traversal.rs) |
| Handoff into UDP transport | Implemented | [`src/discovery.rs`](../../src/discovery.rs), [`src/transport/udp/mod.rs`](../../src/transport/udp/mod.rs), [`src/node/lifecycle.rs`](../../src/node/lifecycle.rs) |
| `udp:nat` integration | Implemented | [`src/node/lifecycle.rs`](../../src/node/lifecycle.rs) |
| Cleanup on shutdown | Implemented | [`src/discovery/nostr/runtime.rs`](../../src/discovery/nostr/runtime.rs), [`src/node/lifecycle.rs`](../../src/node/lifecycle.rs) |
| NAT-lab integration tests | Implemented | [`testing/nat/scripts/nat-test.sh`](../../testing/nat/scripts/nat-test.sh) |
| CI coverage | Implemented | [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) |

## Maintainer Attention Points

These are the main non-mechanical review points that still matter:

### STUN default policy

The code is configurable, but maintainers may still want to decide whether the
shipped defaults should include a contributor-operated STUN server or whether
the project should prefer only project-operated or clearly third-party defaults.

### Example crate removal

The standalone prototype crate at `examples/nostr-discovery` has been removed
from this PR. It duplicated protocol/runtime logic that now lives in the main
crate (`src/discovery/nostr/*`) and had no remaining runnable demo binaries.
Keeping a second implementation path increased review and maintenance overhead
without adding shipped functionality.

### Relationship to issue #34

Issue `#37` touched discovery/traversal metadata and overlaps conceptually with
issue `#34`. The code is in place, but the project-level decision on combined
vs separate discovery metadata is still worth recording explicitly.

### Symmetric NAT port prediction

The current implementation handles success cases, LAN preference, and
symmetric-NAT failure with fallback. It does not implement one-sided symmetric
NAT port prediction. If that remains important, it should be a follow-up rather
than an implied expectation hidden inside `#37`.

### Cone NAT acceptance wording

The NAT lab does not rely on loose “plain MASQUERADE means cone NAT” wording.
The working harness uses explicit full-cone-style emulation in the router
namespace. That is the model reviewers should compare against when checking the
tests and acceptance story.

## Bottom Line

The traversal core for issue `#37` is in place:

- Nostr signaling
- STUN observation
- UDP punching
- transport handoff
- node integration
- NAT lab
- CI coverage

What remains is mostly maintainer policy and documentation finality, not a
missing traversal implementation.
