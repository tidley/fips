# FIPS Drop Physical Test

Goal: Android Pushstr sends one blob over embedded FIPS to Pi4ssd/Pi, and the receiver writes it under a test storage root.

Result: validated on 2026-05-05 with phone -> Pi over both Wi-Fi and 4G.

## Artifacts

- Pi receiver binary for Pi4/Pi3 arm64: `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-drop-agent`
- Android APK: `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`

Pi receiver build details:

- Target: `aarch64-unknown-linux-gnu`
- Features: `--no-default-features --features nostr-discovery`
- Size before strip: about `17M`
- SHA256: `81a0c97e3187905b18e3408cdd3f3b6bd2a4a3327244df9bb3fd268053224dfb`

## Pi4ssd Receiver

Copy/install the receiver binary onto Pi4ssd:

```bash
scp /home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-drop-agent \
  fips@pi4ssd:/tmp/fips-drop-agent

ssh fips@pi4ssd
sudo install -m 0755 /tmp/fips-drop-agent /usr/local/bin/fips-drop-agent
sudo install -d -m 0755 /var/lib/fips-drop
```

Use a config shaped like this:

```yaml
node:
  identity:
    persistent: true
  discovery:
    nostr:
      enabled: true
      policy: configured_only
      advertise: true
      advert_relays:
        - "wss://relay.damus.io"
        - "wss://nos.lol"
        - "wss://offchain.pub"
      dm_relays:
        - "wss://relay.damus.io"
        - "wss://nos.lol"
        - "wss://offchain.pub"
      stun_servers:
        - "stun:stun.l.google.com:19302"
        - "stun:stun.cloudflare.com:3478"
        - "stun:global.stun.twilio.com:3478"

tun:
  enabled: false

dns:
  enabled: false

transports:
  udp:
    bind_addr: "0.0.0.0:0"
    advertise_on_nostr: true
    public: false

peers: []
```

Run it manually first:

```bash
sudo -u fips RUST_LOG="info,fips::discovery::nostr=debug" \
  /usr/local/bin/fips-drop-agent \
  --config /etc/fips/fips.yaml \
  --storage-root /var/lib/fips-drop \
  --port 4242
```

Keep the Pi4ssd npub from the startup log.

If the normal `fips.service` uses the same config/identity, stop it before
manual receiver tests:

```bash
sudo systemctl stop fips
```

## Android

Install the APK:

```bash
adb install -r /home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk
```

In Pushstr:

1. Open the drawer.
2. Open `FIPS Drop`.
3. Paste the Pi4ssd npub.
4. Tap `Start`.
5. Tap `Connect`.
6. Pick a small file.
7. Tap `Send`.

Expected:

- Pi4ssd logs a service packet on port `4242`.
- The blob appears below `/var/lib/fips-drop`.
- Android receives an ACK/ERROR response over the FIPS service path.

For the validated 3 MiB video test, the current conservative sender profile
logs normal service payloads around `796` bytes and stores:

```text
/var/lib/fips-drop/VID-20260505-WA0003.mp4
```

See the durable runbook at `docs/pocs/fips-drop-android-pi.md`.
