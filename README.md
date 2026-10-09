# viptv backend

Actions delivery: main pushes and manual builds produce sideloading artifacts
(Android universal APK; desktop Windows/Linux installers; Roku ZIP; TV WGT/IPK).
Other repositories have no Actions workflows. Local checks remain; previous
CI/release-publication descriptions below are historical. No automatic deploys.

Extracted from `vynxc/viptv@7d6b413`. `MIGRATION.json` records every original file and SHA-256; the original repository retains history. This repository owns the Rust backend and deployment packaging.

The [design repository](https://github.com/viptv-org/design) is the product source of truth. Read [SPEC.md](SPEC.md), [AGENTS.md](AGENTS.md), and the pinned `DESIGN_REF` before implementation. Future platform work must inherit its interaction contracts.


## License

Copyright (C) 2026 viptv contributors.

This program is free software; you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation; version 2 of the License. See [LICENSE](LICENSE). The playback adapters (`viptv-org/video`, `viptv-org/tauri-video-plugin`) and the Android repository remain under their existing MIT OR Apache-2.0 terms.
## Optional Monitoring

Backend and gateway support opt-in OpenTelemetry export to New Relic; no Roku
agent or application update is required. `OBSERVABILITY_ENABLED=false` is the
default. Set an ingest-only `NEW_RELIC_LICENSE_KEY` privately, enable monitoring,
and retain the persistent data volume containing the daily byte-budget ledger.
See [the shared exporter documentation](server/shared/service-telemetry/README.md)
for limits, field allowlists, sampling, shutdown and failure behavior. Export
failure never changes playback outcomes. Free-only deployments must retain a
free vendor plan; byte limits are not vendor billing guarantees.
