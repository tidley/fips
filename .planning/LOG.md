## 2026-04-28 11:27 UTC
- Created PR readiness TODO at `.planning/PR-READINESS-TODO.md` and populated NOW/NEXT/BACKLOG for peer-assisted rendezvous PR cleanup.

## 2026-04-28 11:32 UTC
- Audited branch tip for secret material (`fips.key`, `fips.pub`, `fips.yaml`, `git grep nsec`).
- Removed tracked local key/config files from index: `fips.key`, `fips.pub`, `fips.yaml`.
- Restored/expanded `.gitignore` safety for local/private files: `AGENTS.md`, `CLAUDE.md`, `agents/`, plus `fips.key`, `fips.pub`, `fips.yaml`.
- Updated CI unit-test lane to include `nostr-discovery` feature: `cargo nextest run --all --profile ci --features "gateway nostr-discovery"`.
- Recorded key-history exposure risk in `.planning/RISKS.md`; no history rewrite performed.
- Documented NAT harness CI/manual strategy in `testing/nat/README.md` (manual/nightly-only, privileged netns/iptables rationale, explicit `assist` run command).

## 2026-04-28 PR prep continuation
- Documented `node.discovery.nostr.peer_assist.*` in `docs/design/fips-configuration.md` with a safe chained-onboarding example.
- Added commented, disabled-by-default `peer_assist` sample config in `packaging/common/fips.yaml`.
- Added per-sender/window incoming traversal-offer rate limiting before offer worker dispatch.
- Added protocol/rate-limit tests for assist request/grant expiry, grant mismatch, invalid helper endpoint, rejected grant path, and rate-window behavior.
- Added peer-assisted rendezvous changelog entry and `.planning/PR-DESCRIPTION.md` draft.
- Verification passed:
  - `cargo fmt --all -- --check`
  - `cargo clippy --features "gateway nostr-discovery" --all-targets -- -D warnings`
  - `cargo test --features "gateway nostr-discovery"`
  - `./testing/nat/scripts/nat-test.sh assist`
