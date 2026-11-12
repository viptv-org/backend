# viptv backend

Actions runs the backend checks on pull requests into `edge` and `main` and on
pushes to `edge`. The manual **backend release artifact** workflow builds an
exact full commit matching the selected `main` or `edge` head with Rust1.98.0,
locked dependencies and `live-runtime` plus the default SDK runtime enabled.
Dispatch it only after that source's required checks pass. Its seven-day
download contains an executable tar, source/tree/hash manifest and checksums;
the tar preserves executable permissions. Ubuntu22.04 and an ELF GLIBC2.35
ceiling keep the binary compatible with the Bookworm runtime. Verify the
manifest and checksums before using it in a manually built server image
(`Dockerfile`, `compose.yaml`). No automatic deploys.

This repository owns the Rust backend and deployment packaging.

The [design repository](https://github.com/viptv-org/design) is the product source of truth. Read [SPEC.md](SPEC.md), [AGENTS.md](AGENTS.md), and the pinned `DESIGN_REF` before implementation. Future platform work must inherit its interaction contracts.


## License

Copyright (C) 2026 viptv contributors.

This program is free software; you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation; version 2 of the License. See [LICENSE](LICENSE). The playback adapters (`viptv-org/video`, `viptv-org/tauri-video-plugin`) and the Android repository remain under their existing MIT OR Apache-2.0 terms.
## Optional Monitoring

Backend and gateway support opt-in OpenTelemetry export to New Relic; no
client agent or application update is required. `OBSERVABILITY_ENABLED=false` is the
default. Set an ingest-only `NEW_RELIC_LICENSE_KEY` privately, enable monitoring,
and retain the persistent data volume containing the daily byte-budget ledger.
See [the shared exporter documentation](server/shared/service-telemetry/README.md)
for limits, field allowlists, sampling, shutdown and failure behavior. Export
failure never changes playback outcomes. Free-only deployments must retain a
free vendor plan; byte limits are not vendor billing guarantees.
