# Delivery and migration

Backend CI validates fmt, Clippy, backend tests including real FFmpeg, a server-only Docker build and isolated container acceptance. Web CI independently validates its source tests/build, and tv-web CI validates the TV bundle the same way. Tags beginning with v publish the exact tested backend image to ghcr.io/viptv-org/backend using the source SHA and release tag, plus digest metadata in a GitHub release. Pin production to the digest. Main pushes validate only.

## Frontend bundles

The dashboard (`/`) and the TV bundle (`/tv`, the Tizen/Vizio viewing client) are built from the `dashboard` and `tv` git submodules inside the Docker image; no built artifacts are committed to this repository. The gitlinks are the promotion step: initialize the submodules with your normal authenticated GitHub access, check out the reviewed web/tv-web commits, commit the gitlinks together, and build the image. CI cannot clone the private sibling repositories, so it builds a server-only image (`--build-arg FRONTENDS=0`); the empty `VIPTV_DASHBOARD_DIST`/`VIPTV_TV_DIST` values used by the validation compose disable the mounts and the server answers `/` with its identity. Frontend source validation stays in each frontend repository's own CI; review those runs when moving a gitlink.

## Production
Production has not been redeployed by the repository split. The health endpoint was unreachable from the migration environment, so live status is unverified. Follow DEPLOYMENT.md only after a fresh live-state check and consistent private backup. Retain the current .env and all Compose overlays (compose.yaml, compose.warp.yaml, compose.qsv.yaml), account/profile/provider/addon/history data, render GID and named volume viptv_viptv_data. Compose now names that volume explicitly, avoiding accidental empty data when the checkout directory changes. The old monorepo is retained for history and rollback.

Automated production promotion is separate from artifact publication: it needs production reachability and guarded data/session checks. This extraction does not restart the household server.
