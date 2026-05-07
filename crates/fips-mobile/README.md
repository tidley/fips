# fips-mobile

`fips-mobile` is the mobile-oriented Rust package boundary for embedding FIPS in
Android now and iOS later.

It currently re-exports the proven embedded client from the root `fips` crate:

- start an in-process FIPS node with TUN, DNS, and control disabled;
- connect to a peer `npub` through Nostr/STUN traversal;
- wait for an encrypted FSP service session;
- send FIPS Drop v0 blobs to service port `4242`;
- receive service packets and status snapshots.

The crate also provides product-name FIPS Drop aliases while preserving the
older `dropbox` function names for PoC compatibility.

The current Android bridge in the Pushstr app depends on this crate and exposes
`fipsMobileSendFipsDropBlob` as the product-named send API. The older
`fipsMobileSendDropboxBlob` bridge call remains as a wrapper so existing builds
do not break while the Dart/UI layer finishes moving to FIPS Drop terminology.

## Android Build

```bash
cargo ndk -t arm64-v8a build \
  -p fips-mobile \
  --release \
  --no-default-features \
  --features nostr-discovery
```

The crate emits `rlib`, `staticlib`, and `cdylib` artifacts. The next layer can
generate JNI, Flutter Rust Bridge, or UniFFI bindings from this crate instead of
binding against the root daemon package.

For a faster compile-only Android verification:

```bash
cargo ndk -t arm64-v8a check \
  -p fips-mobile \
  --no-default-features \
  --features nostr-discovery
```

## iOS Direction

The crate is structured to support an iOS wrapper, but iOS target cleanup is
still required in the root `fips` crate before that target should be considered
supported. In particular, host TUN/gateway dependencies should be made fully
feature-gated for mobile builds.
