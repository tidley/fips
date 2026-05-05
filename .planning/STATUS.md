# STATUS

Local FIPS focus is the Pushstr/FIPS Dropbox product PoC.

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
- Added in-process FSP service ports and a Dropbox-style protocol on reserved port `4242`.
- Added a Pi4ssd receiver-agent shape as Rust receiver logic:
  - `hello`
  - `put`
  - `put_chunk`
  - `put_done`
  - hash/status acknowledgements.
- Added the runnable `fips-dropbox-agent` binary:
  - loads a normal FIPS config,
  - forces TUN/DNS/control off for embedded operation,
  - registers service port `4242`,
  - writes received blobs under a configured storage root,
  - returns ACK/ERROR replies over FIPS service data.
- Added `fips::mobile` as the first Android-facing Rust facade:
  - embedded node lifecycle,
  - Nostr npub connect request,
  - FSP session readiness checks,
  - Dropbox blob payload generation/send,
  - app service-packet receive/status calls.
- Added Pushstr mobile bridge/UI on branch `feature/fips-dropbox-mobile`:
  - exposes `fips_mobile_*` functions through Pushstr's existing `flutter_rust_bridge` crate,
  - adds drawer entry `FIPS Drop`,
  - starts embedded FIPS from generated YAML,
  - connects to a target npub,
  - picks a local file,
  - sends it to FIPS Dropbox service port `4242`.
- Android native Rust libraries now build with embedded FIPS for:
  - `arm64-v8a`,
  - `armeabi-v7a`.
- Android debug APK builds at:
  - `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`.
- Pi4/Pi3 arm64 receiver binary builds at:
  - `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent`.
- Bluetooth is now a default feature rather than an unconditional Linux dependency, so UDP/Nostr-only receiver builds can cross-compile without a BlueZ/dbus sysroot.

Immediate priority:

1. Run `fips-dropbox-agent` on Pi4ssd with an advert-enabled FIPS config.
2. Install the Pushstr debug APK on Android.
3. Run real Nostr/STUN traversal and one blob upload against Android/Pi4ssd hardware.
4. Add Blossom/Nostr metadata after raw direct FIPS transfer succeeds.

Verification from current junction:

- `cargo build --release --bin fips-dropbox-agent --features nostr-discovery`
- `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --features nostr-discovery --bin fips-dropbox-agent`
- `cargo test mobile::tests --lib --features nostr-discovery`
- `cargo test mobile::tests --lib --no-default-features`
- `cargo test --features nostr-discovery --all-targets`
- `cargo llvm-cov --lib --features nostr-discovery --summary-only -- mobile::tests`
  - `src/mobile.rs`: 98.63% line coverage, 100% function coverage
- Pushstr `cargo check`
- Pushstr `flutter test`
- Pushstr `cargo ndk` Android library builds for `arm64-v8a` and `armeabi-v7a`
- Pushstr `flutter build apk --debug`

Worktree caution:

- Ignored local key/config files exist; keep them out of PoC material and commits.
