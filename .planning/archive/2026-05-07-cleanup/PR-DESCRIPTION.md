# PR Description Draft — Peer-Assisted NAT Rendezvous

## Summary

This PR adds peer-assisted UDP rendezvous on top of the existing
`nostr-discovery` overlay discovery work. It lets a private node join through a
trusted already-joined helper when direct public STUN traversal is unavailable,
while keeping the resulting connection inside the normal FIPS UDP transport,
Noise session, and peer lifecycle paths.

## Problem

`udp:nat` rendezvous works when both peers can learn and punch usable
reflexive endpoints. That does not cover chained onboarding cases where a new
node can reach a helper through private or already-established topology but
cannot expose a public STUN-reflexive endpoint that a remote peer can use.

## Design

- Adds `peer_assist` dial and helper policy under `node.discovery.nostr`.
- Keeps both sides opt-in: default dial mode is `disabled`, and helper serving
  is disabled.
- Requires helper serving opt-in at both the Nostr helper config and UDP
  transport level with `peer_assist: true`.
- Uses Nostr encrypted signaling for assist request, grant, and observed
  address messages.
- Uses a short grant TTL and one-shot probe token to bind the observed private
  endpoint to the grant.
- Promotes successful assisted handoff into an adopted UDP transport named
  `nostr-assist`.
- Keeps configured-peer dialing static-first, with Nostr discovery and private
  assist as fallback paths unless `prefer_private` is explicitly selected.

## Security Model

- Private assist is disabled by default.
- Helpers default to fail-closed `request_policy: allowlist` with an empty
  allowlist.
- Helpers can explicitly choose `request_policy: open_rate_limited`.
- Open helper mode is per-sender rate limited.
- Pending grants are capped.
- Traversal offers have a separate per-sender rate limit before STUN/punch work.
- Grant, observed, and traversal messages are TTL-bound and sender/recipient
  checked.
- Replay tracking is bounded by TTL and a max-entry cap.

## Operator Configuration

The documented knobs are:

- `dial_mode`: `disabled`, `fallback_private`, `prefer_private`
- `grant_ttl_secs`
- `helper.enabled`
- `helper.request_policy`: `open_rate_limited`, `allowlist`
- `helper.request_allowlist`
- `helper.max_pending_requests`
- `helper.max_requests_per_peer_per_window`
- `helper.request_window_secs`
- `max_offers_per_peer_per_window`
- `offer_window_secs`

The packaged sample config keeps the feature commented out and disabled.

## Validation

- CI now runs tests with `nostr-discovery` enabled.
- NAT lab strategy is documented as manual/nightly-only because it requires
  Docker, network namespaces, veth interfaces, iptables/nftables NAT rules, and
  local relay/STUN services.
- Recommended privileged NAT lab command:

```bash
./testing/nat/scripts/nat-test.sh assist
```

## Known Limitations

- The NAT lab is not in default PR CI because it needs privileged networking.
- Symmetric NAT remains constrained by normal UDP hole-punching limits unless a
  reachable helper path is available.
- Branch history previously contained local key material at an earlier tip; the
  current tree no longer tracks it, but any real key must be treated as rotated
  or compromised before publication.
