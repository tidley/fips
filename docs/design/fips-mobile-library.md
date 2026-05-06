# FIPS Mobile Library

## Purpose

`crates/fips-mobile` is the Rust package boundary for mobile applications that
embed FIPS directly. It exists so Android and later iOS wrappers can depend on
a small app-facing crate instead of binding to the root daemon package.

The current implementation deliberately reuses the proven embedded client in
`src/mobile.rs`. That keeps the Android-to-Pi FIPS Drop PoC stable while giving
future JNI, Flutter Rust Bridge, or UniFFI bindings a cleaner target.

## Current Shape

The crate builds:

- `rlib` for Rust integration and tests,
- `staticlib` for static platform linking,
- `cdylib` for Android shared-library loading.

It re-exports:

- `FipsMobileConfig`,
- `FipsMobileClient`,
- `FipsMobileError`,
- `ServicePacket`,
- core identity/config helpers,
- FIPS Drop constants and message helpers.

The app-facing flow is:

```text
load config
start embedded node
connect peer npub over Nostr/STUN traversal
wait for FSP service session
send FIPS Drop blob to service port 4242
receive status or service replies
stop embedded node
```

The mobile config path disables TUN, DNS, and the control socket by default.
Mobile applications should use FSP service ports, not a system-wide VPN path,
for this PoC line.

## Android Build

Use `cargo-ndk` so C dependencies such as `ring` see the Android NDK compiler:

```bash
cargo ndk -t arm64-v8a check \
  -p fips-mobile \
  --no-default-features \
  --features nostr-discovery

cargo ndk -t arm64-v8a build \
  -p fips-mobile \
  --release \
  --no-default-features \
  --features nostr-discovery
```

The built shared library appears under:

```text
target/aarch64-linux-android/release/libfips_mobile.so
```

## iOS Direction

The crate is intended to become the iOS boundary too, but iOS should not be
advertised as supported until the root crate's host networking dependencies are
fully feature-gated for mobile builds. In particular, TUN/gateway code should be
kept out of pure app-service builds.

## Naming

The working PoC still has internal `dropbox` symbols for source compatibility.
New APIs should use FIPS Drop naming. The mobile crate already provides
product-name aliases for the protocol constants and message helper.

## Next Binding Work

1. Point the Android native bridge at `crates/fips-mobile`.
2. Regenerate the Flutter/Rust bridge against FIPS Drop names.
3. Add transfer progress callbacks or an event stream.
4. Add resume/repair status events that do not expose raw missing-chunk lists to
   the UI.
5. Split host-only dependencies behind features before adding iOS CI.
