# NOW

- Focus on the Android FIPS Dropbox PoC, not repository demo harnesses.
- Current Rust slice is implemented and covered:
  - embedded Nostr bootstrap wrapper,
  - TUN disabled,
  - no system-wide Android VPN dependency,
  - traversal event adoption into the FIPS node,
  - normal FIPS peer/session readiness over adopted UDP,
  - app-service send/receive API,
  - Dropbox-style service protocol on port `4242`,
  - Pi4ssd receiver-agent logic for filesystem storage and acknowledgements.
- Next work is at the integration boundary:
  - package the receiver as a runnable Pi4ssd agent or keep it as a library first,
  - test real Nostr/STUN traversal on hardware,
  - add the Flutter/Pushstr bridge.
- Keep `.planning/ANDROID-FIPS-DROPBOX-POC.md` as the current execution plan.
- Keep local key/config material out of all PoC docs and commits.
