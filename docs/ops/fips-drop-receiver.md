# FIPS Drop Receiver Operations

This runbook turns the validated Android-to-Pi PoC into a normal Pi receiver
service.

## Install

Build or copy the arm64 binary to the Pi:

```bash
sudo install -m 0755 /tmp/fips-drop-agent /usr/local/bin/fips-drop-agent
```

Install the unit and storage directory:

```bash
sudo install -m 0644 packaging/systemd/fips-drop.service /etc/systemd/system/fips-drop.service
sudo install -m 0644 packaging/systemd/fips-drop.env.example /etc/default/fips-drop
sudo install -m 0644 packaging/systemd/fips-drop.tmpfiles /etc/tmpfiles.d/fips-drop.conf
sudo systemd-tmpfiles --create /etc/tmpfiles.d/fips-drop.conf
sudo systemctl daemon-reload
```

The default storage path is `/var/lib/fips-drop`.

## Configure

The receiver uses the normal FIPS config and identity:

```text
/etc/fips/fips.yaml
```

Keep a stable identity in that config so the Android app can target one durable
Pi `npub`.

Optional receiver overrides live in:

```text
/etc/default/fips-drop
```

Supported overrides:

- `FIPS_DROP_CONFIG`
- `FIPS_DROP_STORAGE_ROOT`
- `FIPS_DROP_PORT`
- `RUST_LOG`

## Run

```bash
sudo systemctl enable --now fips-drop.service
sudo journalctl -u fips-drop -f
```

For clean PoC tests, do not run another `fips.service` instance with the same
identity at the same time:

```bash
sudo systemctl stop fips.service
```

## Expected Logs

Startup:

```text
FIPS Drop agent starting
FIPS Drop agent node created:
FIPS Drop agent running
```

Connection:

```text
traversal: responder punch succeeded
Adopted NAT traversal socket
Peer promoted to active
Session established
```

Transfer:

```text
FIPS Drop service packet received
FIPS Drop binary blob chunk received
FIPS Drop binary blob stored
```

## Debug Mode

Run manually when diagnosing bootstrap, NAT, or repair behavior:

```bash
sudo RUST_LOG="debug,fips::discovery::nostr=trace" \
  /usr/local/bin/fips-drop-agent \
  --config /etc/fips/fips.yaml \
  --storage-root /var/lib/fips-drop \
  --port 4242
```

## Upgrade

```bash
sudo systemctl stop fips-drop.service
sudo install -m 0755 /tmp/fips-drop-agent /usr/local/bin/fips-drop-agent
sudo systemctl start fips-drop.service
sudo journalctl -u fips-drop -n 80 --no-pager
```

## Retrieve Files

```bash
sudo find /var/lib/fips-drop -maxdepth 1 -type f -ls
scp 'pi4fips@192.168.1.147:/var/lib/fips-drop/example.jpg' /tmp/
```

If SSH user permissions block direct `scp`, stage a copy:

```bash
sudo cp /var/lib/fips-drop/example.jpg /home/pi4fips/
sudo chown pi4fips:pi4fips /home/pi4fips/example.jpg
```
