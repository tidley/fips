# FIPS Drop Android-to-Pi PoC

This PoC proves a direct mobile-to-homebase file drop:

```text
Android FIPS Drop UI
  -> embedded app-owned FIPS node
  -> Nostr relays for bootstrap/signalling
  -> STUN-assisted UDP hole punch
  -> adopted FIPS peer/session
  -> FSP service port 4242
  -> Pi receiver writes the file under /var/lib/fips-drop
```

It has been physically validated with phone-to-Pi transfers on both Wi-Fi and
4G. The Android app does not use Android `VpnService`, a system-wide TUN, or a
separate FIPS daemon.

## Scope

This is a FIPS Drop protocol-v0 proof, not a final product protocol. The
service uses compact binary file-transfer frames over FIPS service packets,
with sparse repair after each transfer window and after the sender's completion
marker. The receiver still accepts the earlier CoAP/JSON messages for
compatibility, but Android uses the binary blob path by default.

Blossom/Nostr alignment is intentionally next-phase work: this PoC stores the
received file on the Pi filesystem and returns a stored acknowledgement with
hash and size.

## Artifacts

FIPS repo:

- Mobile Rust crate:
  `crates/fips-mobile`
- Pi arm64 receiver binary:
  `target/aarch64-unknown-linux-gnu/release/fips-drop-agent`
- Legacy compatibility receiver binary:
  `target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent`
- Receiver binary source:
  `src/bin/fips-drop-agent.rs`
- Protocol/receiver implementation:
  `src/dropbox.rs`
- Mobile facade:
  `src/mobile.rs`

Pushstr repo:

- Android APK:
  `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`
- Android native libraries:
  `/home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/arm64-v8a/libpushstr_rust.so`
  `/home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/armeabi-v7a/libpushstr_rust.so`
- UI:
  `/home/tom/code/pushstr/mobile/lib/main.dart`
- Flutter/Rust bridge functions:
  `/home/tom/code/pushstr/pushstr_rust/src/api.rs`

Latest local build hashes from 2026-05-05:

```text
81a0c97e3187905b18e3408cdd3f3b6bd2a4a3327244df9bb3fd268053224dfb  target/aarch64-unknown-linux-gnu/release/fips-drop-agent
a8e7bc9ba2790bb6d654a4b2aab932774c82157d91f098ab4f268c20a7375c86  target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent
ba9c56ffd8522cbb2cda19f180767027c3db9e56ab1079b15d8eedd281a67db7  /home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk
dbb4954e86bb4f6873b30b1cb51b7004a615f19130744fed1c2e3876c7622d74  /home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/arm64-v8a/libpushstr_rust.so
f78da1736299f56e0d700a80537d9304585021ae6d5a33a73c1d1299ba919bd5  /home/tom/code/pushstr/mobile/android/app/src/main/jniLibs/armeabi-v7a/libpushstr_rust.so
```

## Build

Build the Pi arm64 receiver:

```bash
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --release \
  --target aarch64-unknown-linux-gnu \
  --bin fips-drop-agent \
  --no-default-features \
  --features nostr-discovery
```

Build Android native libraries from the Pushstr Rust bridge:

```bash
cd /home/tom/code/pushstr/pushstr_rust
cargo ndk -t arm64-v8a -t armeabi-v7a \
  -o ../mobile/android/app/src/main/jniLibs \
  build --release
```

Build the Android debug APK:

```bash
cd /home/tom/code/pushstr/mobile
flutter build apk --debug
```

The FIPS-side mobile crate can also be checked directly:

```bash
cargo check -p fips-mobile --features nostr-discovery
cargo ndk -t arm64-v8a check \
  -p fips-mobile \
  --no-default-features \
  --features nostr-discovery
```

## Pi Receiver

Use one receiver process for clean tests. If the normal `fips.service` is using
the same identity/config, stop it before manual PoC runs:

```bash
sudo systemctl stop fips
```

Copy the receiver binary:

```bash
scp target/aarch64-unknown-linux-gnu/release/fips-drop-agent \
  pi4fips@192.168.1.147:/home/pi4fips/fips-drop-agent
```

Create the storage directory. For a development PoC where files are pulled back
with `scp`, keep the directory traversable by the SSH user:

```bash
sudo install -d -m 0755 /var/lib/fips-drop
```

Run the receiver:

```bash
sudo RUST_LOG="info,fips::discovery::nostr=debug" \
  /home/pi4fips/fips-drop-agent \
  --config /etc/fips/fips.yaml \
  --storage-root /var/lib/fips-drop \
  --port 4242
```

For deeper diagnosis:

```bash
sudo RUST_LOG="debug,fips::discovery::nostr=trace" \
  /home/pi4fips/fips-drop-agent \
  --config /etc/fips/fips.yaml \
  --storage-root /var/lib/fips-drop \
  --port 4242
```

Keep the receiver `npub` from the startup log.

The legacy `fips-dropbox-agent` binary is still built as an alias for existing
manual tests. New PoC material should use `fips-drop-agent` and
`/var/lib/fips-drop`.

## Android

Install the APK:

```bash
adb install -r /home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk
```

Run the flow:

1. Open Pushstr.
2. Open the drawer.
3. Open `FIPS Drop`.
4. Paste the Pi receiver `npub`.
5. Tap `Start`.
6. Tap `Connect` and wait for `Connected`.
7. Pick a file.
8. Tap `Send`.

The current Android PoC caps files at 10 MiB.

## Expected Result

The Pi logs a stored message similar to:

```text
FIPS Drop binary blob stored id=... size=... path=... stored_path=/var/lib/fips-drop/...
```

The file appears on the Pi:

```bash
sudo find /var/lib/fips-drop -maxdepth 1 -type f -ls
```

Pull it back from another machine:

```bash
scp 'pi4fips@192.168.1.147:/var/lib/fips-drop/VID-20260505-WA0003.mp4' /tmp/
```

If the directory is intentionally locked down, stage the file first:

```bash
sudo cp /var/lib/fips-drop/VID-20260505-WA0003.mp4 /home/pi4fips/
sudo chown pi4fips:pi4fips /home/pi4fips/VID-20260505-WA0003.mp4
```

## Expected Log Markers

Connection setup:

```text
traversal: responder punch succeeded
Adopted NAT traversal socket
Connection promoted to active peer
Peer promoted to active
Session established
```

File transfer:

```text
FIPS Drop service packet received
FIPS Drop binary blob chunk received
FIPS Drop reply queued for FIPS send
FIPS Drop binary blob stored
```

Non-fatal cleanup commonly seen after successful connection:

```text
Stale handshake connection timed out
bootstrap transport has no remaining references; dropping
```

Those stale-handshake lines are acceptable when a duplicate bootstrap path lost
the race to the adopted/active path.

## Known-Good Test Matrix

| Date | Path | Payload | Result |
|------|------|---------|--------|
| 2026-05-05 | Android on Wi-Fi -> Pi on LAN | 3 MiB MP4 | Stored |
| 2026-05-05 | Android on 4G -> Pi on LAN | 3 MiB MP4 | Stored |
| 2026-05-05 | Android on 4G -> Pi on LAN | 216 KiB JPG | Reproduced missing-tail failure before conservative/adaptive tuning |
| 2026-05-05 | Android on 4G -> Pi on LAN | 351 KiB PNG | Reproduced missing-window repair failure before conservative/adaptive tuning |

Current post-validation build starts with conservative/adaptive binary transfer
tuning: `768` byte data chunks, `32` chunk initial windows, `8` chunk initial
repair batches, `6 ms` initial chunk pacing, and dynamic backoff/growth based on
receiver sparse reports.

## Protocol Notes

- Service port: `4242`.
- Sender reply port: `49152`.
- Binary magic: `FDB1`.
- Default Android file data per chunk: `768` bytes.
- Initial binary transfer window: `32` chunks, adaptive range `8..64`.
- Initial repair batch size: `8` chunks, adaptive range `4..16`.
- Initial sender pacing: `6 ms` between chunk sends, adaptive range `3..20 ms`,
  plus a short pause between windows.
- Normal observed Android service payload length: about `796` bytes.
- Current observed session MTU on phone-to-Pi tests: `1280`.
- Reliability model: no per-chunk ACK; sender sends start, sends each chunk
  window, asks for a sparse report, repairs missing chunks within the sent
  prefix, adapts window/pacing to observed loss, then repeats until the final
  stored acknowledgement.

The `768` byte Android chunk size is deliberately conservative for the first
physical PoC. It leaves headroom under the 1280-byte path and avoids relying on
IP fragmentation on mobile networks.

## Known Limits

- Manual target `npub` entry.
- Manual receiver deployment.
- Public Nostr relays are still used for signalling.
- No Blossom object API or Nostr manifest event yet.
- No background upload persistence across Android process death yet.
- No automatic receiver-side authorization policy beyond FIPS identity/session
  establishment.
