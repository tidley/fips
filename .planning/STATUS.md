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

Immediate priority:

1. Add/prove a public embedded bootstrap wrapper around existing Nostr discovery/traversal.
2. Use that wrapper to request/connect to a node by npub/peer config via Nostr adverts/signaling.
3. Adopt the established UDP traversal into FIPS and verify peer/session readiness without TUN/VPN.
4. Then add/prove Dropbox service-port payload send/receive, currently proposed as port `4242`.
5. Then bridge that API into Pushstr's Flutter/Rust mobile stack.

Worktree caution:

- Pre-existing dirty state remains: deleted `.planning/peer-assisted-core-34e00b9-to-3eaf9ac.diff` and untracked `.planning/pushstr-fips-dropbox-plan.md`.
- Ignored local key/config files exist; keep them out of PoC material and commits.
