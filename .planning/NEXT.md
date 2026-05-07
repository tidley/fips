# NEXT

The Android-to-Pi FIPS Drop PoC is physically validated and now moving into a
receiver/product hardening track.

Current implemented junction:

- Android embedded FIPS node starts without VPN/TUN.
- Android connects to the Pi receiver npub through Nostr/STUN traversal.
- FIPS adopts the punched UDP socket and establishes peer/session state.
- Android sends FIPS Drop v0 binary frames over FSP service port `4242`.
- Regular Pushstr can use the same selected profile identity to send direct
  app messages over FIPS service port `49153`, falling back to Nostr DMs when
  FIPS is unavailable.
- The Pi receiver stores files under `/var/lib/fips-drop`.
- Sparse repair and adaptive sender tuning handle lossy 4G paths.
- Public FIPS UDP nodes can act as same-socket STUN servers and advertise
  `stunServices` through Nostr overlay adverts.
- `crates/fips-mobile` is now the Rust package boundary for Android and future
  iOS app wrappers.

Primary artifacts:

- Receiver binary: `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-drop-agent`
- Legacy receiver alias: `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent`
- Android debug APK: `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`
- Runbook: `docs/pocs/fips-drop-android-pi.md`
- Wire spec: `docs/specs/fips-drop-v0.md`
- Receiver ops: `docs/ops/fips-drop-receiver.md`
- Roadmap: `.planning/FIPS-DROP-NEXT-PHASES.md`
- Real-world harness: `testing/realworld/fips-drop-functional.sh`
- Mobile Rust crate: `crates/fips-mobile`

Next work:

1. Split and commit the current FIPS/mobile-bridge junction:
   - FIPS repo: `crates/fips-mobile` FIPS Drop API aliases, docs, planning.
   - Pushstr repo: bridge dependency on `crates/fips-mobile`, regenerated FRB
     bindings, rebuilt Android native libraries, and direct Pushstr-over-FIPS
     message calls.
2. Run the real-world harness regularly during protocol changes:
   `FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh`.
3. Test a VPS/public-IP node with `public: true` and confirm its advert includes
   both direct UDP and `stunServices`.
4. Rebuild and physically retest the current `fips-drop-agent` and APK from the
   bridge-on-`crates/fips-mobile` baseline.
5. Physically test mobile-to-mobile Pushstr messaging over FIPS/Nostr/STUN and
   decide whether app-level delivery ACKs are needed before widening use.
6. Finish the product-name cleanup:
   - keep the FIPS Drop wire protocol stable,
   - keep old `dropbox` APIs as wrappers until downstream callers are migrated,
   - stop adding new public APIs with `dropbox` names.
7. Add progress reporting from the mobile sender to the Flutter UI.
8. Add receiver authorization/quota policy.
9. Implement the FIPS Drop receipt layer and decide the Nostr/Blossom event
   shape.

Next phases:

1. Baseline and commit hygiene.
   Record artifact hashes, split local FIPS and Pushstr changes into reviewable
   commits, and confirm the APK/binary pair still matches the physical PoC.
2. Real-world regression harness.
   Make `testing/realworld/fips-drop-functional.sh` the routine check for
   transfer-protocol changes, with public relay/STUN opt-in and clear pass/fail
   log markers.
3. Public-node STUN validation.
   Run a VPS node with stable UDP, confirm `stunServices` adverts, then verify a
   mobile/private peer can use the FIPS node itself as STUN input.
4. Mobile UX hardening.
   Add transfer progress, repair/retry state, QR/npub pairing, and clearer
   recoverable error states.
5. Receiver deployment hardening.
   Move systemd/config/storage/logging material toward package install, add
   authorization and quotas, and make manual `sudo RUST_LOG=...` debug-only.
6. Protocol hardening.
   Feed path metrics into adaptive pacing, persist resumable transfer state, and
   retire compatibility names only after tests and downstream callers are ready.
7. Nostr/Blossom receipts.
   Add content-hash receipts and optional Blossom/Nostr metadata publication on
   top of the private FIPS Drop transfer.
