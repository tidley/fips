
Thanks Tom, this was a substantial rework and I appreciate you taking
it on. I've done a full pass on the rework commits (`75e5c3f` through
`cecd761`) and everything structural checks out. The generalized
`fips-overlay-v1` advert schema with per-transport `{transport, addr}`
endpoints, per-transport `advertise_on_nostr` flags, startup order
(transports before Nostr), config validation, dynamic advert building,
layering (zero nostr imports in transport modules), and the
`via_nostr` dial path with static-first priority all verified
end-to-end. The `examples/nostr-bootstrap/` sub-crate is gone. PR
closes #37 and rolls in #34 as intended.

A handful of smaller items remain before I can merge. Master has also
advanced since your last rebase and picked up a couple of changes that
touch the same code paths, so a rebase is needed anyway. Details
below.

## Change Requests

### 1. STUN defaults: remove your personal host (HIGH)

`default_stun_servers()` currently includes `fips.tomdwyer.uk:3478`.
Could you drop it from the upstream defaults and keep it in your own
config? I don't want to ship a personal host as a default that every
downstream user will hit.

### 2. Naming: reflect the broader scope this feature has grown into

The PR started life as a UDP NAT bootstrap and has since grown into a
general Nostr-mediated peer discovery mechanism, with UDP hole
punching as one capability among several (public UDP, TCP, Tor
adverts, `via_nostr` fallback dialing). The config namespace already
reflects that with `node.discovery.nostr`, but the code-level naming
still reads like the original UDP-bootstrap scope: `nostr-bootstrap`
feature, `src/bootstrap/` module, `NostrBootstrap` /
`NostrBootstrapConfig`. I'd like to rename to match the feature's
current shape:

- Feature: `nostr-bootstrap` → `nostr-discovery`
- Module: `src/bootstrap/` → `src/discovery/`
- Structs: `NostrBootstrap` → `NostrDiscovery`,
  `NostrBootstrapConfig` → `NostrDiscoveryConfig`

### 3. Offer/answer schema cleanup

`TraversalOffer` / `TraversalAnswer` still carry `app` and `eventKind`
fields from the pre-rework schema. They're inconsistent with the
cleaned-up `OverlayAdvert` (which derives envelope metadata from the
Nostr event). Could you remove them?

### 4. `seen_sessions` pruning

`advert_cache` is properly bounded (TTL expiry + 2048-entry cap).
`seen_sessions` is pruned only on access, so it grows unbounded for a
node that receives many offers but accepts few. Could you add either
periodic cleanup (tick-driven) or a max-size cap matching
`advert_cache`? Either works for me.

### 5. `AccessDenied` propagation during the rebase (NEW)

This one wasn't visible at review time. It comes from the peer-ACL
merge (`745b523`, PR #50) that landed after your last rebase.
Master's `initiate_peer_connection` bails out immediately when
`initiate_connection` returns `NodeError::AccessDenied`, because the
ACL is keyed per-npub and continuing to try other addresses is wasted
effort (and generates extra rejected handshakes that the responder
has to reject again at msg3). Your refactored
`attempt_peer_address_list` logs-and-continues on every `Err`, which
would swallow `AccessDenied` after the rebase and try every
configured address in turn.

While resolving the conflict in `src/node/lifecycle.rs`, could you add
an early-return arm to the inner match on `initiate_connection()`'s
result?

```rust
Err(e @ NodeError::AccessDenied(_)) => return Err(e),
```

Small and mechanical, just want to flag it so it doesn't get lost
during conflict resolution.

## Rebase Summary

Master is at `be0708a` (10 commits ahead of your base `83b20b3`). I
did a test merge locally on my end; it surfaces four conflicts:

- **`src/node/lifecycle.rs`**: imports (additive, trivial) and one
  real semantic conflict in `initiate_peer_connection`. Your
  refactored helper is the right direction structurally. Keep your
  structure and fold in the `AccessDenied` propagation above.
- **`src/node/handlers/rx_loop.rs`**: master added a
  `self.reload_peer_acl()` call and inlined the epoch-millis
  computation; you extracted `Self::now_ms()`. Keep your helper and
  add master's `reload_peer_acl()` call alongside it.
- **`src/node/tests/mod.rs`**: trivial. You added `mod bootstrap;`,
  master added `mod bloom_poison;`. Keep both.
- **`Cargo.lock`**: regenerate.

Non-conflicting but worth knowing about:

- `src/node/handlers/handshake.rs` now runs the peer-ACL check at
  inbound handshake entry (PR #50). Your outbound path feeds this.
- `src/node/bloom.rs` now validates FilterAnnounce fill ratio on
  ingress. Discovery-synthesized peers that cross a saturated-filter
  peer will have those announces rejected.
- `src/protocol/tree.rs` tightened TreeAnnounce ancestry validation.
- `src/upper/dns.rs` added an arrival-interface filter on the
  in-process DNS responder. Doesn't interact with the bootstrap
  path, just a heads-up.

If you'd prefer to rebase onto master rather than merge, that keeps
the history linear. Your one existing merge commit (`4ee07fb`) was
the last one, so a rebase now would be a one-time cleanup. Totally up
to you though.

## Nice-to-have Follow-ups (not blocking)

- `PeerConfig.addresses` `#[serde(default)]` so `via_nostr`-only peers
  can omit the field entirely in YAML rather than `addresses: []`.
- Cross-field validation: `punch_start_delay_ms` <
  `attempt_timeout_secs * 1000`. Defaults are safe; user-provided
  values are unchecked.
- SatsAndSports's ephemeral-pubkey suggestion for the offer/answer
  relay flow. Nice privacy improvement, fine as a follow-up PR.


My only pointers:

max-size cap matching `advert_cache` so it is can't be swamped

rebase onto master rather than merge


