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
- Added FIPS Drop protocol and receiver logic on service port `4242`.
- Proved the embedded bootstrap/adoption path with TUN disabled and a normal FIPS session over the adopted traversal.
- Proved encrypted in-process service data transfer and Dropbox `put`/`ack` round trip between two FIPS nodes.
- Coverage for the new files:
  - `src/node/embedded.rs`: 100%
  - `src/node/service.rs`: 100%
  - `src/dropbox.rs`: 99.55%

## 2026-05-05 FIPS Drop physical validation
- Built and installed the Android debug APK and Pi arm64 receiver binary.
- Validated Android -> Pi FIPS Drop upload over Wi-Fi.
- Validated Android -> Pi FIPS Drop upload over 4G.
- Confirmed Nostr/STUN traversal, NAT socket adoption, FIPS peer/session establishment, service-port delivery, binary blob sparse repair, and receiver filesystem storage.
- Stored a 3 MiB video at `/var/lib/fips-dropbox/VID-20260505-WA0003.mp4`.
- Added the reproducible PoC runbook at `docs/pocs/fips-drop-android-pi.md`.

## 2026-05-05 FIPS Drop PoC hardening roadmap
- Logged the seven next phases in `.planning/FIPS-DROP-NEXT-PHASES.md`.
- Added product-facing receiver binary entry point `fips-drop-agent`, while
  retaining `fips-dropbox-agent` as a compatibility alias.
- Moved new receiver deployment material to `/var/lib/fips-drop` and
  `packaging/systemd/fips-drop.service`.
- Added FIPS Drop v0 wire/protocol spec at `docs/specs/fips-drop-v0.md`.
- Added receiver ops runbook at `docs/ops/fips-drop-receiver.md`.
- Added Blossom/Nostr alignment note at
  `docs/design/fips-drop-blossom-alignment.md`.
- Added adaptive Android sender tuning based on sparse missing reports.
- Hardened Android file-transfer timeout UX to avoid raw missing chunk dumps.

## 2026-05-05 Real-world functional harness
- Added `fips-drop-functional`, an opt-in harness that starts an embedded FIPS
  Drop receiver and mobile-style sender, uses public Nostr relays/STUN, sends a
  deterministic file over FSP service port `4242`, and verifies the stored hash.
- Added `testing/realworld/fips-drop-functional.sh` with explicit
  `FIPS_REALWORLD=1` opt-in to avoid accidental public relay traffic.
- Documented real-world harness scope, defaults, and non-CI expectations in
  `testing/realworld/README.md` and linked it from `testing/README.md`.

## 2026-05-05 Same-socket FIPS STUN service
- Added a minimal STUN Binding responder to UDP transports, demuxed before FIPS
  packet parsing so public FIPS UDP sockets can also answer STUN requests.
- Added `node.discovery.nostr.stun_server.*` config with `auto` mode, advert
  publication, and per-source-IP rate limiting.
- Extended Nostr overlay adverts with `stunServices` for public UDP endpoints.
- Nostr traversal now uses configured `stun_servers` plus `stunServices` from
  the signed advert of the specific peer being dialed.
- Public UDP transports bound to wildcard addresses can publish the observed
  public endpoint after STUN observation, which covers VPS-style
  `0.0.0.0:2121` listeners.

## 2026-05-05 FIPS mobile crate scaffold
- Added `crates/fips-mobile` as the mobile-oriented Rust package boundary for
  Android and future iOS wrappers.
- Turned the root package into a Cargo workspace containing the existing `fips`
  package plus `fips-mobile`.
- Re-exported the proven embedded mobile client API and added FIPS Drop
  product-name aliases so new bindings do not have to depend on old Dropbox
  terminology.
- Documented Android build/check commands and the iOS target cleanup that still
  needs to happen before claiming iOS support.

## 2026-05-06 Documentation pass
- Added index pages for `docs/ops`, `docs/pocs`, and `docs/specs`.
- Added `docs/design/fips-mobile-library.md` to explain the mobile crate
  boundary, Android build command, iOS caveat, and next binding work.
- Updated the root README and docs indexes so the FIPS Drop PoC, receiver
  runbook, protocol spec, real-world harness, and mobile crate are discoverable
  without reading planning notes.

## 2026-05-06 FIPS Drop coverage pass
- Added deterministic unit coverage for the FIPS Drop receiver CLI, storage-root
  compatibility helper, host-service disabling, and service-packet reply path.
- Added deterministic unit coverage for the real-world functional harness CLI,
  payload generation, mobile-safe Nostr/STUN config, and JSON summary output.
- Verified with
  `cargo llvm-cov --workspace --no-default-features --features nostr-discovery --summary-only`:
  1241 library tests plus bin tests passed, 4 ignored, 0 failed; line coverage
  was 64.05% overall.

## 2026-05-06 Android bridge switched to fips-mobile
- Added product-named FIPS Drop aliases on the mobile client facade, including
  `send_fips_drop_blob_to_npub` and `build_fips_drop_put_message`.
- Kept the older `dropbox` method/function names as compatibility wrappers so
  the working FIPS Drop v0 protocol and existing callers remain stable.
- Pointed the Pushstr Android Rust bridge at `crates/fips-mobile` instead of the
  root `fips` daemon package.
- Regenerated Flutter Rust Bridge bindings and updated the Dart FIPS Drop UI to
  call `fipsMobileSendFipsDropBlob`, leaving `fipsMobileSendDropboxBlob` as a
  generated compatibility alias.

## 2026-05-06 Planning advanced after bridge switch
- Updated `.planning/NEXT.md`, `.planning/STATUS.md`, and
  `.planning/FIPS-DROP-NEXT-PHASES.md` so the Android bridge switch is recorded
  as done rather than future work.
- Expanded the next execution phases around commit hygiene, real-world
  regression, public-node STUN validation, mobile UX hardening, receiver
  deployment, protocol hardening, and Nostr/Blossom receipts.
- Refreshed risk notes to distinguish solved PoC blockers from remaining
  product risks.

## 2026-05-07 Pushstr over FIPS path
- Investigated the regular Pushstr "no npub available" report and confirmed the
  current Android app derives the selected Pushstr profile npub from the active
  nsec, then passes that identity into the embedded FIPS mobile runtime.
- Added a Pushstr app-service channel on FIPS service port `49153` for direct
  mobile-to-mobile messages over Nostr/STUN-discovered FIPS sessions.
- Added Pushstr bridge calls for sending and polling FIPS-delivered Pushstr
  messages, regenerated Flutter Rust Bridge bindings, and wired the regular
  Pushstr send path to try FIPS first before falling back to Nostr DMs.
- Confirmed public, Nostr-advertised FIPS UDP nodes already have same-socket
  STUN service config and signed `stunServices` adverts for open-port nodes.
- Archived stale scratch planning files under
  `.planning/archive/2026-05-07-cleanup/`.
