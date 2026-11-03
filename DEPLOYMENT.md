# Production deployment and migration

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
