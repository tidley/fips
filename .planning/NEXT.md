# NEXT

The Android-to-Pi FIPS Drop PoC is physically validated and now moving into a
receiver/product hardening track.

Current implemented junction:

- Android embedded FIPS node starts without VPN/TUN.
- Android connects to the Pi receiver npub through Nostr/STUN traversal.
- FIPS adopts the punched UDP socket and establishes peer/session state.
- Android sends FIPS Drop v0 binary frames over FSP service port `4242`.
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

1. Run the real-world harness regularly during protocol changes:
   `FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh`.
2. Test a VPS/public-IP node with `public: true` and confirm its advert includes
   both direct UDP and `stunServices`.
3. Rebuild and physically retest the post-hardening `fips-drop-agent` and APK.
4. Point the Android native bridge at `crates/fips-mobile`, then
   regenerate/rename Flutter Rust Bridge APIs away from `dropbox` naming when
   the UI bridge is ready for a breaking API cleanup.
5. Add progress reporting from the mobile sender to the Flutter UI.
6. Add receiver authorization/quota policy.
7. Implement the FIPS Drop receipt layer and decide the Nostr/Blossom event
   shape.
