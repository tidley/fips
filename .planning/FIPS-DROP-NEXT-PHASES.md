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

Deferred:

- Full internal Rust type/module rename from `dropbox` to `fips_drop`.
- Flutter Rust Bridge API rename away from `send_dropbox_blob`; keep source-level
  compatibility until generated bindings are intentionally regenerated.

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
