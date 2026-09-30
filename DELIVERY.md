# Delivery and migration

Owner policy: backend has no automatic image publication, release or deployment
workflow. Retain local Rust, frontend, static configuration and isolated gateway
checks. Android/desktop/Roku/TV-web owning repositories provide their build artifacts.

## Frontend source pins

The dashboard at / and viewing bundle at /tv build from reviewed dashboard/tv
gitlinks. Initialize those submodules, verify their owning source tests and build
the candidate image. FRONTENDS=0 produces a backend-only image for local checks;
it does not qualify a production frontend package or imply that assets are live.

## Runtime and production boundary

The backend is a control plane: no embedded FFmpeg/GPU/media relay/WARP sidecar.
Independent gateway settings and private operator environment remain separately
owned. Preserve viptv_viptv_data, account/profile/history/provider/addon IDs,
keyrings and exact HTTPS origin. Keep historical private deployment overlays/
env for review and rollback; removed public overlays must not be reapplied as
active backend configuration.

Production promotion, explicit ownership/encryption/retirement and rollback
are separately approved. Follow DEPLOYMENT.md and docs/RUNTIME_RETIREMENT.md
only after reviewing actual current state and durable private backups. Source
commits, local image builds and fixture passes do not establish a live deploy.
