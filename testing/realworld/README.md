# Real-World Functional Harness

This directory contains opt-in tests that use live public infrastructure rather
than Docker-only local fixtures. The first harness starts an in-process FIPS
Drop receiver and mobile-style sender, publishes ephemeral Nostr adverts,
performs NIP-59/STUN NAT traversal, opens the normal FIPS service session, sends
a file over service port `4242`, and verifies the receiver's stored file hash.

## FIPS Drop Transfer

Run from the repository root:

```bash
FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh
```

Useful options:

```bash
FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh \
  --runs 3 \
  --payload-bytes 1048576 \
  --keep-artifacts \
  --json
```

Defaults:

- Relays: `wss://relay.damus.io`, `wss://nos.lol`, `wss://offchain.pub`
- STUN: `stun:stun.l.google.com:19302`,
  `stun:stun.cloudflare.com:3478`, `stun:global.stun.twilio.com:3478`
- Payload: 192 KiB deterministic binary file
- Storage: a temporary directory removed after success unless
  `--keep-artifacts` or `--storage-root` is used
- Local RFC1918/ULA candidates are advertised by default so a single developer
  machine can exercise the full relay/signaling/session/transfer path without
  depending on router hairpin behavior. Use `--no-local-candidates` when you
  specifically want an internet-reflexive traversal check.

The harness creates a unique Nostr discovery app namespace per run, so public
relay traffic should not collide with normal FIPS nodes or other test runs.

## What It Covers

- Public relay connectivity for adverts and encrypted signaling.
- STUN observation and Nostr traversal handoff.
- UDP transport adoption into the normal FIPS handshake.
- End-to-end FSP service session establishment.
- FIPS Drop binary transfer, sparse ACK/repair path, stored-file verification.

## Limits

Single-host execution is still subject to the network it runs on. Some NATs,
firewalls, VPNs, or host routing rules can prevent the two local nodes from
using the reflexive path even though public relays work. The default local
candidate mode avoids that for developer automation; `--no-local-candidates`
is stricter and may fail on networks that do not support hairpin traversal.

This harness is intentionally not part of normal CI. It depends on public
relays, public STUN, and current network conditions.
