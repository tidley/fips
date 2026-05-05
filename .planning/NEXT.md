# NEXT

- Create the Pi4ssd receiver-agent shape using the same app-service API:
  - listen on service port `4242`,
  - accept `hello`, `put`/`put_chunk`, `put_done`,
  - write files under a configured test directory,
  - reply with hash/status acknowledgements.
- Add a `flutter_rust_bridge` mobile surface after the Rust app-service API is proven:
  - likely via Pushstr's existing `/home/tom/code/pushstr/pushstr_rust` crate or a small dedicated `fips_mobile` crate.
- Build the Android Pushstr-style send/share UI against the embedded FIPS library.
- Add Blossom/Nostr metadata only after raw direct FIPS transfer succeeds.
