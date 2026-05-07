# PR Readiness TODO — Peer-Assisted NAT Rendezvous

Objective: make `feature/peer-assisted-rendezvous` upstream-quality and ready to open as a high-confidence PR.

## NOW

- [x] **Remove committed FIPS key material**
  - Check whether `fips.key` and `fips.pub` are tracked on this branch.
  - If tracked, remove them from the branch and add/restore ignores as appropriate.
  - Search for secret material with `git grep "nsec"` and equivalent history/diff checks.
  - If `fips.key` was ever usable, rewrite branch history or treat the key as compromised before PR.
  - Acceptance: PR diff contains no private key material; `git grep "nsec"` returns no usable secret.

- [x] **Restore `.gitignore` safety**
  - Review `.gitignore` diff against `origin/master`.
  - Restore ignores for local/private agent files unless intentionally removed: `AGENTS.md`, `CLAUDE.md`, `agents/`.
  - Acceptance: local assistant/project-private files cannot be accidentally committed.

- [x] **Add CI lane for `nostr-discovery` tests**
  - Update `.github/workflows/ci.yml` so CI runs tests with `nostr-discovery` enabled.
  - Preferred command: `cargo test --features "nostr-discovery"` or repo-standard `nextest` equivalent.
  - Acceptance: tests gated by `#[cfg(feature = "nostr-discovery")]` run in CI.

## NEXT

- [x] **Define NAT harness CI strategy**
  - Choose one path:
    - run `testing/nat/scripts/nat-test.sh assist` in CI,
    - add a manual/nightly GitHub workflow,
    - or explicitly document the NAT lab as privileged/manual-only.
  - If manual-only, document the reason in `testing/nat/README.md`.
  - Acceptance: reviewers can see exactly how the NAT lab is validated.

- [x] **Document `peer_assist` config**
  - Update `docs/design/fips-configuration.md`.
  - Document: `dial_mode`, `grant_ttl_secs`, `helper.enabled`, `helper.request_policy`, `helper.request_allowlist`, `helper.max_pending_requests`, `helper.max_requests_per_peer_per_window`, `helper.request_window_secs`, `max_offers_per_peer_per_window`, `offer_window_secs`.
  - Include a safe chained-onboarding example.
  - Acceptance: an operator can configure peer assist safely from docs alone.

- [x] **Update package/sample config**
  - Review `packaging/common/fips.yaml`.
  - Add safe commented `peer_assist` example if useful.
  - Do not silently enable risky defaults.
  - Acceptance: packaged config aligns with documented defaults and does not surprise operators.

- [x] **Harden incoming traversal offer rate limiting**
  - Add per-sender/window rate limiting for incoming traversal offers before expensive STUN/punch work.
  - Mirror the peer-assist request rate-limit style where possible.
  - Add tests for allowed request, over-limit rejection, and expired-window reset.
  - Acceptance: a malicious sender cannot force unbounded traversal work.

## BACKLOG

- [x] **Add/verify protocol validation tests**
  - Cover expired assist request, expired grant, mismatched sender/recipient, replayed request/session ID, invalid helper endpoint, and rejected grant path.
  - Acceptance: protocol misuse is rejected with deterministic test coverage.

- [x] **Add changelog entry**
  - Update `CHANGELOG.md`.
  - Mention Nostr peer-assisted UDP rendezvous, chained onboarding, NAT lab assist scenario, and config knobs.
  - Acceptance: release notes explain feature and operator impact.

- [x] **Run full local verification from clean tree**
  - Required commands:
    - `cargo fmt --all -- --check`
    - `cargo clippy --all-targets -- -D warnings`
    - `cargo test --features "nostr-discovery"`
    - `./testing/nat/scripts/nat-test.sh assist`
  - Acceptance: all commands pass from a clean checkout.

- [x] **Prepare PR description**
  - Include problem statement, design summary, security model, config examples, test evidence, and known limitations.
  - Specifically mention symmetric NAT/manual NAT lab constraints if still applicable.
  - Acceptance: reviewer can understand feature and risk profile without reconstructing from commits.

- [ ] **Final branch hygiene**
  - Ensure branch is up to date with `origin/master`.
  - Ensure no generated NAT configs or local artifacts are tracked.
  - Ensure `git status --short` is empty.
  - Push final commits.
  - Acceptance: PR diff contains only intentional source/docs/CI changes.
