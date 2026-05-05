# NEXT

Physical test junction is ready.

Artifacts:

- Pi receiver binary for Pi4/Pi3 arm64: `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent`
- Local x86_64 receiver binary: `/home/tom/code/fips/target/release/fips-dropbox-agent`
- Android debug APK: `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`
- Android native Rust libs rebuilt:
  - `/home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/arm64-v8a/libpushstr_rust.so`
  - `/home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/armeabi-v7a/libpushstr_rust.so`

Next physical test:

1. Put `fips-dropbox-agent` on Pi4ssd and run it against a FIPS config that advertises a Nostr `udp:nat` endpoint.
2. Install the Pushstr debug APK on Android.
3. Open drawer -> `FIPS Drop`.
4. Paste Pi4ssd npub.
5. Start embedded FIPS.
6. Connect to Pi4ssd.
7. Pick one small file and send it.
8. Confirm Pi4ssd writes the blob under the configured storage root and returns ACK/ERROR over FIPS service data.

Pi receiver build note:

- The arm64 receiver was built with `--no-default-features --features nostr-discovery`.
- Bluetooth remains part of default FIPS builds, but this PoC binary is UDP/Nostr-only so it cross-compiles without a BlueZ/dbus sysroot.

After the raw direct FIPS transfer succeeds:

- add progress/chunk UI,
- add Blossom/Nostr metadata,
- decide whether the receiver should become a permanent Pi4ssd service.
