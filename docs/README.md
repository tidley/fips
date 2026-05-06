# FIPS Documentation

| Directory | Description |
|-----------|-------------|
| [design/](design/) | Protocol design specifications and analysis |
| [ops/](ops/) | Operational runbooks for deployed services |
| [specs/](specs/) | Stable or PoC-level wire/application protocol contracts |
| [pocs/](pocs/) | Reproducible proof-of-concept runbooks |

## Current PoC Thread

The Android-to-Pi FIPS Drop line is currently documented across:

- [design/fips-embedded-client.md](design/fips-embedded-client.md) for the
  app-service embedding model.
- [design/fips-mobile-library.md](design/fips-mobile-library.md) for the
  `crates/fips-mobile` package boundary.
- [specs/fips-drop-v0.md](specs/fips-drop-v0.md) for the binary transfer
  protocol.
- [ops/fips-drop-receiver.md](ops/fips-drop-receiver.md) for the Pi receiver
  service.
- [pocs/fips-drop-android-pi.md](pocs/fips-drop-android-pi.md) for the
  physically validated phone-to-Pi test.
