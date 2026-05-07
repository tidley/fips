# Local Change Summary (PR Context)

This document maps the current local `issue37` work to the review comments and the intended purpose of each change set. The changes are currently represented by local commit `18e3229` on top of `origin/issue37`; this planning file itself remains local/ignored.

## PR-Comment-Driven Follow-Ups

| PR comment / concern | What changed | Main files | Purpose |
|---|---|---|---|
| Avoid over-publishing adverts | Local advert state is now tracked; publish is only triggered when advert content actually changes, while periodic refresh remains in the runtime loop. | `src/bootstrap/nostr/runtime.rs`, `src/node/lifecycle.rs` | Prevent relay spam from generic tick traffic while retaining refresh semantics. |
| Preserve true static-first behavior | Configured peer dialing now explicitly tries static addresses first, then uses Nostr-resolved fallback endpoints only after static attempts fail. | `src/node/lifecycle.rs`, `src/config/peer.rs` | Keep legacy static-peer latency/behavior and make `via_nostr` additive fallback, not eager lookup. |
| NAT-only adverts must remain discoverable | Open discovery candidate selection no longer requires a direct/non-NAT endpoint. `udp:nat`-only adverts remain eligible. | `src/bootstrap/nostr/runtime.rs`, `src/node/lifecycle.rs` | Ensure rendezvous-capable peers are discoverable under `policy: open`. |
| Enforce advert freshness semantically | Advert ingest/fetch now rejects expired or stale adverts using NIP-40 expiration and created-at staleness windows; cache pruning uses semantic validity windows instead of receipt time. | `src/bootstrap/nostr/runtime.rs`, `src/bootstrap/nostr/types.rs` | Prevent stale remote endpoints from lingering/being used after freshness bounds. |
| Scope adverts by app/protocol | Advert ingest/fetch now requires the event `protocol` tag to match `node.discovery.nostr.app`. | `src/bootstrap/nostr/runtime.rs` | Prevent unrelated `fips-overlay-v1` or same-kind relay events from being consumed across namespaces. |
| Never publish wildcard bind addresses | Direct UDP/TCP advert generation now suppresses unspecified bind addresses (`0.0.0.0`, `[::]`). | `src/node/lifecycle.rs` | Prevent publishing non-dialable addresses as peer endpoints. |
| Bound open-discovery enqueueing | Open discovery retry insertion now respects outbound slot availability and an explicit pending cap (`open_discovery_max_pending`). | `src/node/lifecycle.rs`, `src/config/node.rs` | Prevent unbounded retry queue growth under ambient open-discovery traffic. |
| Expire open-discovery retry state | Opportunistic open-discovery retry entries now carry an expiry and are not treated as indefinite reconnects. | `src/node/lifecycle.rs`, `src/node/retry.rs`, `src/node/tests/unit.rs` | Prevent stale ambient discoveries from becoming long-lived retry noise. |

## Purpose-Driven Refactor (Core PR Scope)

| Change area | What changed | Main files | Why it exists |
|---|---|---|---|
| Config model split by capability | Added `peers[].via_nostr`, per-transport `advertise_on_nostr`, UDP `public`, and discovery `policy` (`disabled` / `configured_only` / `open`). | `src/config/peer.rs`, `src/config/transport.rs`, `src/config/node.rs`, `packaging/common/fips.yaml` | Match the intended capability split: configured-peer fallback vs inbound advert vs optional open discovery. |
| Cross-field validation | Added config invariants: Nostr must be enabled when required by transport advertising or `via_nostr`; NAT advert requires relays + STUN. Validation invoked in both `Node::new` and `Node::with_identity`. | `src/config/mod.rs`, `src/node/mod.rs` | Fail fast on invalid operator configurations and avoid constructor-level bypasses. |
| Unified overlay advert format | Replaced NAT-specific advert payloads with scoped endpoint advert model (`fips-overlay-v1`, versioned) including optional NAT metadata. | `src/bootstrap/nostr/types.rs`, `src/bootstrap/nostr/runtime.rs`, `src/bootstrap/nostr/mod.rs` | One advert model for configured-peer resolution and discovery, with explicit schema checks. |
| Advert ingestion/cache semantics | Cache is now keyed by author with replace-by-recency and pruning bounds; advert subscription/filter scope uses `fips-overlay-v1`. | `src/bootstrap/nostr/runtime.rs` | Keep cache bounded and consistent with scoped overlay advert consumption. |
| Startup/teardown correctness | Startup order now initializes transports before Nostr advert publication; shutdown keeps advert withdrawal path. | `src/node/lifecycle.rs`, `src/bootstrap/nostr/runtime.rs` | Avoid advertising endpoints before transports are live; keep cleanup deterministic. |
| Shared dial path across sources | Static, via-nostr fallback, discovery candidates, and `udp:nat` rendezvous all normalize into the same connection attempt path. | `src/node/lifecycle.rs` | Reduce duplicate dial logic and keep transport dispatch behavior consistent. |
| Private-subnet traversal candidates | Offer/answer local addresses now include active private interface candidates, and punching plans LAN/private and STUN-reflexive paths in parallel. | `src/bootstrap/nostr/stun.rs`, `src/bootstrap/nostr/traversal.rs`, `src/bootstrap/nostr/tests.rs` | Support direct same-LAN connections even when STUN/reflexive paths are also available. |

## Scope Consolidation / Cleanup

| Change | Main files | Purpose |
|---|---|---|
| Removed duplicate prototype crate | `examples/nostr-bootstrap/*` (deleted) | Eliminate parallel implementation path now that behavior is in-tree. |
| Updated docs to new model | `README.md`, `docs/design/fips-configuration.md`, `docs/proposals/nostr-udp-hole-punch-protocol.md`, `docs/proposals/issue-37-status-and-closure-plan.md` | Align docs and protocol references with overlay advert model and config semantics. |

## Test and Compatibility Updates

| Change | Main files | Purpose |
|---|---|---|
| Added/updated advert and config validation tests | `src/bootstrap/nostr/tests.rs`, `src/config/mod.rs` | Cover new advert schema and cross-field validation rules. |
| Added local-address and retry-expiry tests | `src/bootstrap/nostr/stun.rs`, `src/node/tests/unit.rs` | Cover private candidate filtering and expiry of open-discovery retry state. |
| Struct-initializer compatibility updates | `src/node/tests/unit.rs`, `src/transport/udp/mod.rs` | Keep tests/build stable after new config fields were introduced. |

## Miscellaneous

| Change | File | Note |
|---|---|---|
| Ignore local planning workspace | `.gitignore` | Adds `.planning/` to ignore list; repo hygiene only, no runtime effect. |
