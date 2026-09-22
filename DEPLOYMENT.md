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
