# fips-nostr-bootstrap

Experimental Nostr-signaled UDP traversal helpers and test support, vendored
into the `fips` repo for issue 37 work.

This subcrate now contains:
- Nostr advert / offer / answer protocol types and helpers
- STUN URL parsing and local reflexive-address observation helpers
- binary UDP punch / ack packet helpers
- punch target planning and timing helpers
- client/server rendezvous runtime helpers
- protocol-level tests used by the local NAT harness

It does not currently ship runnable post-handoff demo binaries. The earlier
console/video examples depended on an in-tree app-port API that has been
removed from this PR and should be reviewed separately.

## Files

- [src/lib.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/lib.rs)
  - exports the helper/runtime modules used by the protocol tests and NAT lab
- [src/client_runtime/](/home/tom/code/fips/examples/nostr-bootstrap/src/client_runtime)
  - initiator-side rendezvous helpers
- [src/server_runtime/](/home/tom/code/fips/examples/nostr-bootstrap/src/server_runtime)
  - responder-side rendezvous helpers
- [src/tests.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/tests.rs)
  - protocol and planning tests

## Build

```bash
cd /home/tom/code/fips
cargo test --manifest-path examples/nostr-bootstrap/Cargo.toml
```

## Notes

- The helper crate still carries built-in relay and STUN defaults for local
  testing, but operators are expected to override infrastructure choices in
  real deployments.
- Built-in defaults include contributor-operated infrastructure and should be
  treated as best-effort:
  - advert relay: `wss://strfry.bitsbytom.com`
  - STUN server: `stun:fips.tomdwyer.uk:3478`
- STUN probing uses only the local configured server list. Peer-advertised
  STUN values are informational and are not treated as arbitrary egress
  targets.
- The current in-tree STUN parser handles IPv4 and IPv6 mapped-address
  responses, but local interface discovery is still best-effort.
- Traversal is intentionally multi-port: each attempt binds a fresh UDP socket,
  uses that socket for STUN and punching, and hands that exact socket into
  FIPS after success.

## Intended PR Scope

This subcrate supports the issue-37 bootstrap layer:
- Nostr-based discovery and signaling
- STUN-driven reflexive address discovery
- UDP hole punching
- NAT-lab protocol testing

The in-tree Nostr bootstrap in `src/bootstrap/nostr/` is the merge target for
the actual `fips` runtime behavior.
