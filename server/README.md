# VIPTV server

Rust/Axum, SQLite, account-owned Xtream and Stremio-compatible addons. Requires
Rust 1.90+, a C compiler for bundled SQLite at build time, and trusted HTTPS
ingress for clients. The backend does not run FFmpeg, probe media, expose a
generic media relay or manage GPU devices. The separate playback gateway owns
media processing and viewer capabilities.

Build with `cargo build --release --locked`; check with `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`.
See ../tests/README.md for disposable network-none gateway acceptance.

## Configuration

| Variable | Default / purpose |
| --- | --- |
| VIPTV_BIND | 0.0.0.0:8080 |
| VIPTV_AUTH_ORIGIN | One exact public HTTPS origin, required for normal public browser deployment |
| VIPTV_DATABASE | data/viptv.sqlite |
| VIPTV_DASHBOARD_DIST | Optional account/admin assets at / |
| VIPTV_TV_DIST | Optional viewing assets at /tv |
| VIPTV_SECRETS_KEYRING | Operator-managed encrypted source/gateway storage; no insecure default |

The Compose template passes a configured keyring through and leaves an absent
value unresolved/omitted, rather than inventing empty JSON. Keep real keyrings,
deployment env, database copies and exports private. Preserve the existing
named data volume. Backend media/GPU/WARP/session-limit environment settings
are retired; keep historical private deployment copies for operator review,
not as active backend hooks.

## Contracts and ownership

Authentication, account/profile IDs, favorites, progress, Continue Watching,
parent policy, manual VOD matches and addon metadata remain backend-owned.
See AUTH.md, ../docs/V2_OPERATIONS.md, ../docs/ADDONS_V2.md and
../docs/CATALOGS_V2.md. /api/catalogs, /api/discover and /api/meta remain active;
source discovery uses /api/v2/streams. Sources are opaque and caller-scoped.

Ordinary raw live viewing uses cursor-bound /api/v2/iptv/live/channels and
categories, native Xtream guide reads and exact-channel source resolution.
Preserve provider ordering/IDs/logos; never reconstruct a full client playlist.
There is no generic M3U/external XMLTV import or automatic family remapping.

Playback uses /api/v2/playback start/status/heartbeat/release. Only account-
authorized gateways are eligible; Roku/Vizio require one. Other eligible native
clients may receive direct source URLs/headers deliberately after authorization.
Actual decoder limits remain; profile maximum quality is retired. The backend
never falls back to embedded execution or another account's gateway.

Serve frontend/API control on the same HTTPS origin. The watch reverse proxy's
Host/Origin rewrites satisfy the single-origin policy and must remain. Media
URLs point to the authorized delivery endpoint, not a backend relay. Do not
disable TLS validation or restore obsolete SSE/media proxy settings.

## Migration and evidence

No runtime boot performs destructive retirement or infers provider ownership.
Explicit ownership/encryption and backup-first offline retirement are documented
in ../docs/RUNTIME_RETIREMENT.md. Active legacy routed connections refuse
retirement until operator review; archived routing must be reviewed before
re-enablement. Old protocol namespaces return client_update_required.

Host tests, client mocks and isolated gateway media fixtures have separate
evidence limits. Production backup/migration/rollback, real providers and physical
TV/4K/tracks remain separately qualified operations.
