## 2026-04-28 11:27 UTC
- Created PR readiness TODO at `.planning/PR-READINESS-TODO.md` and populated NOW/NEXT/BACKLOG for peer-assisted rendezvous PR cleanup.

## 2026-04-28 11:32 UTC
- Audited branch tip for secret material (`fips.key`, `fips.pub`, `fips.yaml`, `git grep nsec`).
- Removed tracked local key/config files from index: `fips.key`, `fips.pub`, `fips.yaml`.
- Restored/expanded `.gitignore` safety for local/private files: `AGENTS.md`, `CLAUDE.md`, `agents/`, plus `fips.key`, `fips.pub`, `fips.yaml`.
- Updated CI unit-test lane to include `nostr-discovery` feature: `cargo nextest run --all --profile ci --features "gateway nostr-discovery"`.
- Recorded key-history exposure risk in `.planning/RISKS.md`; no history rewrite performed.
- Documented NAT harness CI/manual strategy in `testing/nat/README.md` (manual/nightly-only, privileged netns/iptables rationale, explicit `assist` run command).

## 2026-04-28 PR prep continuation
- Documented `node.discovery.nostr.peer_assist.*` in `docs/design/fips-configuration.md` with a safe chained-onboarding example.
- Added commented, disabled-by-default `peer_assist` sample config in `packaging/common/fips.yaml`.
- Added per-sender/window incoming traversal-offer rate limiting before offer worker dispatch.
- Added protocol/rate-limit tests for assist request/grant expiry, grant mismatch, invalid helper endpoint, rejected grant path, and rate-window behavior.
- Added peer-assisted rendezvous changelog entry and `.planning/PR-DESCRIPTION.md` draft.
- Verification passed:
  - `cargo fmt --all -- --check`
  - `cargo clippy --features "gateway nostr-discovery" --all-targets -- -D warnings`
  - `cargo test --features "gateway nostr-discovery"`
  - `./testing/nat/scripts/nat-test.sh assist`

## 2026-05-04 PoC suite refocus
- Confirmed local `/home/tom/code/fips` is now focused on a suite of PoCs.
- Reviewed existing harnesses and planning with Satoshi/Ducky support.
- Added `.planning/POC-SUITE.md` as the suite taxonomy and authoring guide.
- Refreshed `STATUS.md`, `NOW.md`, `NEXT.md`, `BACKLOG.md`, and `DECISIONS.md` for the PoC-suite direction.
- Preserved existing dirty-state caution: deleted `.planning/peer-assisted-core-34e00b9-to-3eaf9ac.diff`, untracked `.planning/pushstr-fips-dropbox-plan.md`, and ignored local key/config files.

## 2026-05-04 Android Dropbox PoC refocus
- Tom clarified that the focus is specifically `.planning/pushstr-fips-dropbox-plan.md` and taking forward a Pushstr-style Android app.
- Key correction: the PoC should embed FIPS communication inside the Android app/library, not depend on a system-wide VPN/gateway path.
- Added `.planning/ANDROID-FIPS-DROPBOX-POC.md` with the next course of action.
- Updated `STATUS.md`, `NOW.md`, `NEXT.md`, and `DECISIONS.md` to prioritize embedded FIPS app-service communication and de-emphasize existing repo demo harnesses for this track.

## 2026-05-04 Nostr bootstrap correction
- Tom clarified the initial Android connection should use the existing Nostr bootstrap/signaling path to establish a UDP hole punch between phone and FIPS node.
- Updated `.planning/ANDROID-FIPS-DROPBOX-POC.md` to put embedded Nostr discovery + STUN/UDP traversal before app-service payload work.
- Updated `STATUS.md`, `NOW.md`, and `DECISIONS.md`: first milestone is library-friendly Nostr bootstrap -> established traversal -> `Node::adopt_established_traversal` -> FIPS session readiness with TUN disabled.

## 2026-05-05 Embedded Dropbox Rust slice
- Consolidated the Pushstr Dropbox work into the main feature branch and removed stale clean worktrees.
- Added embedded Nostr bootstrap methods on `Node`:
  - `request_nostr_bootstrap`
  - `drain_nostr_bootstrap`
  - `NostrBootstrapOutcome`
- Added in-process FSP service ports for app-owned payloads without TUN.
- Added Dropbox-style protocol and receiver logic on service port `4242`.
- Proved the embedded bootstrap/adoption path with TUN disabled and a normal FIPS session over the adopted traversal.
- Proved encrypted in-process service data transfer and Dropbox `put`/`ack` round trip between two FIPS nodes.
- Coverage for the new files:
  - `src/node/embedded.rs`: 100%
  - `src/node/service.rs`: 100%
  - `src/dropbox.rs`: 99.55%
