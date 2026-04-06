# fips-nostr-bootstrap

Experimental Nostr-signaled UDP traversal and FIPS handoff examples, now vendored directly into the `fips` repo for issue 37 work.

This subcrate contains:
- Nostr advert / offer / answer protocol types and helpers
- STUN URL parsing and startup observation helpers
- binary UDP punch / ack packet helpers
- punch target planning and timing helpers
- CLI console client/server examples that bootstrap over Nostr, punch UDP, and then hand the live socket into the in-tree FIPS transport
- local HTTP daemon variants for console, web, and video frontends
- a shell-oriented server example for remote command execution over the established session

It does **not** include the browser `.mjs` UI from the separate prototype repo.

## Files

- [src/lib.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/lib.rs)
  - protocol constants
  - advert / offer / answer structs
  - STUN helpers
  - binary punch packet helpers
  - punch planning helpers
  - protocol-level tests
- [src/fips_handoff.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/fips_handoff.rs)
  - bridge from successful traversal into the local `fips` crate
  - app-port runtime helper for post-handoff examples
- [src/bin/fips-console-server.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-console-server.rs)
  - advertised responder
  - receives traversal offers
  - publishes traversal answers
  - performs UDP punch response
  - hands the punched socket into FIPS
  - provides a pure CLI text console
- [src/bin/fips-console-client.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-console-client.rs)
  - discovers adverts or targets explicit `npub`
  - performs STUN, offer/answer signaling, and punch attempts
  - hands the punched socket into FIPS
  - provides a pure CLI text console
- [src/bin/fips-console-daemon.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-console-daemon.rs)
  - local HTTP daemon for a console-style frontend
  - exposes `/api/meta`, `/api/discover`, `/api/connect`, `/api/send`, and `/api/events`
  - hands the punched socket into FIPS app-port runtime on port `4200`
- [src/bin/fips-web-daemon.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-web-daemon.rs)
  - local HTTP daemon for a web console frontend
  - exposes `/api/meta`, `/api/discover`, `/api/connect`, `/api/cmd`, `/api/ctrlc`, and `/api/events`
  - relays session frames after traversal and optional FIPS handoff
- [src/bin/fips-video-daemon.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-video-daemon.rs)
  - local HTTP daemon for a video-oriented frontend
  - exposes `/api/meta`, `/api/discover`, `/api/connect`, `/api/frame`, `/api/remote-frame`, and `/api/events`
  - hands the punched socket into FIPS app-port runtime on port `4100`
- [src/bin/fips-shell-server.rs](/home/tom/code/fips/examples/nostr-bootstrap/src/bin/fips-shell-server.rs)
  - advertised server that accepts rendezvous offers and runs shell commands over the session
  - supports `--trusted-npubs` filtering for allowed peers
  - can either keep raw traversal handling or hand the established session into FIPS

## Build

```bash
cd /home/tom/code/fips
cargo build --manifest-path examples/nostr-bootstrap/Cargo.toml --bins
```

## Run

Available binaries:
- `fips-console-server`
- `fips-console-client`
- `fips-console-daemon`
- `fips-web-daemon`
- `fips-video-daemon`
- `fips-shell-server`

Server:

```bash
cd /home/tom/code/fips && \
FIPS_STUN_SERVERS='stun:fips.tomdwyer.uk:3478,stun:stun.l.google.com:19302' \
cargo run --manifest-path examples/nostr-bootstrap/Cargo.toml --bin fips-console-server -- \
  --nsec '<SERVER_NSEC>' \
  --udp-port 9999 \
  --advert-relays 'wss://offchain.pub,wss://strfry.bitsbytom.com' \
  --dm-relays 'wss://nip17.com,wss://offchain.pub'
```

Run a client on another machine with an explicit target:

```bash
FIPS_STUN_SERVERS='stun:fips.tomdwyer.uk:3478,stun:stun.l.google.com:19302' \
cargo run --manifest-path examples/nostr-bootstrap/Cargo.toml --bin fips-console-client -- \
  --nsec '<CLIENT_NSEC>' \
  --npub '<SERVER_NPUB>' \
  --advert-relays 'wss://offchain.pub,wss://strfry.bitsbytom.com' \
  --dm-relays 'wss://nip17.com,wss://offchain.pub'
```

Or let the client discover the first available advert by omitting `--npub`:

```bash
FIPS_STUN_SERVERS='stun:fips.tomdwyer.uk:3478,stun:stun.l.google.com:19302' \
cargo run --manifest-path examples/nostr-bootstrap/Cargo.toml --bin fips-console-client -- \
  --nsec '<CLIENT_NSEC>' \
  --advert-relays 'wss://offchain.pub,wss://strfry.bitsbytom.com' \
  --dm-relays 'wss://nip17.com,wss://offchain.pub'
```


After connection, type in either terminal to exchange text over a FIPS session bootstrapped by Nostr relay discovery, DM signaling, STUN, and UDP hole punching.

## Intended PR Scope

This subcrate is the issue-37 bootstrap layer:
- Nostr-based discovery and signaling
- STUN-driven reflexive address discovery
- UDP hole punching
- handoff into the real FIPS transport

The remaining work after upstreaming is polishing how much of this should move from example code into the main `fips` daemon/config surface.
