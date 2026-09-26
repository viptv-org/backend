# Production deployment and migration

## Authorized target

Production is `https://viptv.syek.tech`, currently routed to the authorized VIPTV Docker host. Host access, sudo transport, account credentials, recovery material, database backups, and release assets remain private and are never stored in this repository.

The application container runs UID/GID 10001, read-only, with all Linux capabilities dropped and `no-new-privileges`. `/data` persists SQLite; generated HLS and temporary files are bounded tmpfs. FFmpeg has automatic threading and the container intentionally has no CPU, memory, swap, or PID quota. Provider and playback concurrency remain bounded.

## Account-only configuration

The host `.env` must be owner-readable only and contain the public origins:

```dotenv
# Comma-separated HTTPS origins. The FIRST entry is canonical: device-pairing
# QR codes and verification URLs always send devices there. Additional entries
# are extra accepted browser origins, e.g. a reverse-proxy hostname that
# serves the same bundle against this backend (no Host/Origin rewriting needed).
VIPTV_AUTH_ORIGIN=https://viptv.syek.tech
VIPTV_PUBLISH_IP=<server-lan-ip>
VIPTV_PORT=8080
VIPTV_MAX_SESSIONS=2
VIPTV_SESSION_TTL=120
```

There is no shared application credential in Compose or the Roku package. The Roku private package locks only `https://viptv.syek.tech`; each TV obtains revocable device credentials through QR/device pairing.

For a genuinely new database, create the owner offline with `viptv-server create-admin` before starting public service. Public registration never creates an owner. Existing production already has its owner and must not run owner creation again.

## Guarded upgrade procedure

1. Confirm the candidate commit/release and keep the current production image/source as rollback material.
2. Confirm `GET /api/status` reports zero active playback sessions, or coordinate a maintenance window.
3. Create a private consistent backup using SQLite's online backup API from a read-only source connection, or stop the application for a complete database/WAL backup. Never raw-copy an active SQLite database. Record file checksum, profile/favorite/progress/provider/addon row counts, stable profile IDs, and progress timestamps/positions. Refresh this backup immediately before replacement if data has changed.
4. Build the candidate without changing the production volume or tag. Preserve configured overlays (`compose.warp.yaml` and `compose.qsv.yaml`) when replacing production. Run `scripts/container-check.sh` against the candidate in its isolated project.
5. Run Rust, dashboard, Roku, static deployment, and migration tests. Migration acceptance must prove existing IDs/history remain unchanged and ambiguous historical multi-account profile ownership fails safely.
6. Start the candidate against a copy of production SQLite first. Verify migration ledger, row counts, representative values, owner login, imported-profile setup state, and provider/addon inventory.
7. Replace production only after the copied-data check passes. Keep a protected copy of the original `.env` for rollback. Set the exact `VIPTV_AUTH_ORIGIN` and remove retired shared-access configuration from the active environment, preserving other settings, the named volume, HTTPS route, and rollback image.
8. Verify public health and unauthorized API rejection. Use `tests/live_deployment_check.py` with existing owner credentials supplied through `VIPTV_TEST_USERNAME`/`VIPTV_TEST_PASSWORD`; it never creates accounts, profiles, or library rows.
9. Verify browser registration with a deliberate disposable account only in an isolated environment—not production. On production, verify existing owner/member login, profile selection, and administration without creating test library data.
10. Pair the physical Roku through its QR, verify remembered profile/switching, manual source selection, Resume source identity, direct/remux/transcode startup, native pause, repeated seeks, seek rollback, cleanup, and device revocation.
11. Compare production row counts and representative history again. Keep the backup and prior image until the observation window ends.

Never publish database copies, cookies, passwords, recovery codes, device tokens, provider URLs/credentials, addon installation URLs, or secret-bearing logs.

## Verified native playback rollout — 2026-09-26

Backend `d715195aa16df7f9fd52f50e4f6d045e077449d9` and TV-web `f620993` were built together and promoted as image `sha256:5ab7ca9a28541086fd3a45302c8a2496dedc683a281305b2e73487e8ed29894a`. The running container's exact image and healthy public API were verified. Both `/tv/` and the viewing hostname serve `index-C6qjErCC.js`; all entry script/style requests returned the correct content types. The deployed viewing app reached its native account sign-in screen in a browser.

Acceptance included 208 server tests, strict Clippy, 61 engine unit tests, 14 real-FFmpeg tests, isolated image acceptance and actual server Quick Sync decoding/output checks. A candidate started against an online read-only backup of production; profile, account ownership, favorites, progress, provider, addon and migration rows were identical before/after. A fresh protected online backup and zero-session check preceded replacement; the same data comparisons passed afterwards. The original image, environment and backups remain private rollback material.

The first replacement encountered the host WARP service's existing D-Bus stale-PID restart loop and restored the prior backend image. Giving that service an ephemeral `/run` fixed the loop while retaining its registration volume. The egress sidecar was recovered separately; the successful backend retry preserved that sidecar and the original routing. The viewing bundle was staged from the exact image with its `/tv/` paths, retaining the existing nginx API Host/Origin rewrites.

The native credential-login endpoint is live and validates requests. An authenticated production live-channel launch returned direct mode in 49 ms, followed by successful lease release; this measures API launch, not time to first decoded frame. Native password login, direct decoding/seeking, source headers, loading/cancellation and player lifetime were separately verified on the isolated Android emulator. No new physical Roku, Tizen/Vizio, phone HDR/DRM, or universal provider qualification is claimed.

## Rollback

Do not run an older binary against a migrated database unless compatibility is proven. If rollback is required:

1. Stop the candidate.
2. Preserve candidate logs privately and checksum the failed database for diagnosis.
3. Restore the complete stopped-instance pre-upgrade SQLite backup with UID/GID 10001 and mode 0600.
4. Restore the prior image/source and unchanged `.env`/HTTPS route.
5. Start, check health, authenticate as the existing owner, and compare recorded IDs/history/counts.

`docker compose down` preserves the named volume; `docker compose down -v` deletes it and is never part of upgrade/rollback.

## Validation commands

On the host, without printing rendered configuration:

```sh
bash scripts/host-check.sh
sudo bash scripts/container-check.sh
sudo docker compose --project-directory /home/vynxc/viptv \
  --env-file /home/vynxc/viptv/.env -f /home/vynxc/viptv/compose.yaml ps
```

Live read-only account/API/container validation:

```sh
read -rp 'Existing owner username: ' VIPTV_TEST_USERNAME
read -rsp 'Existing owner password: ' VIPTV_TEST_PASSWORD; printf '\n'
export VIPTV_TEST_USERNAME VIPTV_TEST_PASSWORD
sudo -E python3 tests/live_deployment_check.py \
  --env-file /home/vynxc/viptv/.env --check-cinemeta
unset VIPTV_TEST_USERNAME VIPTV_TEST_PASSWORD
```

Prefer a root-owned environment/credential handoff instead of `sudo -E` where available. Credentials must not be command-line arguments or written into shell history.

## HTTPS route

The public route terminates TLS and forwards to the host application port. It must preserve the public Host and proxy `/api`, `/media`, SSE, and dashboard assets without bypassing authentication. Do not place browser SSO in front of Roku API/media routes unless the native client is designed for it. Do not disable TLS certificate validation.

## Release gate

A release requires all automated checks, isolated Docker/real-FFmpeg acceptance, copied-production migration proof, actual production validation, and physical Roku evidence. Simulator success is not physical-device proof. Release ZIPs include no credential, only the locked origin, and must be distributed privately with complete corresponding GPLv2 Roku source/notices and rebuild instructions.
