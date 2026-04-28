#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ROOT_DIR="$(cd "$NAT_DIR/../.." && pwd)"
DERIVE_KEYS="$ROOT_DIR/testing/lib/derive_keys.py"
OUTPUT_DIR="$NAT_DIR/generated-configs"
SCENARIO="${1:?usage: generate-configs.sh <cone|symmetric|lan|assist> [mesh-name]}"
MESH_NAME="${2:-nat-lab-$(date +%s)-$$}"

case "$SCENARIO" in
    cone|symmetric|lan|assist) ;;
    *)
        echo "Unknown scenario: $SCENARIO" >&2
        exit 1
        ;;
esac

mkdir -p "$OUTPUT_DIR/$SCENARIO"

keys_a="$(python3 "$DERIVE_KEYS" "$MESH_NAME" "a")"
keys_b="$(python3 "$DERIVE_KEYS" "$MESH_NAME" "b")"
keys_c=""
keys_d=""
if [ "$SCENARIO" = "assist" ]; then
    keys_c="$(python3 "$DERIVE_KEYS" "$MESH_NAME" "c")"
    keys_d="$(python3 "$DERIVE_KEYS" "$MESH_NAME" "d")"
fi

nsec_a="$(echo "$keys_a" | awk -F= '/^nsec=/{print $2}')"
npub_a="$(echo "$keys_a" | awk -F= '/^npub=/{print $2}')"
nsec_b="$(echo "$keys_b" | awk -F= '/^nsec=/{print $2}')"
npub_b="$(echo "$keys_b" | awk -F= '/^npub=/{print $2}')"
nsec_c="$(echo "$keys_c" | awk -F= '/^nsec=/{print $2}')"
npub_c="$(echo "$keys_c" | awk -F= '/^npub=/{print $2}')"
nsec_d="$(echo "$keys_d" | awk -F= '/^nsec=/{print $2}')"
npub_d="$(echo "$keys_d" | awk -F= '/^npub=/{print $2}')"

relay_addr="ws://172.31.254.30:7777"
stun_addr="stun:172.31.254.40:3478"
if [ "$SCENARIO" = "lan" ]; then
    relay_addr="ws://172.31.10.30:7777"
    stun_addr="stun:172.31.10.40:3478"
fi

advert_refresh_secs=20
retry_base_interval_secs=2
retry_max_backoff_secs=8
if [ "$SCENARIO" = "assist" ]; then
    advert_refresh_secs=3
    retry_base_interval_secs=1
    retry_max_backoff_secs=4
fi

peer_block_a=$(cat <<EOF
  - npub: "$npub_b"
    alias: "node-b"
    addresses:
      - transport: udp
        addr: "nat"
        priority: 1
    connect_policy: auto_connect
    auto_reconnect: true
EOF
)

peer_block_b=$(cat <<EOF
  - npub: "$npub_a"
    alias: "node-a"
    addresses:
      - transport: udp
        addr: "nat"
        priority: 1
    connect_policy: auto_connect
    auto_reconnect: true
EOF
)

peer_block_c=$(cat <<EOF
  - npub: "$npub_b"
    alias: "node-b"
    addresses:
      - transport: udp
        addr: "nat"
        priority: 1
    connect_policy: auto_connect
    auto_reconnect: true
  - npub: "$npub_d"
    alias: "node-d"
    addresses:
      - transport: udp
        addr: "nat"
        priority: 1
    connect_policy: auto_connect
    auto_reconnect: true
EOF
)

peer_block_d=$(cat <<EOF
  - npub: "$npub_c"
    alias: "node-c"
    addresses:
      - transport: udp
        addr: "nat"
        priority: 1
    connect_policy: auto_connect
    auto_reconnect: true
EOF
)

if [ "$SCENARIO" = "symmetric" ]; then
    peer_block_a="$peer_block_a"$'\n'"      - transport: tcp
        addr: \"172.31.254.11:8443\"
        priority: 20"
    peer_block_b="$peer_block_b"$'\n'"      - transport: tcp
        addr: \"172.31.254.10:8443\"
        priority: 20"
fi

write_config() {
    local output_file="$1"
    local nsec="$2"
    local peer_block="$3"
    local stun_mode="$4"
    local enable_peer_assist="$5"
    local nostr_advertise="$6"
    local udp_advertise="$7"

    local stun_block=""
    local peer_assist_block=""
    local udp_peer_assist="false"

    if [ "$stun_mode" = "default" ]; then
        stun_block=$(cat <<EOF
      stun_servers:
        - "$stun_addr"
EOF
)
    elif [ "$stun_mode" = "empty" ]; then
        stun_block='      stun_servers: []'
    fi

    if [ "$enable_peer_assist" = "true" ]; then
        peer_assist_block=$(cat <<EOF
      peer_assist:
        mode: fallback_private
        request_policy: open_rate_limited
EOF
)
        udp_peer_assist="true"
    fi

    cat > "$output_file" <<EOF
node:
  identity:
    nsec: "$nsec"
  retry:
    max_retries: 3
    base_interval_secs: $retry_base_interval_secs
    max_backoff_secs: $retry_max_backoff_secs
  discovery:
    nostr:
      enabled: true
      advertise: $nostr_advertise
      app: "fips.nat.lab.v1"
      advert_relays:
        - "$relay_addr"
      dm_relays:
        - "$relay_addr"
$stun_block
$peer_assist_block
      signal_ttl_secs: 30
      policy: configured_only
      attempt_timeout_secs: 6
      replay_window_secs: 60
      punch_start_delay_ms: 500
      punch_interval_ms: 100
      punch_duration_ms: 2500
      advert_ttl_secs: 60
      advert_refresh_secs: $advert_refresh_secs

tun:
  enabled: true
  name: fips0
  mtu: 1280

dns:
  enabled: true
  bind_addr: "127.0.0.1"
  port: 5354

transports:
  udp:
    bind_addr: "0.0.0.0:2121"
    mtu: 1472
    advertise_on_nostr: $udp_advertise
    public: false
    peer_assist: $udp_peer_assist
  tcp:
    bind_addr: "0.0.0.0:8443"

peers:
$peer_block
EOF
}

write_config "$OUTPUT_DIR/$SCENARIO/node-a.yaml" "$nsec_a" "$peer_block_a" default "$([ "$SCENARIO" = "assist" ] && echo true || echo false)" true true
write_config "$OUTPUT_DIR/$SCENARIO/node-b.yaml" "$nsec_b" "$peer_block_b" "$([ "$SCENARIO" = "assist" ] && echo empty || echo default)" "$([ "$SCENARIO" = "assist" ] && echo true || echo false)" true true

if [ "$SCENARIO" = "assist" ]; then
    write_config "$OUTPUT_DIR/$SCENARIO/node-c.yaml" "$nsec_c" "$peer_block_c" empty true true true
    write_config "$OUTPUT_DIR/$SCENARIO/node-d.yaml" "$nsec_d" "$peer_block_d" empty true true true
fi

cat > "$OUTPUT_DIR/$SCENARIO/npubs.env" <<EOF
NPUB_A=$npub_a
NPUB_B=$npub_b
NPUB_C=$npub_c
NPUB_D=$npub_d
MESH_NAME=$MESH_NAME
SCENARIO=$SCENARIO
EOF

echo "Generated NAT lab configs for scenario=$SCENARIO mesh=$MESH_NAME"
echo "NPUB_A=$npub_a"
echo "NPUB_B=$npub_b"
if [ -n "$npub_c" ]; then
    echo "NPUB_C=$npub_c"
fi
if [ -n "$npub_d" ]; then
    echo "NPUB_D=$npub_d"
fi
