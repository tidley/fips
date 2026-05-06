# FIPS Operations Runbooks

Operational documents for running FIPS services outside the developer test
harnesses.

| Document | Purpose |
| -------- | ------- |
| [fips-drop-receiver.md](fips-drop-receiver.md) | Install, configure, run, debug, and upgrade the Pi FIPS Drop receiver service. |

The FIPS Drop receiver uses the normal FIPS config and identity, listens on FSP
service port `4242`, and stores received files under `/var/lib/fips-drop` by
default.
