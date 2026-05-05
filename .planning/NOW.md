# NOW

- Focus on the Android FIPS Dropbox PoC, not repository demo harnesses.
- Build the first Rust slice for embedded app-owned FIPS communication:
  - TUN disabled,
  - no system-wide Android VPN dependency,
  - Nostr advert/signaling bootstrap,
  - STUN-assisted UDP hole punch,
  - adopt established UDP traversal into the FIPS node,
  - verify normal FIPS peer/session readiness over that punched UDP path.
- Then build the app-service send/receive API over a reserved Dropbox service port, proposed `4242`.
- Keep `.planning/ANDROID-FIPS-DROPBOX-POC.md` as the current execution plan.
- Keep local key/config material out of all PoC docs and commits.
