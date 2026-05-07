# STATUS

Local FIPS focus is the Pushstr/FIPS Drop product PoC.

Current direction:

- Android app should communicate directly with FIPS nodes through an embedded Rust library.
- Do not depend on a separate Android FIPS/VPN app for the proof of concept.
- Do not route this track through the existing sidecar/NAT/static demo harnesses.
- The first technical milestone is embedded Nostr bootstrap + STUN/UDP hole punch + FIPS traversal adoption with TUN disabled.
- The second technical milestone is an app-service channel over the resulting FIPS session.

Current execution plan:

- `.planning/ANDROID-FIPS-DROPBOX-POC.md` is the active PoC plan.
- `.planning/pushstr-fips-dropbox-plan.md` remains the product background/design file.

Completed Rust slice:

- One active worktree remains for this directive; stale `/tmp` worktrees were removed after confirming they were clean.
- Added an embedded Nostr bootstrap wrapper:
  - `request_nostr_bootstrap(peer_config)`
  - `drain_nostr_bootstrap()`
  - `NostrBootstrapOutcome`
- Proved `request_connect -> BootstrapEvent::Established -> adopt_established_traversal` in-process.
- Proved the adopted traversal path can establish a normal FIPS session with TUN disabled.
- Added in-process FSP service ports and a FIPS Drop protocol on reserved port `4242`.
- Added a Pi4ssd receiver-agent shape as Rust receiver logic:
  - `hello`
  - `put`
  - `put_chunk`
  - `put_done`
  - hash/status acknowledgements,
  - binary blob transfer with sparse missing-chunk repair.
- Added the runnable `fips-drop-agent` receiver binary:
  - loads a normal FIPS config,
  - forces TUN/DNS/control off for embedded operation,
  - registers service port `4242`,
  - writes received blobs under a configured storage root,
  - returns ACK/ERROR replies over FIPS service data.
- Kept `fips-dropbox-agent` as a compatibility alias for previous manual PoC
  commands.
- Added `fips::mobile` as the first Android-facing Rust facade:
  - embedded node lifecycle,
  - Nostr npub connect request,
  - FSP session readiness checks,
  - FIPS Drop blob payload generation/send,
  - app service-packet receive/status calls.
- Added `fips-drop-functional` and `testing/realworld/` as the first
  real-world functional harness:
  - uses public Nostr relays and public STUN,
  - starts embedded receiver and mobile-style sender nodes,
  - sends deterministic files over FIPS Drop service port `4242`,
  - verifies receiver filesystem hashes.
- Added same-socket STUN service support for public FIPS UDP nodes:
  - `node.discovery.nostr.stun_server.mode: auto` enables it for public,
    Nostr-advertised UDP transports,
  - public adverts include `stunServices`,
  - traversal can use the target peer's advertised `stunServices` in addition
    to configured `stun_servers`.
- Added Pushstr mobile bridge/UI on branch `feature/fips-dropbox-mobile`:
  - depends on `../../fips/crates/fips-mobile` rather than the root daemon crate,
  - exposes `fips_mobile_*` functions through Pushstr's existing `flutter_rust_bridge` crate,
  - exposes the product-named `fipsMobileSendFipsDropBlob` binding,
  - keeps `fipsMobileSendDropboxBlob` as a compatibility wrapper,
  - adds drawer entry `FIPS Drop`,
  - starts embedded FIPS from generated YAML,
  - connects to a target npub,
  - picks a local file,
  - sends it to FIPS Drop service port `4242`.
- Added the first regular Pushstr-over-FIPS path:
  - starts the embedded FIPS runtime from the selected Pushstr profile nsec,
  - advertises/listens with the same selected Pushstr npub,
  - sends direct Pushstr messages over FIPS service port `49153`,
  - polls inbound FIPS Pushstr messages and merges them into the existing
    local message/contact store,
  - falls back to the existing Nostr DM path if FIPS session setup or send
    fails.
- Android native Rust libraries now build with embedded FIPS for:
  - `arm64-v8a`,
  - `armeabi-v7a`.
- Android debug APK builds at:
  - `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`.
- Pi4/Pi3 arm64 receiver binary builds at:
  - `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-drop-agent`.
- Bluetooth is now a default feature rather than an unconditional Linux dependency, so UDP/Nostr-only receiver builds can cross-compile without a BlueZ/dbus sysroot.

Physical validation:

1. Phone -> Pi on Wi-Fi works.
2. Phone -> Pi on 4G works.
3. A 3 MiB video stored successfully.
4. The current Android binary blob profile keeps normal service payloads around
   796 bytes under the observed 1280-byte session MTU and adapts window/pacing
   from sparse receiver reports.

Immediate priority:

1. Physically test phone-to-phone Pushstr messaging over FIPS/Nostr/STUN.
2. Split and commit the current FIPS and Pushstr bridge changes cleanly.
3. Use the real-world harness for automated regression during protocol changes.
4. Test the same-socket STUN advert path on a public VPS listener.
5. Rebuild/retest the current `fips-drop-agent` + Android APK baseline.
6. Add progress reporting and pairing UX.
7. Add receiver authorization/quota policy.
8. Add Blossom/Nostr receipt metadata after raw direct FIPS transfer succeeds.
9. Move from PoC receiver docs to package integration.

Verification from current junction:

- `cargo build --release --bin fips-drop-agent --features nostr-discovery`
- `FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh`
- `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --features nostr-discovery --bin fips-drop-agent`
- `cargo test mobile::tests --lib --features nostr-discovery`
- `cargo test mobile::tests --lib --no-default-features`
- `cargo test --features nostr-discovery --all-targets`
- `cargo llvm-cov --lib --features nostr-discovery --summary-only -- mobile::tests`
  - `src/mobile.rs`: 98.63% line coverage, 100% function coverage
- Pushstr `cargo check`
- Pushstr `flutter test`
- Pushstr `cargo ndk` Android library builds for `arm64-v8a` and `armeabi-v7a`
- Pushstr `flutter build apk --debug`
- Pushstr physical phone-to-phone FIPS message test

Worktree caution:

- Ignored local key/config files exist; keep them out of PoC material and commits.
