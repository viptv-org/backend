# Engine-free packaging

This is a local, reviewable packaging change, not a deployment or database cutover.
Runtime retirement and its backup/export/routing guard remain separately reviewed
in `RUNTIME_RETIREMENT.md`. No private env or production files were edited.

The removed public QSV/WARP/validation overlays, WARP proxy image, embedded-media
smoke/SSE/provider/soak probes, fixture generator and transcoder/GPU benchmarks
are retired. They exercised retired backend media/session/setup paths, not the
frozen v2 lease/gateway contract. Normal Rust tests and the disposable gateway
acceptance remain the replacements for current contracts.

The generic HTTPS byte-range fixture `scripts/benchmark-source.py` is retained
for diagnostic reuse; it has no backend hook.

Public Compose and the runtime image do not advertise embedded-media quotas,
cache, binary selection, GPU or WARP. The exact browser origin, restricted
nonroot/read-only container, named `viptv_viptv_data` volume and optional private
keyring remain. An unset keyring is not converted to empty JSON. Frontend build
flags belong to pinned client/video behavior and are not backend media
settings. The compatibility `container-check.sh` delegates to the existing
disposable network-none gateway fixture; it does not launch a deployment stack.

Local checks: Compose/static deployment assertions, host helper tests, shell
syntax, extraction inventory and a locked offline backend build. `docker build
--build-arg FRONTENDS=0` produces a backend-only image; it does not qualify
frontend packaging, physical devices, real subscriptions, production migration
or production ingress.

