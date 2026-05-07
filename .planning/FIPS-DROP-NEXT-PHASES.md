# FIPS Drop Next Phases

Status: active roadmap after Android-to-Pi physical validation.

## 1. Freeze The Working PoC

Goal: preserve the known-good junction so future product work can always return
to a reproducible baseline.

Implemented now:

- Reproducible runbook: `docs/pocs/fips-drop-android-pi.md`.
- Current artifact hash block in the runbook.
- Build/copy/install/run commands.
- Expected connection and transfer log markers.
- Known-good Wi-Fi/4G test matrix and known failure cases that drove reliability
  tuning.

## 2. Rename And Productize The Surface

Goal: stop exposing the old Dropbox concept to operators and users.

Implemented now:

- New receiver binary entry point: `fips-drop-agent`.
- New default storage root: `/var/lib/fips-drop`.
- Legacy `fips-dropbox-agent` binary retained as a compatibility alias.
- User-facing receiver logs say `FIPS Drop`.
- Operator docs and packaging use `FIPS Drop`.
- `crates/fips-mobile` exposes FIPS Drop product-name constants and sender
  APIs while keeping old names as compatibility wrappers.
- Pushstr's Android bridge depends on `crates/fips-mobile` and exposes
  `fipsMobileSendFipsDropBlob`; the old `fipsMobileSendDropboxBlob` binding
  remains as a wrapper.

Deferred:

- Full internal Rust type/module rename from `dropbox` to `fips_drop`.
- Removal of old `dropbox` compatibility aliases after downstream callers and
  regression tests are ready.

## 3. Make Transfer Protocol v0 Boring

Goal: make the protocol explicit, testable, and small.

Implemented now:

- Wire spec: `docs/specs/fips-drop-v0.md`.
- Receiver regression tests for sparse repair and out-of-order blob delivery.
- Explicit Android sender profile and adaptive reliability notes in the spec.

## 4. Adaptive Reliability

Goal: replace static pacing with behavior that backs off on loss and grows only
after clean reports.

Implemented now:

- Android sender starts with conservative settings:
  - `768` byte data chunks,
  - `32` chunk window,
  - `8` chunk repair batch,
  - `6 ms` chunk spacing.
- Sparse reports drive adaptation:
  - missing chunks halve the window, shrink repair batches, and increase
    spacing,
  - three clean windows grow cautiously,
  - report timeouts force minimum window/batch and maximum spacing.

Deferred:

- Feed FIPS MMP RTT/loss/goodput directly into the app sender.
- Persist transfer state across Android process death.

## 5. Receiver Packaging

Goal: make manual receiver invocation a debug path, not the normal deployment.

Implemented now:

- Systemd unit: `packaging/systemd/fips-drop.service`.
- Environment template: `packaging/systemd/fips-drop.env.example`.
- tmpfiles snippet: `packaging/systemd/fips-drop.tmpfiles`.
- Ops runbook: `docs/ops/fips-drop-receiver.md`.

Deferred:

- Debian package integration for `fips-drop-agent`.
- Installer integration that chooses receiver-only versus full FIPS daemon.

## 6. Nostr/Blossom Alignment

Goal: keep FIPS Drop as the private pipe and align content identity/receipts
with open Nostr/Blossom patterns.

Implemented now:

- Alignment design note: `docs/design/fips-drop-blossom-alignment.md`.
- Protocol spec reserves the next layer as a receipt containing content hash,
  size, MIME type, receiver npub, optional Blossom URL, and optional Nostr event
  id.

Deferred:

- Actual Blossom object publishing.
- Nostr metadata event kind/schema decision.

## 7. UX Hardening

Goal: hide internal repair details from normal users and make the flow easier to
repeat.

Implemented now:

- Android button states show `Starting` and `Connecting...`.
- File-transfer ack timeout now presents a plain retry-oriented message instead
  of raw missing chunk lists.

Deferred:

- Progress reporting by chunk/window.
- QR/pairing flow for receiver npub.
- Background/resume behavior.

## 8. Baseline And Commit Hygiene

Goal: make the current working junction reviewable and reproducible.

Next:

- Split FIPS repo changes from Pushstr app changes.
- Commit FIPS mobile API/docs/planning changes independently.
- Commit Pushstr bridge/binding/native-library changes independently.
- Record fresh artifact hashes for:
  - Android debug APK,
  - `fips-drop-agent`,
  - legacy `fips-dropbox-agent` alias if still distributed.
- Confirm the physical Wi-Fi/4G matrix still passes after the bridge now targets
  `crates/fips-mobile`.

## 9. Real-World Regression Harness

Goal: make public-relay/public-STUN functional testing routine without putting
non-deterministic internet tests in default CI.

Next:

- Keep `testing/realworld/fips-drop-functional.sh` as the opt-in transfer
  regression.
- Add expected log markers for connect, session established, sparse repair, and
  stored hash.
- Add a documented cadence: run the harness before protocol, reliability, or
  mobile bridge changes are merged.
- Keep deterministic unit/integration tests as the default CI gate.

## 10. Public Node STUN Validation

Goal: prove a public FIPS node can reduce dependency on centralized STUN
services.

Next:

- Run a VPS node with stable public UDP and `public: true`.
- Confirm its Nostr advert contains direct UDP endpoint data and `stunServices`.
- Verify private/mobile peers can use the advertised FIPS STUN service during
  traversal.
- Document privacy tradeoffs and fallback behavior when peer STUN is absent or
  unusable.

## 11. Mobile UX Hardening

Goal: make the PoC usable without reading logs.

Next:

- Surface transfer progress from the Rust sender to Flutter.
- Show repair/retry state as normal progress, not raw chunk lists.
- Add QR/npub pairing for the receiver.
- Separate connection, transfer, repair, and stored states in the UI.

## 12. Receiver Deployment Hardening

Goal: make receiver operation boring on Pi and VPS targets.

Next:

- Move systemd/env/tmpfiles material into package install paths.
- Add receiver authorization, storage quotas, and filename/path policy.
- Add log commands and health checks that match the service name.
- Keep manual `sudo RUST_LOG=... /home/pi4fips/...` as debug mode only.

## 13. Protocol Hardening

Goal: keep FIPS Drop v0 stable while improving reliability.

Next:

- Feed FIPS path metrics into the adaptive sender.
- Persist resumable transfer state across Android process death.
- Add lossy/out-of-order regression coverage for any new pacing or repair
  changes.
- Retire compatibility names only after the stable API surface is covered.

## 14. Nostr/Blossom Receipts

Goal: keep private transfer over FIPS while making the result interoperable.

Next:

- Define the receipt event shape: content hash, size, MIME type, receiver npub,
  optional Blossom URL, optional Nostr event id.
- Add local receipt generation after stored ACK.
- Add optional Blossom publishing without making it required for private FIPS
  transfer.
