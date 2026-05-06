# FIPS PoC Suite

## Objective

Turn the local FIPS repo into a coherent suite of proof-of-concepts without creating a parallel framework. Future PoCs should be thin, documented scenarios layered on the existing `testing/` harnesses wherever possible.

## Suite Taxonomy

### 1. Network Core PoCs

Purpose: prove deterministic FIPS network behavior in local/lab environments.

Existing base:

- `testing/static/`
- static mesh, chain, TCP chain, rekey, public-node topology shapes

Acceptance pattern:

- topology starts cleanly
- `fipsctl show peers|links|status|transports` reports expected convergence
- all-pairs `.fips` DNS + ping succeeds where intended
- logs collected on failure

### 2. Internet Reachability PoCs

Purpose: prove discovery and connectivity across real-world network boundaries.

Existing base:

- `testing/nat/`
- scenarios: `cone`, `symmetric`, `lan`, `assist`
- Nostr discovery and peer-assisted NAT rendezvous work on the active branch

Acceptance pattern:

- endpoint discovery succeeds
- expected transport is selected, e.g. direct UDP, LAN-preferred, or `nostr-assist`
- routed ICMP/application traffic succeeds
- failure paths are visible in logs and diagnostics

### 3. Deployment Pattern PoCs

Purpose: prove FIPS can wrap real apps and deployment targets.

Existing base:

- `testing/sidecar/`
- `examples/sidecar-nostr-relay/`
- `examples/k8s-sidecar/`
- `examples/wireguard-sidecar-macos/`
- `testing/static/scripts/gateway-test.sh`

Acceptance pattern:

- app/service only reachable through intended FIPS path
- gateway/sidecar policies are enforced
- DNS, nftables/firewall, and forwarding assertions pass where relevant

### 4. Product PoCs

Purpose: prove user-facing value built on FIPS.

First track:

- Pushstr-style FIPS Drop: Android phone -> FIPS network -> Pi4ssd Blossom storage
- private Pi4/Pi4ssd admin/status page over FIPS
- WireGuard/VPS replacement slice for selected private services

Existing base:

- `.planning/pushstr-fips-dropbox-plan.md`

Acceptance pattern:

- phone/laptop reaches target service over FIPS with WireGuard disabled
- upload/download works over FIPS
- service is not exposed directly to public internet unless explicitly intended
- LNVPS can be absent after peers are established for local/private access tests

### 5. Resilience / Operations PoCs

Purpose: prove operational behavior under degraded conditions.

Existing base:

- `testing/chaos/`
- Pi relay redundancy plan in `.planning/pushstr-fips-dropbox-plan.md`

Acceptance pattern:

- scenario has documented failure injected
- expected degradation/recovery is observed
- logs and test summary are preserved
- rollback or operator action is documented

## PoC Authoring Rule

Prefer adding a scenario to an existing harness over creating a new one.

Use:

- `testing/static/configs/topologies/` for deterministic local topologies
- `testing/chaos/scenarios/` for churn, routing, traffic, netem, and convergence behavior
- `testing/nat/` for NAT, rendezvous, peer-assist, and public/private endpoint behavior
- `testing/sidecar/` or `examples/*sidecar*` for deployment pattern demos
- `testing/pocs/<name>/README.md` only when a PoC spans multiple harnesses or needs an operator story wrapper

Each PoC must document:

- objective
- topology
- exact run command
- privilege/platform requirements
- acceptance assertions
- expected artifacts/log locations
- cleanup/rollback notes

## Initial Build Order

1. **PoC smoke template**
   - Base: `testing/static/`
   - Small 3-node or 5-node deterministic topology.
   - One documented command that builds, starts, waits for convergence, asserts `.fips` DNS + ping, and collects logs on failure.
   - Goal: copyable golden pattern for future PoCs.

2. **Gateway PoC promotion**
   - Base: `testing/static/scripts/gateway-test.sh`
   - Add README/operator wrapper, explicit acceptance checklist, and artifact notes.
   - Goal: prove non-FIPS LAN client -> gateway -> FIPS mesh HTTP service.

3. **Peer-assist/NAT PoC wrapper**
   - Base: `testing/nat assist`
   - Add demo/operator story around the existing assertions.
   - Goal: show peer-assisted rendezvous as the flagship reachability PoC.

4. **Pi4ssd Blossom Dropbox PoC**
   - Base: `.planning/pushstr-fips-dropbox-plan.md`
   - Start with curl/upload/download over FIPS before Android integration.
   - Goal: first product PoC and first WireGuard/VPS replacement slice.

## Known Risks

- Current worktree has pre-existing dirty state:
  - deleted `.planning/peer-assisted-core-34e00b9-to-3eaf9ac.diff`
  - untracked `.planning/pushstr-fips-dropbox-plan.md`
- Ignored local key/config files exist in repo root; never use those directly in PoC docs or commits.
- The NAT harness is privileged/manual-sensitive and may depend on Docker/netns/iptables runner behavior.
- Pushstr/FIPS Android work can sprawl; prove the product workflow with separate FIPS connectivity first, embed FIPS mobile later.
- Multi-daemon Pi identities improve isolation but add operational complexity; introduce only at meaningful trust boundaries.
