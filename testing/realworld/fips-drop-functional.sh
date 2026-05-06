#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ "${FIPS_REALWORLD:-}" != "1" ]]; then
  cat >&2 <<'EOF'
Refusing to run real-world relay/STUN test without opt-in.

This test publishes ephemeral Nostr adverts and signaling events to public
relays. Re-run with:

  FIPS_REALWORLD=1 testing/realworld/fips-drop-functional.sh
EOF
  exit 2
fi

cd "$ROOT_DIR"
exec cargo run --release --bin fips-drop-functional --features nostr-discovery -- "$@"
