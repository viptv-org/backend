# Engine-free packaging checkpoint

This is a local, reviewable packaging change, not a deployment or database cutover.
Runtime retirement and its backup/export/routing guard remain separately reviewed
in `RUNTIME_RETIREMENT.md`. No private env or production files were edited.

The removed public QSV/WARP/validation overlays, WARP proxy image, embedded-media
smoke/SSE/provider/soak probes, fixture generator and transcoder/GPU benchmarks
are recoverable at `d6b8d570326fa98bdb8077e8078f660617071b02`. `MIGRATION.json`
retains every original extraction hash and records removals or current hashes.
They exercised retired backend media/session/setup paths, not the frozen v2
lease/gateway contract. Shared active invariant coverage is mapped in
`RETIRED_TEST_AUDIT.md`; normal Rust tests and the disposable gateway acceptance
remain the replacements for current contracts, not claims of identical old tests.

The generic HTTPS byte-range fixture `scripts/benchmark-source.py` is retained
after explicit gateway-owner coordination. It is historical diagnostic material
without a backend hook; it was not imported into or moved to the gateway.

Public Compose and the runtime image no longer advertise embedded-media quotas,
cache, binary selection, GPU or WARP. The exact browser origin, restricted
nonroot/read-only container, named `viptv_viptv_data` volume and optional private
keyring remain. An unset keyring is not converted to empty JSON. Frontend build
flags still belong to pinned client/video behavior and are not backend media
settings. The compatibility `container-check.sh` now delegates to the existing
disposable network-none gateway fixture; it does not launch a deployment stack.

Local qualification: Compose/static deployment assertions, nine fake-Docker host
helper tests, shell syntax, extraction inventory and locked offline backend build
passed. `docker build --build-arg FRONTENDS=0` also passed with local runtime image
`sha256:4a9b5e7a1a7b` (short ID); it does not qualify frontend packaging, physical devices, real subscriptions,
production migration or production ingress. No shared HTTPS service was stopped.
