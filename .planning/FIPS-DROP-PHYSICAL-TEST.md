# FIPS Drop Physical Test

Goal: Android Pushstr sends one small blob over embedded FIPS to Pi4ssd, and Pi4ssd writes it under a test storage root.

## Artifacts

- Pi receiver binary for Pi4/Pi3 arm64: `/home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent`
- Android APK: `/home/tom/code/pushstr/mobile/build/app/outputs/flutter-apk/app-debug.apk`

Pi receiver build details:

- Target: `aarch64-unknown-linux-gnu`
- Features: `--no-default-features --features nostr-discovery`
- Size after strip: about `13M`
- SHA256: `8709f03ad5afe1a9f85f1aaca8d356c68d7ecc671e37cb92297a8bdf434278c3`

## Pi4ssd Receiver

Copy/install the receiver binary onto Pi4ssd:

```bash
scp /home/tom/code/fips/target/aarch64-unknown-linux-gnu/release/fips-dropbox-agent \
  fips@pi4ssd:/tmp/fips-dropbox-agent

ssh fips@pi4ssd
sudo install -m 0755 /tmp/fips-dropbox-agent /usr/local/bin/fips-dropbox-agent
sudo install -d -m 0750 -o fips -g fips /var/lib/fips-dropbox
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
  /usr/local/bin/fips-dropbox-agent \
  --config /etc/fips/fips.yaml \
  --storage-root /var/lib/fips-dropbox \
  --port 4242
```

Keep the Pi4ssd npub from the startup log.

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
- The blob appears below `/var/lib/fips-dropbox`.
- Android receives an ACK/ERROR response over the FIPS service path.
