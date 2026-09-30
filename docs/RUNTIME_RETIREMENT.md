# Engine-free runtime and offline retirement — 2026-09-30

This is an isolated source candidate, not a production operation. The user's
Android UI checkout still needs its reviewed handoff merged; do not deploy this
server under old media/catalog clients.

The backend no longer links `viptv-playback-engine`, configures FFmpeg/GPU/media
directories, serves `/media` bytes, or starts family catalog/health/XMLTV workers.
Its image omits FFmpeg and Intel media drivers. The independent gateway retains
its separate engine and wire contract. `/catalogs`, `/discover`, `/meta`, account
auth, profiles, history, favorites, queue, addons and v2 API contracts remain.
Retired namespaces refuse every HTTP method with `client_update_required`
before parsing input or creating jobs. Historical composite IDs are unavailable
for playback, retained exactly in storage, and never remapped to raw channels.

Normal startup creates no retired configuration tables. Existing advanced
configuration remains archived until a separately approved offline operation.
Legacy WARP rows fence v2 source use rather than silently changing routing;
the export reader remains compatible before and after table retirement.

## Reviewed offline phase

Build `provider-owners`, obtain separate approval and stop normal readers/writers.
Complete explicit provider ownership and credential encryption first. Retain the
operator keyring and existing private deployment environment. Use fresh files
in an owner-private directory:

```sh
provider-owners retire /absolute/path/source.sqlite NEW_BACKUP.sqlite \
  NEW_ADVANCED.json FULL_40_CHARACTER_BACKEND_SOURCE_COMMIT --confirm-retirement
```

The confirmation flag is required; this command is never called by server boot.
It holds `BEGIN IMMEDIATE`, creates/checks a consistent SQLite online backup,
streams the version-1 `contains_secrets=true` export, and syncs both files and
their directories before any source mutation. Existing artifact paths fail.
Missing ownership, plaintext/missing/corrupt ciphertext, wrong keys and unknown
foreign dependencies refuse/roll back deletion. No owner is inferred or assigned.
An enabled provider with a legacy WARP route also refuses retirement with
`retirement_routing_review_required`; its route remains fenced and preserved in
the private artifacts. The operator must review routing outside VIPTV before
disabling/archiving that connection. Only disabled archived route rows may be
exported/dropped; review their exported requirements before later re-enablement.
The static deletion set excludes accounts, profiles, favorite/progress/queue
records, addons, provider identities/raw indexes, manual VOD matches and v2 data.
Auth/profile family-token terminology is unrelated and remains intact.

The transaction removes only reviewed family/pool/filter/repair/scheduler tables,
checks foreign-key integrity, and records retirement version 2/source revision.
Fresh-path retries validate keys and produce new preservation artifacts even if
no retired tables remain. Artifacts require mode 0600 / private-owned directories
on Unix. Failed attempts can leave private artifacts; keep them for inspection.
This is not secure erasure, proof that every process has stopped, or automatic
rollback. Never overwrite a current database from an old backup or run
`docker compose down -v`; preserve `viptv_viptv_data` and later history.

Maximum quality is absent from active preferences and decoder policy. Historical
quality JSON remains archived across new quality-free writes and in private
export/backup; mixed old writes ignore it, quality-only writes refuse retirement.
Actual client decoder dimensions remain in the unchanged v2 playback request.

## Evidence boundary

Five synthetic offline fixtures verify backup/export contents and permissions,
ID/history/match preservation, active-routing/ownership/encryption refusal, wrong-key/unknown-FK
rollback, idempotence and no table resurrection at boot. Runtime retirement
fixtures verify no job allocation/media relay, async responsiveness, exact
raw/composite history IDs and quality-free writer preservation. The removed
legacy API suite's shared cases are audited in RETIRED_TEST_AUDIT.md.

Passed: 199 locked Rust tests (185 library + 14 integration), two opt-in gateway
fixtures ignored; strict all-target Clippy, formatting and original extraction
inventory checks. This replaces retired fixture expectations explicitly rather
than retaining backend media execution. Root's network-none real-gateway harness
must be rerun on this exact
source commit. Production ownership/migration/rollback, native TLS, real provider,
physical decoder/4K/tracks and coordinated client rollout remain separate gates.
