# Delivery and migration

Backend CI validates fmt, Clippy, backend tests including real FFmpeg, the pinned web bundle, Docker build and isolated container acceptance. Web CI independently validates its source tests/build. Tags beginning with v publish the exact tested backend image to ghcr.io/viptv-org/backend using the source SHA and release tag, plus digest metadata in a GitHub release. Pin production to the digest. Main pushes validate only.

## Private web dependency
GitHub rejected a scoped deploy key with `Deploy keys are disabled for this repository`. Rather than store an account-wide credential, backend commits the built public web assets in web-dist/, with every file checksummed in WEB_BUNDLE.json and its exact source revision pinned at the dashboard git submodule. These are distributable JS/CSS/HTML assets, not credentials. Backend CI can validate and build without private cross-repository checkout access. The original web app source lives only in its web repository.

To update: initialize dashboard with your normal authenticated GitHub access, check out the reviewed web commit, then run `python3 scripts/update-web.py`. This requires a clean web source tree, runs npm ci/tests/build, refreshes the bundle, and records hashes. Commit dashboard, WEB_BUNDLE.json and web-dist together. CI rejects mismatched pins, extra/missing files or changed hashes. Review the linked web CI run. This explicit promotion step can later be replaced by a scoped GitHub App or approved package access; no such credential was created.

## Production
Production has not been redeployed by the repository split. The health endpoint was unreachable from the migration environment, so live status is unverified. Follow DEPLOYMENT.md only after a fresh live-state check and consistent private backup. Retain the current .env and all Compose overlays (compose.yaml, compose.warp.yaml, compose.qsv.yaml), account/profile/provider/addon/history data, render GID and named volume viptv_viptv_data. Compose now names that volume explicitly, avoiding accidental empty data when the checkout directory changes. The old monorepo is retained for history and rollback.

Automated production promotion is separate from artifact publication: it needs production reachability and guarded data/session checks. This extraction does not restart the household server.
