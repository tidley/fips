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

Immediate priority:

1. Decide whether the Pi4ssd receiver becomes a small `fips-dropbox-agent` binary, a crate API consumed by Pushstr, or both.
2. Run real Nostr/STUN traversal against Pi4ssd/Pi/mobile hardware.
3. Bridge the proven Rust API into Pushstr's Flutter/Rust mobile stack.
4. Add Blossom/Nostr metadata after raw direct FIPS transfer succeeds.

Worktree caution:

- Ignored local key/config files exist; keep them out of PoC material and commits.
