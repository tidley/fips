@tidley, one piece of context up front: the prior review rounds were largely mechanical passes focused on specific items (feature gating, clock sources, task spawning, identity verification). Useful and necessary, but narrow in scope. This is the first round where I've had the chance to do a thorough manual analysis of the full implementation end-to-end.

Credit where it's due: you've been responsive across every round, and what's here works. Feature gating, app-port split, rustfmt alignment, transport cleanup on disconnect, STUN IPv6, monotonic clock, bounded task spawning: all addressed, with a working multi-scenario NAT test harness to back it up. The UDP hole-punching mechanism itself is solid: offer/answer via NIP-59, STUN-driven reflexive discovery, mutual UDP probes, and socket handoff into the FIPS transport stack all hang together, and the integration tests confirm the Noise handshake completes over the punched socket. That's the hard part of #37, and it's proven out.

I know this round lands differently than the previous ones, so some background on why. #34 (Nostr relay peer discovery for overlay transports) was always intended to be the first step of this work, with hole punching layered on top once the advert and discovery plumbing was in place. The current PR went straight to the harder problem, which worked (the protocol runs end-to-end), but the advert format, subscription scheme, and discovery flow that came out of it are shaped specifically for NAT punch. Merging as-is means a wire-format migration later when we come back to finish #34 properly. I'd rather backfill #34 into this PR so both issues close together on a single schema.

The asks break into four concrete structural items, which I'll lay out in separate comments below so we can discuss each independently. There's also a handful of smaller items I noticed, but I'll hold those for a follow-up round: some may reshape or dissolve during the rework anyway. Push back on anything that feels off. The payoff is that after the restructuring the feature lands as "Nostr-mediated overlay peer discovery with UDP hole-punch fallback," which closes both #37 and #34 in one PR and fits cleanly into the shape the daemon was built to host.

**Structural item 1 of 4: generalize the advert and build out the peer discovery mechanism — close #34 in this PR**

The feature this wants to land is **relay-mediated overlay peer discovery**: a node with a listening UDP/TCP/Tor endpoint publishes a signed advert on one or more Nostr relays, and other nodes subscribe and learn where to reach peers they care about. Conceptually it plays the same role that Ethernet and BLE beacons already play in the codebase — a way for nodes to find each other without manually configured addresses — but operating over public relay infrastructure rather than a single L2 segment, so it reaches past local scope. NAT hole-punching becomes one endpoint type in that advert (`"I only accept connections via rendezvous"`), not the whole point of it.

Mechanically it breaks into three pieces, each of which the current PR has partial primitives for:

1. **Production.** Each overlay transport (UDP, TCP, Tor) gets an `advertise_on_nostr: bool` flag in its config. When set, the transport's listening endpoint is bundled into the node's advert at publish time. The nostr subsystem queries operational transports at advert-build time — so if a transport goes down, the next refresh excludes it. Transports don't depend on the nostr crate; they just expose a method that returns their advertisable endpoint. (Config surface and validator rules are covered in item 3.)

2. **Publication.** The advert carries a list of `{transport, addr}` endpoints with the same shape `PeerConfig.addresses` already uses in fips.yaml, with `addr:"nat"` (UDP-only) signaling "rendezvous required." `signalRelays` and `stunServers` are only present when the advert includes a `udp:"nat"` entry — pure direct-dial advertisers don't publish them at all.

3. **Consumption.** A periodic tick walks `advert_cache`, filters by a configurable policy, and synthesizes in-memory `PeerConfig` entries from adverts for eligible npubs. Those go through `try_peer_addresses` — which is already the merged dial entry point. Direct endpoints dial via the existing UDP/TCP/Tor transports unchanged; `udp:"nat"` endpoints fall into the rendezvous path you already built.

The PR today has partial (1) (a single hardcoded advert built from one config block), partial (2) (schema is NAT-specific and can't express the other endpoint types), and none of (3). Backfilling these is what closes #34 alongside #37.

**Proposed advert shape:**

```json
{
  "publisherNpub": "npub1...",
  "publishedAt":   "...",
  "expiresAt":     "...",
  "sequence":      "...",
  "endpoints": [
    { "transport": "udp", "addr": "203.0.113.5:2121" },
    { "transport": "udp", "addr": "nat" },
    { "transport": "tcp", "addr": "198.51.100.7:8443" },
    { "transport": "tor", "addr": "abc...def.onion:8443" }
  ],
  "signalRelays": ["wss://..."],
  "stunServers":  ["stun:..."]
}
```

The `{transport, addr}` entries are structurally identical to `PeerConfig.addresses`, so synthesizing a peer config from an advert is a direct copy. No `natRequired` flag is needed — `"nat"` as addr is UDP-only by construction (the only transport with a punch pathway in this PR) and reuses the existing sentinel recognition in `try_peer_addresses`.

The subscription filter should include `.identifier("fips-overlay-v1")` so a node only ingests relevant adverts. Currently it's wildcard over Kind 30078, which picks up every NIP-33 event on that kind from every unrelated app — `advert_cache` grows unbounded on a busy relay.

**Connection policy**, configurable, default `config`:

| policy | behavior |
|--------|----------|
| `config` | Dial only npubs in the static `peers:` list. For entries with `via_nostr: true`, consult the peer's advert to resolve the endpoint address. |
| `open` | In addition to `config` behavior, also dial npubs that appear in the advert cache but aren't in static config. Public-mesh / bootstrap-node mode. |

Per-npub cooldown prevents an advert refresh from triggering a burst of reconnects.

**Closes** #37 and #34 together — the point of the restructuring is that after this item lands, the PR delivers the feature in the shape the daemon was built to host it.

**Structural item 2 of 4: start transports before any Nostr activity**

`NostrBootstrap::start()` currently runs at [lifecycle.rs:525](https://github.com/jmcorgan/fips/blob/issue37/src/node/lifecycle.rs#L525), before `create_transports()` and before any `handle.start()` on the transports. That sequence needs to flip.

Even with the current NAT-specific advert, there's a window between "advert published" and "UDP transport listening" during which a peer who sees the advert and dials gets a connection-refused: the advert says "I accept rendezvous" but the daemon hasn't bound its UDP socket yet. Small but real.

Once item 1 lands, the ordering becomes hard-required: the advert's `endpoints` list is built by querying each operational transport for its advertisable endpoint. If transports haven't started yet there's nothing to enumerate — you'd either publish an empty advert or have to defer publication anyway.

**The fix:** move the `NostrBootstrap::start()` block to after the transport-start loop. Proposed new order in `Node::start()`:

1. State transition + packet channel (unchanged)
2. `create_transports(...).await`
3. Loop: `handle.start().await` on each, insert into `self.transports`
4. TUN setup (unchanged)
5. **← `NostrBootstrap::start()` here.** `publish_advert` can now enumerate real transport endpoints.
6. Spawn rx_loop + remaining periodic tasks

`NostrBootstrap::start()` failure stays non-fatal (warn + continue), same as today.

**Shutdown order is already correct** — `bootstrap.shutdown()` runs before transport stop in `Node::stop()`, which withdraws the advert before transports go dark. Symmetric with the new startup order. Nothing to change there.

**Tor onion propagation caveat:** a Tor transport `handle.start()` returning `Ok` means local Tor sockets are up, not that the onion descriptor has propagated to the Tor DHT (typically 30–60 seconds on a warm Tor). If the advert lists a Tor onion immediately, peers dialing within that window will fail and retry. My preference is to accept the window — retries are already in the dialer path, descriptor propagation is fast enough in practice, and waiting on it would add plumbing for a transient condition. Worth one sentence in the proposal doc so operators aren't surprised.

**Structural item 3 of 4: per-transport advertise flag, peer `via_nostr` flag, config validator rule**

This is the configuration-surface and layering detail for item 1's "production" piece. It also supersedes the current `node.discovery.nostr.advertise` flag — that belongs on the transports, not in the nostr block.

### Transport config surface

Each overlay transport gets an `advertise_on_nostr: bool`. UDP additionally gets a `public: bool` that determines whether the advert lists a direct address or the rendezvous sentinel:

```yaml
transports:
  udp:
    bind: "0.0.0.0:2121"
    advertise_on_nostr: true
    public: true                  # or false, for a NAT'd node
  tcp:
    bind: "0.0.0.0:8443"
    advertise_on_nostr: true
  tor:
    onion: "abc.onion:8443"
    advertise_on_nostr: true
```

UDP semantics:
- `public: true` → advert gets `{udp, "<bind-addr>"}`. Peers dial directly; NAT mapping on their side opens on first packet.
- `public: false` → advert gets `{udp, "nat"}`. Peers initiate the rendezvous + punch flow to reach this node.

This is binary — either the UDP endpoint is publicly reachable and rendezvous is not necessary, or it isn't and rendezvous is the only path. No meaningful third state.

TCP and Tor don't have this duality in this PR, so they only have the advertise flag.

### Layering

Transports don't depend on the nostr crate. Each transport exposes a method roughly like:

```rust
fn advertisable_endpoint(&self) -> Option<AdvertEndpoint>;
```

The nostr subsystem iterates `self.transports` at advert-build time, calls the method on each, builds the endpoints list. No nostr types leak into the transport modules.

### Config validator rule

`advertise_on_nostr: true` on any transport requires `node.discovery.nostr.enabled = true` — otherwise there's nothing to advertise with. Reject the config at load time.

That's the only structural validator rule needed; the rest falls out of the binary public/non-public semantics above.

### Peer config: `via_nostr`

Today `peers:` entries bind an npub to a hard-coded address list. With adverts as a runtime source of endpoints, peer entries gain a `via_nostr` field:

```yaml
peers:
  - npub: npub1X
    via_nostr: true                 # resolve endpoints from advert
  - npub: npub1Y                    # legacy: explicit addresses
    addresses:
      - {transport: udp, addr: "1.2.3.4:2121"}
  - npub: npub1Z                    # hybrid: static + advert fallback
    addresses:
      - {transport: udp, addr: "5.6.7.8:2121"}
    via_nostr: true
```

At dial time, `try_peer_addresses` merges:

1. Explicit `addresses:` from config (tried first — operator intent).
2. If `via_nostr: true` and an advert is cached for this npub, append the advert's endpoints.

`{transport:"udp", addr:"nat"}` from either source triggers the rendezvous path — unchanged from the current PR.

### Three sources of a dial attempt

After items 1 and 3 land, a connection attempt can originate from:

| source | controlled by |
|--------|---------------|
| Static addresses | Operator — `addresses: [...]` on a peer entry |
| Advert for a statically-named npub | `via_nostr: true` on a peer entry |
| Advert for a non-configured npub | Discovery loop + `discover_policy: open` |

All three feed the same merged list consumed by `try_peer_addresses`.

**Structural item 4 of 4: delete `examples/nostr-bootstrap/`**

The sub-crate at `examples/nostr-bootstrap/` (package `fips-nostr-rendezvous`) looks like the original prototype that validated the design before the integrated version was carved into `src/bootstrap/nostr/`. Now that the integration is in place, the sub-crate can go.

**It's safe to delete** — nothing in the repo depends on it. `grep -r "examples/nostr-bootstrap\|fips-nostr-rendezvous"` across `testing/nat/` returns zero matches. The NAT harness's `docker-compose.yml` runs `image: fips-test:latest`, and the node entrypoint execs the real fips daemon — same pattern as `testing/static/` and `testing/rekey/`. The integration tests already exercise the real runtime end-to-end against a real strfry relay and a Python STUN server. No harness work needed.

**Reasons to remove it:**

- **Duplicate protocol code.** Wire types, STUN parsing, punch packet logic, and signaling helpers all exist twice — here and in `src/bootstrap/nostr/`. Future protocol changes have to be made in two places and can drift silently.
- **Diverged shape.** The sub-crate is split into `client_runtime/` + `server_runtime/`; the integrated version uses a single unified `NostrBootstrap` for both roles. The older shape is frozen and nothing else in the codebase follows it.
- **No runnable binaries.** The README notes that the demo binaries were removed in a prior review round (the AppDatagram split). What's left is library code with no entry points.
- **Diff-size cost.** ~3,200 of the 8,488 additions in this PR are here. That's review load for zero shipped functionality.

**What to do:** delete the directory, and port any tests from `examples/nostr-bootstrap/src/tests.rs` that aren't already covered by `src/bootstrap/nostr/tests.rs` into the main crate. A cursory comparison suggests most protocol tests are already in both places.

**Small side note on naming:** the sub-crate is named `fips-nostr-rendezvous` while the main-crate feature is `nostr-bootstrap`. You already reached for a different word to name the standalone version, which is a hint that "bootstrap" isn't the right name for the whole thing. We'll settle on a consistent name before merge — just flagging that the naming is on the list.

 SatsAndSports
commented
3 days ago

A small suggestion on privacy, deviating from the protocol:

The giftwrapped messages are addressed to the pubkeys of the two nodes. I suggest changing the 'offer', which is sent by the initiator with the STUN results, such that it includes an ephemeral pubkey to be used for the reply. The final 'answer', which includes the responder's STUN results, can then be addressed to that pubkey

With this setup, third parties can see the original advertizement and can see that somebody initiatiated a connection to that npub (i.e. the responder's npub), but they can't see the npub of the initiator


