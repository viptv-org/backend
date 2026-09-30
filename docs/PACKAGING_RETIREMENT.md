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

## Full image HTTP acceptance — 2026-09-30

`scripts/check-runtime-image.py` exercises an existing local image by exact
content ID and source label. It never pulls, loads private environment files,
mounts host/production data, publishes ports or changes the shared HTTPS stack.
Each run creates a new network-none/read-only/nonroot container with private
tmpfs synthetic SQLite data and removes only its returned container ID. Private
summary/log evidence is retained in a fresh mode-0700 temporary directory; no
credentials or database are included in the public result or committed evidence.

```sh
python3 scripts/check-runtime-image.py --sudo viptv:qualification-205a70a \
  --expect-image-id sha256:9b2393c866d55321fabd21e61aba86035915e0e1ad1b28108bfb449cb9f94827 \
  --expect-revision 205a70a06527a9b054e96b84e726e86a51b22fe1
```

That exact full image passed seven acceptance groups: image/source identity;
UID10001, read-only root, no capabilities/mounts/ports and no FFmpeg; actual
Docker healthy status; dashboard/TV HTML and linked module/CSS MIME; real
registration/profile/account isolation and quality-free preferences; empty lazy
catalog/default and no-keyring refusal before fetch/write; retired namespaces
across GET/POST/PUT/PATCH/DELETE with malformed JSON and no media child processes;
and browser/native login, refresh rotation, stale-token rejection, logout and
session revocation. The image contains dashboard `93c9316` and TV `db9` from the
tracked-source packaging build. The TV entry has linked modules rather than a
separate CSS link; dashboard CSS is explicitly checked.

Cookie requests deliberately supply the pinned HTTPS Origin and CSRF token over
container-namespace HTTP. This is API/packaging evidence, **not browser TLS,
Secure/SameSite cookie-storage behavior, browser navigation/focus, production
ingress, deployment or real media/provider/gateway/hardware qualification**.
Those remain separate isolated HTTPS and native acceptance gates.
