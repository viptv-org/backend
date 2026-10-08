# Production deployment and migration

This document separates current v2 operations from historical rollout evidence.
No source checkpoint or fixture authorizes a production change.

## Current deployment boundary

Production access, account credentials, keyrings, backups and rollback material
remain private. The backend runs UID/GID10001, read-only, cap_drop ALL and
no-new-privileges. /data retains SQLite in viptv_viptv_data; /tmp is bounded.
FFmpeg, GPU devices, WARP sidecars and generated media storage belong outside
this backend's active packaging.

Configure one exact public HTTPS VIPTV_AUTH_ORIGIN. Preserve the watch proxy's
Host/Origin rewrites; two hostnames are not independent backend API origins.
Retain existing private environment/overlays for review and rollback, but do not
blindly reapply removed public media/GPU/WARP overlays to this candidate.
Provision and securely retain VIPTV_SECRETS_KEYRING separately from backups.
No default key material or shared application credential is shipped.

Fresh owner creation is an explicit offline operation. Existing accounts,
profiles and histories must not be reseeded or replaced.

## Separately approved upgrade checklist

1. Review exact backend/gateway/frontend/client revisions and confirm every
   ordinary media/catalog client has adopted v2, including the Android
   handoff. Retired endpoints explicitly require a client update.
2. Coordinate downtime and inspect authenticated v2 viewer state/known clients;
   the removed /api/status is not an admission or maintenance oracle.
3. Preserve current private environment, keyring and a consistent SQLite online
   backup with checksum, stable IDs and representative history values. Never
   raw-copy an active DB or overwrite newer history from an older backup.
4. Review explicit ownership and encrypted source migration, then the offline
   backup/export retirement phase in docs/RUNTIME_RETIREMENT.md. Enabled legacy
   routed sources refuse retirement; disabling/archive and later re-enablement
   require operator routing review. No owner is inferred.
5. Build/test an isolated candidate and copied-data fixture before any rollout.
   Initialize reviewed dashboard/tv gitlinks for a full frontend image. Keep
   origin/keyring/provider/gateway policy and viptv_viptv_data intact.
6. Run local Rust/configuration checks and the network-none gateway fixture.
   Verify public TLS/redirect behavior, actual gateway/source integration and
   physical device/4K/tracks separately from mocks.
7. Replace production only under explicit approval. Inspect actual deployment
   status, served frontend asset hashes, unauthorized API rejection and existing
   account/profile behavior; do not infer live success from build output.
8. Compare preserved IDs/history/positions afterwards. Keep current recovery
   artifacts protected through the observation window.

Never run docker compose down -v against production. Rollback needs stopped
writers, a fresh preservation snapshot of current data and reviewed compatibility.
Old globally scoped/family/media code cannot simply be restored onto a retired
multi-tenant database.

## Local checks

```sh
node tests/validate_deployment.cjs
python3 tests/test_host_check.py
bash scripts/host-check.sh
bash scripts/container-check.sh LOCAL_GATEWAY_IMAGE
```

host-check is read-only and suppresses rendered secret configuration. The
container check uses uniquely disposable, network-none resources and tracked
source only. No legacy owner-login/embedded-playback deployment probe is active.

## Approved Android native-torrent rollout — 2026-10-08

After explicit owner approval, source `d9ca1fc27b6f4fed5bd75d68ede4143aeef62085`
replaced the October 4 backend as image
`sha256:fb04756e32c6da83bcca5ed75215b6ba61ce10a6312c3b822455263060106350`.
The candidate passed 281 library tests and trusted local HTTPS/copied-data
qualification before replacement. Fresh SQLite online and stopped-writer backups,
the exact previous image/container and the private environment/keyring are
retained. The same named data volume, origin, UID, read-only root, capabilities,
tmpfs, network and port configuration were preserved. No offline retirement ran.

The replacement became ready in 1.63 seconds. Its actual image and healthy
container state were checked. All 90 existing tables matched the stopped-writer
snapshot immediately afterwards, including 40 protected account/profile/history/
source tables. Public HTTPS health, `/tv/` and dashboard asset
`assets/index-CiF8XBO-.js` were verified. The native protocol endpoint returns
401 without authorization and version 1 / native `[1]` with the existing device
session. Existing profiles, addons, history and Continue Watching remain readable.
The gateway, WARP and watch services/configuration were retained; the watch
hostname's separate viewing bundle was not updated in this backend-only rollout.
Physical public-swarm startup remains a separate Android acceptance measurement.

## Historical evidence below

The following records describe their exact older source/images. Their embedded
media/GPU/WARP observations are not current backend instructions or proof of this
candidate's deployment, decoder behavior or production migration.

## Historical native playback rollout — 2026-09-26

Backend `d715195aa16df7f9fd52f50e4f6d045e077449d9` and TV-web `f620993` were built together and promoted as image `sha256:5ab7ca9a28541086fd3a45302c8a2496dedc683a281305b2e73487e8ed29894a`. The running container's exact image and healthy public API were verified. Both `/tv/` and the viewing hostname serve `index-C6qjErCC.js`; all entry script/style requests returned the correct content types. The deployed viewing app reached its native account sign-in screen in a browser.

Acceptance included 208 server tests, strict Clippy, 61 engine unit tests, 14 real-FFmpeg tests, isolated image acceptance and actual server Quick Sync decoding/output checks. A candidate started against an online read-only backup of production; profile, account ownership, favorites, progress, provider, addon and migration rows were identical before/after. A fresh protected online backup and zero-session check preceded replacement; the same data comparisons passed afterwards. The original image, environment and backups remain private rollback material.

The first replacement encountered the host WARP service's existing D-Bus stale-PID restart loop and restored the prior backend image. Giving that service an ephemeral `/run` fixed the loop while retaining its registration volume. The egress sidecar was recovered separately; the successful backend retry preserved that sidecar and the original routing. The viewing bundle was staged from the exact image with its `/tv/` paths, retaining the existing nginx API Host/Origin rewrites.

The native credential-login endpoint is live and validates requests. An authenticated production live-channel launch returned direct mode in 49 ms, followed by successful lease release; this measures API launch, not time to first decoded frame. Native password login, direct decoding/seeking, source headers, loading/cancellation and player lifetime were separately verified on the isolated Android emulator. No new physical Roku, Tizen/Vizio, phone HDR/DRM, or universal provider qualification is claimed.


## Historical browser media rollout — 2026-09-27

Backend `9b0c206f0328ad3583bce7ef4954fe2bd1ff8c1c` and TV-web
`fdcdbf7298fa6280bf9bff497c5000569e806530` are deployed as image
`sha256:6f6d34ba54ad45d9dd695e77be2e67db5fe89106c5f0d264a7c68db2586d79eb`.
Both viewing hosts serve `app-DPY6ZflC.js`; entry, renderer and playback asset
bytes were compared against the exact image. Cloudflare adds its analytics
beacon to HTML, so public HTML checksums are not used as bundle identity.
Public health, unauthorized API rejection, the running image and container
health were verified. Browser preparation and local MSE copy are enabled;
FFmpeg 5.1.9 remains selected (see docs/TRANSCODER_BENCHMARK.md).

A production live source exposed irregular copied-HLS GOPs: its target duration
rose after initial publication, and the client stalled. The corrected release
uses continuous fragmented MP4 for qualified copied live sessions, keeps live
production unthrottled, supervises muxer media-time progress, and gives each
continuous body one viewer. Explicit HLS media refusals are now surfaced before
retry exhaustion. The same production channel subsequently advanced beyond
85 seconds with `video=copy`, `audio=aac`, `format=mp4`; Back returned to Home
and the authenticated dashboard confirmed zero active sessions.

Acceptance included 97 video tests, 217 client tests, 80 engine tests with real
FFmpeg, 208 server tests plus the affected shared-session regression suite,
strict Clippy, production builds and isolated container acceptance. Chromium,
Firefox, Playwright WebKit and the physical Vizio completed 30-minute 720p30
HTTPS runs. The browser drop ratios were 0.158%, 0.324% and 0.311%; maximum
sampled AV skew was below 17.1 ms. Vizio reported zero drops over 54,501 frames
and a maximum sampled MSE buffer of 30.05 seconds. A separate physical-TV
continuous live audio-conversion check passed 45 seconds with 1,472 reported
frames and zero drops. See the video repository's BROWSER_PIPELINE.md for
measurement limits; this is not sustained 4K/HDR or physical Safari evidence.

A copied-production database passed startup and row/hash comparisons before
replacement. Fresh read-only online backups and authenticated zero-session
checks preceded each replacement. Account/profile ownership, favorites,
progress, providers, addons and migration rows remained unchanged by startup.
The named data volume, GPU/WARP overlays and watch Host/Origin proxy rewrites
were retained. The watch bundle was published by an atomic nginx root change,
keeping prior bundles and lazy assets. Private rollback images, configuration,
backups and evidence remain outside Git. The TV was returned to VIPTV and the
temporary diagnostic servers were stopped.
