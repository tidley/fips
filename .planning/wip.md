Below is the revised implementation plan, still aligned exactly to the maintainer’s latest capability breakdown, but now incorporating the four additional corrections:

* avoid over-publishing adverts
* preserve true static-first behaviour
* include NAT-only peers in open discovery
* never publish wildcard UDP bind addresses as dial addresses

## Objective

Implement Nostr support as four operator-selectable capability groups:

* outbound connections to configured peers
* inbound advertisement of this node’s reachable endpoints
* UDP NAT rendezvous as a subcase of Nostr-resolved UDP
* optional discovery loop for acquiring additional peers up to a configured limit

The implementation must keep one common dial path and one common advert model.

---

## Core design rules

1. **Static peers remain the baseline**

   * Existing static UDP/TCP/.onion peer behaviour must continue to work unchanged.

2. **`via_nostr` applies to configured peers only**

   * It means: the peer identity is known, but its current endpoint is obtained from Nostr.

3. **UDP rendezvous is a subcase**

   * Only when the resolved UDP endpoint is NATed should the Nostr rendezvous flow run.

4. **Discovery is separate**

   * Opportunistic acquisition of non-configured peers must be implemented as its own bounded feature, not folded into `via_nostr`.

5. **Inbound advertisement is independent**

   * A node may listen publicly and choose whether or not to advertise on Nostr.

6. **Advert publication must be event-driven plus refresh-driven**

   * Publish when locally advertised endpoint state changes.
   * Republish on the existing periodic refresh loop.
   * Do not publish on generic RX/tick traffic.

7. **Static-first means no remote fetch before static dial**

   * For configured peers, Nostr lookup/fetch belongs strictly in the fallback phase.

8. **`udp:nat` is a valid candidate type**

   * Discovery must not drop NAT-only adverts.

9. **Bind addresses are not advert addresses**

   * Never publish `0.0.0.0:port` or `[::]:port` as direct peer endpoints.

---

# Phase 1 - Refactor the config model to match the capability breakdown

## 1.1 Outbound peer config

Preserve existing peer config for static peers.

Extend configured peer entries to support dynamic endpoint lookup:

```yaml
peers:
  - npub: npub1...
    addresses:
      - { transport: udp, addr: "5.6.7.8:2121" }
    via_nostr: true
```

Semantics:

* `addresses` absent or stale: use Nostr advert lookup
* `addresses` present: try static first, then Nostr-resolved endpoint if `via_nostr: true`
* `via_nostr` does not mean open discovery

## 1.2 Inbound transport config

Move advertisement choices onto transports.

Per transport, support:

* listen enabled/disabled
* advertise_on_nostr enabled/disabled

For UDP specifically, add:

* `public: true|false`

Semantics:

* `public: true` -> advertise direct UDP endpoint
* `public: false` + `advertise_on_nostr: true` -> advertise UDP NAT rendezvous capability

## 1.3 Discovery config

Add a distinct discovery section.

Example shape:

```yaml
discovery:
  enabled: true
  max_sessions: 8
  scan_interval_secs: 60
```

Discovery must be:

* explicit
* bounded
* independent from configured-peer `via_nostr`

## 1.4 Validation rules

Implement strict validation:

* if any transport has `advertise_on_nostr: true`, Nostr must be enabled
* if any peer has `via_nostr: true`, Nostr must be enabled
* if discovery is enabled, Nostr must be enabled
* UDP `public: false` is valid only when rendezvous metadata can be advertised

---

# Phase 2 - Define one advert format that serves both inbound advertisement and outbound lookup

## 2.1 Replace NAT-specific advert payloads

Use a common endpoint advert model for all Nostr-published overlay information.

Target payload shape:

```json
{
  "identifier": "fips-overlay-v1",
  "version": 1,
  "endpoints": [
    { "transport": "tcp", "addr": "203.0.113.20:443" },
    { "transport": "udp", "addr": "nat" },
    { "transport": "tor", "addr": "abcxyz.onion:9001" }
  ],
  "signalRelays": ["wss://relay.example"],
  "stunServers": ["stun:stun.example.org:3478"]
}
```

## 2.2 Advert semantics

* direct TCP, direct UDP, and Tor are ordinary endpoints
* UDP behind NAT is represented as:

  * `{"transport":"udp","addr":"nat"}`
  * plus rendezvous metadata
* `signalRelays` and `stunServers` are present only when a UDP `nat` endpoint is advertised

## 2.3 Scope adverts tightly

All adverts and subscriptions must be scoped to `fips-overlay-v1`.

This advert format must serve two consumers:

* configured peers with `via_nostr: true`
* optional discovery loop

---

# Phase 3 - Implement inbound advertisement exactly as the maintainer described

## 3.1 Build adverts from active transports

After transports are created and started, build the local advert from actual runtime transport state.

For each active transport:

* if `advertise_on_nostr: false`, omit it
* if TCP and public, advertise direct TCP address
* if Tor and available, advertise onion endpoint
* if UDP and `public: true`, advertise direct UDP address
* if UDP and `public: false`, advertise `udp:nat` plus rendezvous metadata

## 3.2 Suppress wildcard bind addresses

When advertising direct UDP:

* do not publish `0.0.0.0:port`
* do not publish `[::]:port`
* publish only a concrete peer-usable address

If no concrete usable UDP address is available yet:

* omit the direct UDP endpoint for now, or
* fall back to `udp:nat` only if that reflects intended operator semantics

Do not serialise unspecified bind addresses into adverts.

## 3.3 Publish only after transports are live

Startup order must be:

1. parse config
2. create transports
3. start transports
4. determine live advertised endpoints
5. start Nostr subsystem
6. publish advert

This prevents advertising endpoints before listeners exist.

## 3.4 Publish on state change plus refresh only

Advert publication must be triggered by:

* meaningful local advertised-endpoint state changes
* the existing periodic refresh loop

It must **not** be triggered by:

* generic RX ticks
* unrelated transport activity
* per-message receive paths

This avoids relay spam and unnecessary replace/delete churn for replaceable events.

## 3.5 Refresh adverts as needed

If a transport’s public-facing address or readiness changes:

* mark advert state dirty
* publish updated advert on the state-change path
* retain the periodic refresh loop as background republish

This is especially relevant for delayed readiness such as Tor.

---

# Phase 4 - Implement outbound configured-peer resolution exactly as the maintainer described

## 4.1 Case A: static configured peer only

No Nostr involvement.

Behaviour:

* use configured static `addresses`
* dial through the existing path

## 4.2 Case B: configured peer with `via_nostr`

This is the “dynamic DNS” case.

Behaviour:

* peer identity is configured by npub
* current endpoint is obtained from the peer’s `fips-overlay-v1` advert on Nostr
* resolved endpoints are converted into ordinary dial candidates

Resolution order must be:

1. try static configured addresses first, if present
2. only after static failure or absence, consult cached advert state
3. only after cache miss / stale cache on the fallback path, perform network fetch if needed
4. append or substitute resolved endpoints into the normal dial path

Important:

* do not block initial static dial attempts on remote advert fetch
* cache-first, network-later on the fallback phase

## 4.3 Case C: configured peer with `via_nostr`, resolved UDP endpoint is NATed

This is the rendezvous subcase.

Behaviour:

* retrieve the configured peer’s advert
* find UDP endpoint with `addr:"nat"`
* obtain its rendezvous metadata
* invoke the existing Nostr-based rendezvous / STUN / probe flow
* hand the resulting adopted UDP socket into the normal transport stack

Important:

* this must remain a continuation of configured-peer dialling
* do not model it as a separate peer acquisition path

---

# Phase 5 - Implement discovery as its own subsystem

## 5.1 Discovery purpose

Discovery is for this operator choice only:

* “make outbound connections up to a maximum number of sessions and periodically scan Nostr for new endpoints”

This is distinct from configured peers.

## 5.2 Discovery loop behaviour

Implement a periodic task that:

1. scans scoped `fips-overlay-v1` adverts
2. filters out peers already connected or otherwise unsuitable
3. selects candidates while below `max_sessions` or equivalent cap
4. converts selected adverts into dial candidates
5. dials them through the same normal dial path

## 5.3 Discovery candidate types

Discovery may yield:

* direct TCP candidates
* direct UDP candidates
* Tor candidates
* UDP NAT candidates requiring rendezvous

All must still feed one dial path.

## 5.4 Do not exclude NAT-only adverts

Discovery must treat NAT-only adverts as valid candidates.

Do **not** require a candidate advert to contain at least one direct/non-`udp:nat` endpoint.

Reason:

* `udp:nat` is a supported transport acquisition path
* NAT-only peers must remain discoverable when discovery is enabled

Candidate filtering should answer:

* is this a valid scoped FIPS advert with usable transport semantics?

It should not answer:

* can this be direct-dialled without extra steps?

## 5.5 Discovery boundaries

Discovery must not:

* override configured peer semantics
* bypass session limits
* behave as an implicit default of `via_nostr`

Configured peers and discovery-acquired peers should remain distinguishable in runtime state.

---

# Phase 6 - Keep one shared dial path

## 6.1 Normalise all outbound attempts into endpoint candidates

All outbound connection sources must produce the same internal candidate shape:

* configured static address
* configured peer advert-resolved endpoint
* discovery-found endpoint
* UDP NAT rendezvous candidate

## 6.2 Dispatch by transport type

The common dial path should then branch only on resolved candidate type:

* TCP direct -> normal TCP dial
* UDP direct -> normal UDP dial
* Tor -> normal Tor dial
* UDP NAT -> rendezvous then adopt socket

## 6.3 Preserve fallback ordering

For configured peers:

* static first
* cached `via_nostr` fallback second
* fetched `via_nostr` fallback third, if needed

For discovery:

* only use discovery candidates when the discovery subsystem selects them

---

# Phase 7 - Rework advert ingestion and cache semantics to match the new roles

## 7.1 Cache by author and recency

Maintain a normalised cache of parsed `fips-overlay-v1` adverts keyed by author pubkey.

Each cache entry should store:

* author pubkey
* parsed advert
* received time
* event creation time

## 7.2 Use cache for two distinct consumers

The same cache serves:

* configured-peer endpoint lookup via `via_nostr`
* discovery scan candidate selection

## 7.3 Bound cache growth

Implement boundedness through:

* scoped subscription filters
* replace-by-author semantics
* age pruning

Do not allow unbounded accumulation of unrelated events.

## 7.4 Support static-first fallback latency profile

The cache layer must support fast fallback for configured peers:

* lookup fresh cached advert immediately after static failure
* only then consider network fetch/update

Avoid putting relay I/O on the initial hot path for configured peers with static addresses.

---

# Phase 8 - Remove the duplicate prototype implementation

## 8.1 Delete `examples/nostr-bootstrap/`

Remove the example/prototype crate entirely.

## 8.2 Preserve only useful tests

Inspect the prototype tests and migrate any coverage that is still valuable into the main crate.

Do not preserve parallel implementation code.

---

# Phase 9 - Update tests by capability group

## 9.1 Outbound tests

Cover:

* configured static peer only
* configured peer with `via_nostr` direct TCP/UDP/Tor endpoint resolution
* configured peer with `via_nostr` and UDP NAT rendezvous
* static-first ordering where static attempts occur before cache miss fetch
* cache-first fallback where cached advert is used without network fetch
* fetched fallback only after static failure and cache miss/staleness

## 9.2 Inbound tests

Cover:

* transport advertised vs not advertised
* UDP public advert generation
* UDP NAT advert generation with rendezvous metadata
* advert publication only after transport startup
* no advert publication on generic RX/tick traffic
* publication on endpoint-state change
* periodic refresh republish still works
* wildcard UDP bind addresses are suppressed from adverts

## 9.3 Discovery tests

Cover:

* periodic scan of scoped adverts
* discovery obeys max session cap
* discovery excludes already connected peers
* discovery can select direct candidates
* discovery can select NAT-only candidates
* NAT-only discovery candidates reach the `udp:nat` rendezvous branch

## 9.4 Common advert tests

Cover:

* serialisation/deserialisation of endpoint adverts
* rejection of malformed or wrongly scoped adverts
* cache replacement/pruning behaviour

---

# Phase 10 - Update docs and examples to reflect the new capability split

## 10.1 Document outbound modes

Document three outbound cases explicitly:

1. static peer only
2. configured peer with `via_nostr`
3. configured peer with `via_nostr` plus UDP NAT rendezvous

Also document the resolution order clearly:

* static first
* cached advert fallback
* fetched advert fallback

## 10.2 Document inbound modes

Document two inbound cases explicitly:

1. publicly reachable transport, optionally advertised
2. UDP behind NAT advertised with rendezvous metadata

Also document that:

* unspecified bind addresses are never advertised as direct endpoints

## 10.3 Document discovery separately

Document discovery as:

* optional
* bounded
* used to acquire additional peers up to a configured limit

Make clear it is not the same thing as `via_nostr`.

Also document that NAT-only peers are valid discovery candidates.

---

# Suggested implementation order for the LLM

Work in this order:

1. refactor config structs and validation
2. introduce the common advert model
3. rework inbound advert publication

   * include wildcard suppression
   * include state-change + refresh publish logic
   * remove generic tick-driven publish
4. rework configured-peer `via_nostr` resolution

   * enforce static-first
   * enforce cache-first fallback
   * only fetch on fallback miss
5. wire UDP NAT rendezvous into the configured-peer path
6. add the discovery loop as a separate subsystem

   * include NAT-only advert eligibility
7. unify all candidate types into one dial path
8. delete the example crate
9. update tests
10. update docs

---

# Acceptance criteria

The work is complete when all of the following are true:

* static peers still work exactly as before
* configured peers can use `via_nostr` to resolve current endpoints
* configured peers with Nostr-resolved UDP NAT endpoints can rendezvous successfully
* static peer dial is never delayed by eager remote advert fetch
* transports can advertise inbound endpoints on Nostr
* advert publication occurs only on endpoint-state change plus refresh loop
* generic RX/tick traffic does not trigger advert publication
* UDP NAT inbound advertisement includes rendezvous metadata
* direct UDP adverts never publish wildcard bind addresses
* discovery is a separate bounded feature for acquiring additional peers
* discovery accepts NAT-only adverts as valid candidates
* all outbound attempts use one common dial path
* `examples/nostr-bootstrap/` is removed
* tests and docs reflect the exact capability breakdown above

If you want, I can now compress this into a very terse “instructions to LLM” version suitable for dropping straight into a coding prompt.
