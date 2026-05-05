# NEXT

- Decide the first runnable packaging shape for the Pi4ssd receiver:
  - small `fips-dropbox-agent` binary in this repo,
  - separate companion crate,
  - or direct Pushstr/Flutter bridge consumption first.
- If adding a binary, wire the existing receiver logic to a live node:
  - register service port `4242`,
  - poll `ServiceRx`,
  - store files under configured storage root,
  - send acknowledgements back over `send_service_data`.
- Add a `flutter_rust_bridge` mobile surface after the Rust app-service API is proven:
  - likely via Pushstr's existing `/home/tom/code/pushstr/pushstr_rust` crate or a small dedicated `fips_mobile` crate.
- Build the Android Pushstr-style send/share UI against the embedded FIPS library.
- Add Blossom/Nostr metadata only after raw direct FIPS transfer succeeds.
