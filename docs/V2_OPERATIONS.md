# V2 account IPTV operations

The development engine-free runtime/guarded offline retirement is described
in [RUNTIME_RETIREMENT.md](RUNTIME_RETIREMENT.md). Reviewed clients now have v2
adoption candidates; historical checkpoint paragraphs below do not override that
source evidence or authorize production migration. The Android v2 handoff is
merged in source (viptv-org/android#6). Legacy runtime behavior cannot be retained in
the cleanup candidate; retired API namespaces return `client_update_required`.

This is a development-branch contract, not authorization to migrate production.
Production deployment and migration require separate approval. No viewing UI or
playlist swap control is introduced by these endpoints.

Current source at `8823114` requires encrypted runtime provider/addon credential
reads and encrypted new credential writes. Preserved plaintext rows are offline
migration inputs, not a runtime fallback. `/api/addons` GET/PATCH/DELETE metadata
wrappers and encrypted-only POST remain for compatible clients; retaining these
wrappers does not restore retired media/setup/organizer behavior. Historical
qualification counts and pre-cutover controls below are checkpoint evidence only.

## Responsive restoration recovery adoption — 2026-10-02

The viewing gitlink adopts reviewed TV-web source
`c797ad081e41f3ee2c3e1d0eaf7f2ae5e91f83e6` (PR11/PR12/PR13). Failed Next/restoration
offers one recovery dialog, the responsive Retry icon, exact outgoing
source/position Retry and manual source/Back actions. Obsolete failure callbacks
and busy cleanup are fenced to their originating navigation/request; an owned
same-navigation deadline still offers recovery. TV layout, Core `f66c87e`, and the dashboard gitlink
`2a6094067e44b6e2b8ab58b012a971c4ab7fad38` are unchanged. Populated responsive
source lists also retain their existing100px desktop/108px phone row sizes,
without flex shrink clipping Best match or description content. The twelve-row
390/1440 geometry regressions are red before/green after; existing focused
filename/reduced-motion acceptance passes. This restores existing CSS sizes.

Video source `2038c0f` also preserves selected VOD Resume intent while native
startup metadata is incomplete; an initial duration0/live=true snapshot no
longer suppresses a requested seek. The two public adapter/IPC regressions are
red before/green after, with128 Video tests/typecheck/build passing. Renderer
policy is unchanged. Actual native replay and the reported controls/sidebar
flicker remain separate; this adoption does not claim a rendering fix.

The consumer's fresh viewing build passes design/Core/video integrity and all
TypeScript groups. TV-web's recovery source `a2c53af` passed267 app tests, two public-stage
ownership regressions (cancelled failure red/green), three full UI/HTTP/adapter
recovery cases and private trusted-HTTPS DeskPlayerRestore inspection.
Existing backend/runtime/native media evidence retains its recorded source
pins; this frontend adoption does not requalify installed/physical consumers,
move a database, change API/media policy or deploy anything.

## Reviewed frontend source checkpoint — 2026-09-30

The dashboard submodule pins web main
`a6eca85f2bd0dcfaa9e31ab352300bbeccbaab98`, which merges ADM-002 management
(`93c9316`) and bounded VOD browsing (`048651e`, see
[BOUNDED_VOD_ACCEPTANCE.md](BOUNDED_VOD_ACCEPTANCE.md)); the viewing submodule pins
tv-web main `e5789ab30361fba5816e0322bf3df98401604d75`, which labels both
responsive player timeline ends as clocks (desktop parity, design#3) on top of
`4ffc748`; that earlier pin adopts Core `1f8483e` and video `514a332`, offers
only plugin-reported desktop engines and ranks continuation sources against
measured device capabilities (242 unit tests, build and targeted single-worker
browser specs passed at both pins). Account management uses the v2
connection, matches and gateway contracts, while ordinary live viewing uses
the raw cursor catalog and exact source selection. Retired organizer/setup
controls are absent from the dashboard. This supersedes the historical notes
below about the old dashboard matches route and pending client guide adoption.
The follow-up dashboard pin also rejects repeated/overlapping management pages
and retains failed-refresh source identity. Same-profile drafts survive parent
challenges without exposing protected portals, persisting secrets or replaying
saves; 88 unit tests and both desktop/phone HTTPS fixtures passed. The earlier viewing pin `db9c5ab`
retires dormant local-only code and adopts Core `8ae9f81`; 234 retained tests and
36 HTTPS cases passed. These remain client-side evidence, separate from backend
runtime acceptance. The engine-free backend at `dd37044` passed 207 tests and
the exact-source isolated real-gateway media lifecycle fixture.

These are source pins, not deployment evidence. Dashboard acceptance used
mocked APIs over local HTTPS; TV-web passed local unit/browser checks.
No production database or public website was modified. See the pinned repos'
validation records for incomplete real-gateway and physical-device gates.

## Exact live playback and personal guide subsets

### Cursor size boundary

Live next/previous tokens are bounded at 4096 characters; unmatched-VOD tokens
retain their existing 2048-character bound. Encoders now enforce the same limit
as their respective decoders. If a provider's original identifier makes a token
too large, the page fails with a safe 502 `catalog_cursor_too_large` reason instead
of returning a token that will fail on the next request. This does not truncate,
filter, delete or rewrite stored provider IDs, change import policy, or add a
playlist-swap UI. Synthetic HTTP fixtures cover both live directions, VOD and
unchanged raw data; an encoder fixture checks the exact bounds. Full suite:
213 passing tests, two opt-in fixtures ignored; Clippy/format/inventory pass.
Actual source observations on physical devices remain separate acceptance.

Viewing clients use POST `/api/v2/iptv/live/:id/source` for one selected raw
channel. It returns `{source}` with an opaque playback handle, provider identity
and safe presentation metadata, never a source URL or credentials. It does not
query addons or allocate a discovery job. The handle then goes through ordinary
`/api/v2/playback`; native HTTP delivery remains eligible, while Roku/Vizio still
require authorized gateway delivery. No gateway means an actionable refusal.

Ownership, enabled live scope, selected profile/parent policy and credential proof
are checked before publication and again at playback admission. A resolved URL
cannot acquire a newer credential proof after a concurrent edit. Missing,
unassigned, foreign, disabled and retired composite IDs do not guess a replacement.

Channel pages also accept `collection=favorites|recent`. These are selected-
profile saved subsets of the chosen/default raw playlist, retaining provider
order—not US classification, family-lineup filtering or an all-provider index.
Personal cursors bind the server-authenticated profile; there is no `profile_id`
query override. Categories reject personal collection filters. Saved references
outside the current playlist (including retired composites) remain in history/
favorites; they are not silently remapped. No synchronous total is introduced.

Historical pre-retirement checkpoint: full backend suite 281 passed/four opt-in
fixtures ignored, strict
all-target Clippy passed. The 13 catalog/ownership fixtures also passed after
extending exact live selection through native direct v2 admission and release.
Synthetic tests cover profile-bound saved subsets, default/override isolation,
hidden foreign/unassigned/composite IDs, parent gates, private source cards and
credential-proof changes. No real provider, hardware or production state was used.
Guide/catalog adoption and retired runtime removal subsequently reached the
reviewed source checkpoints above; that does not complete production cutover.

## Account management API

### Historical private source headers checkpoint — 2026-09-30

Source registration preserves the existing closed request-header allowlist and
adds only `X-API-Key`, which the existing direct-client and gateway transport
contracts can carry privately. Cookie, Authorization, X-CSRF-Token and X-API-Key
credential reflections are omitted from source-card display fields, including
configured producer labels. Ordinary language/client-identification metadata is
retained; short cookie values are matched at token boundaries.

An unsupported, malformed or case-colliding required request header rejects that
candidate with safe `source_headers_unsupported` classification rather than
silently dropping the header. Healthy sibling candidates remain available. No
raw input URL or diagnostic is included in this error. Authorized direct delivery
and gateway preparation retain accepted header values; cards and gateway viewer
delivery do not disclose them.

Synthetic registration, discovery-event and v2 direct/gateway lease fixtures:
205 tests passed, two opt-in tests ignored; strict all-target Clippy passed. These
checks did not qualify real gateway media playback or hardware. At that earlier
checkpoint, version-zero plaintext fallback still existed. The strict runtime
encryption boundary below supersedes that behavior; offline migration/retirement
evidence remains a separate operational gate.

### Runtime encryption boundary (current behavior; historical validation count)

The subsequent coordinated runtime checkpoint supersedes the plaintext fallback
caveat above: provider/addon credential readers and opaque-source registration
refuse version-zero credentials with safe `source_credentials_migration_required`
copy. Restoring the correct keyring remains necessary for encrypted sources.
There is no whole-server startup refusal or runtime encryption/owner assignment;
authentication, history and owned raw catalog-index inspection remain available.
Unmigrated sources retain their identities and bytes for reviewed offline work.

Explicit offline inspection, ownership assignment, encryption and export tools
still inspect legacy data. Only the private offline add-on encryption reader may
decode a legacy manifest URL; runtime discovery never invokes that reader.
Healthy encrypted siblings remain usable. Source discovery reports bounded safe
per-add-on failures, and the v2 management list exposes configuration errors.
Compatibility catalog arrays retain their existing shape and list only usable
encrypted sources when mixed with legacy entries.

Compatibility clients may still call `/api/addons` and `/api/catalogs` metadata
routes. These routes are retained: metadata never returns stored manifest URLs;
GET/PATCH/DELETE wrappers remain account-scoped and do not decrypt legacy secrets.
Enable/delete can operate on archived owned metadata without granting playback
access to unmigrated credentials. Legacy add-on POST now requires a keyring
before fetching and writes only encrypted records. It cannot reinstall a legacy
source to bypass the reviewed migration. No client contract fields or UI changed.

Historical validation at the encryption-boundary checkpoint: 206 tests passed,
two opt-in tests ignored; strict all-target Clippy
passed. Synthetic fixtures cover plaintext refusal, unchanged legacy bytes,
healthy encrypted sources, auth/history/raw-index availability and missing-keyring
POST refusal before network/write. Genuine source/lease fixtures now use encrypted
credentials and detail caches. Existing offline migration/export fixtures pass.
No production migration, deployment, real gateway media or hardware was exercised.

Authenticated account sessions use:

- GET/POST `/api/v2/iptv/connections`
- PATCH/DELETE `/api/v2/iptv/connections/:id`
- PUT `/api/v2/iptv/connections/:id/credentials` with a replacement `password`.
- GET `/api/v2/iptv/matches?provider_id=…&kind=movie&search=…&limit=50&cursor=…`
- PUT `/api/v2/iptv/matches` with `vod_id`, `metadata_id`, `type` (movie/series).
- GET `/api/v2/iptv/live-default`
- PUT `/api/v2/iptv/live-default` with `catalog_id`.

The matches response is `{items, next_cursor}`. No full-library count or candidate
array is produced. Limit is 1–200; default 50. Cursors are tied to account and
filters. Source URL/username/password fields are never returned here. Invalid
cursors and queries return actionable messages plus stable error_code values.

Connection creation accepts `name`, `url`, `username`, `password` and optional
`enabled`, `enable_live`, `enable_movies`, `enable_series` (all default true).
It validates the login before atomically saving an encrypted tuple and ownership.
No URL/user/password is returned by management reads. A connection list returns
`{items,next_cursor}`, with default 50/max 200 and account-bound cursors. Creation
is limited to 64 owned connections; existing larger migrated accounts can still
be paged. Duplicate detection is account-local, never a global subscription probe.
The first enabled live connection persists as default; adding more never switches
it. Disabling/removing a default selects the oldest remaining enabled live source.

PATCH changes the name/enabled scopes only. Password renewal validates the new
password against the existing server/login, rechecks authorization and compares
the prior ciphertext before replacement. It clears detail caches but preserves
catalog IDs and mappings. Changing the server/login identity requires adding a
separate connection, avoiding silent reuse of unrelated stream IDs. Legacy rows
must complete reviewed encryption before v2 mutation. Account sessions, not
paired devices, perform these management operations; parent restrictions remain.

Login checks and scoped/encrypted Xtream API fetches accept public HTTP and HTTPS.
They validate and pin all DNS answers on each request, reject private/reserved
destinations, do not inherit proxies, and refuse redirects rather than forwarding
credentials. Response size, timeout and concurrency are bounded. Redirects,
rejected credentials, API rate limits and unavailable/oversized responses have
distinct safe errors. HTTP 429 is not guessed to mean a stream connection limit.
Operator-managed private-network exceptions remain unimplemented. Loopback
exceptions exist only in explicitly enabled synthetic test fixtures.

Reported positive provider connection allowances govern migrated/new native
direct admission; missing/zero reports do not invent a one-stream subscription
limit. Existing configured allowances are preserved by migration. Gate capacity
is stable while reports change, so outstanding permits are never replaced.
Encrypted connections also encrypt cached Xtream detail/EPG payloads with
account/provider/cache-key binding because those responses can contain secrets.

## Background catalog indexing

Enabled new connections enqueue their first import in the same transaction as
credential storage. Scope/enabled changes and password renewal enqueue a fresh
run; name-only changes do not restart a refresh. Migrated encrypted connections
without refresh state are queued on backend startup. The backend runtime starts
the worker when the operator keyring is configured.

- GET `/api/v2/iptv/connections/:id/refresh`: state, timestamps, safe error,
  next refresh time and last successful item counts.
- POST the same route: enqueue/retry (202); an already queued/running request is
  idempotent.
- DELETE the same route: cancel pending/running work and automatic refresh until
  an explicit retry or content/credential change.

Connection list/create/update responses also include `refresh`. States are idle,
queued, running, succeeded, failed and cancelled. Success schedules the next run
after six hours; failure retries after five minutes. The last successful counts
may remain present while queued/running/failed, just as the previous catalog does.

Refresh is owned by the persisted account/connection configuration, **not by a
browser session**: closing the app or signing out does not cancel indexing.
Control requests require current account authorization. Workers recheck account
enabled state, source ownership, enabled scopes, ciphertext/configuration and
their run token before fetching and publication. Account/source revocation,
explicit cancellation or a configuration change prevents late results from
committing. No session token or provider credentials are saved in job records.

At most two worker catalogs run concurrently, and the shared HTTP gate bounds
requests. Claim ordering favors another waiting account over one already running.
Each run has a 120-second deadline; dropped/timed-out workers invalidate their
publication guard. Expired running claims can recover after 125 seconds with a
new token, preventing an old worker from publishing into its replacement run.
HTTP requests remain individually bounded; cancellation prevents publication
immediately after its state change, while an in-flight fetch may take up to its
request timeout to settle. Missing keyrings report a failure rather than leaving
the queue indefinitely pending.

Protected login validation precedes sequential live/category/movie/series fetches.
Each v2 response is bounded at 64 MiB. This is server-side indexing, not client
playlist synchronization. Catalog replacement, generation and succeeded status
commit atomically. Failure keeps the entire prior snapshot; an authenticated,
valid empty array is a legitimate replacement, not a custom filtering rule.
The old provider refresh/pool/lineup namespaces are retired in the current
engine-free runtime and return `client_update_required`, not an alternate refresh
engine. Reviewed admin/client source adoption is recorded above; production
migration and client handoff/deployment remain separate gates.

Each handler revalidates the captured account session before querying or writing.
Selected kids profiles require parent authorization; paired TV/device sessions
cannot manage account connections. Operator accounts still see only their own
assigned providers. Viewing-catalog APIs will use their own playback/profile
authorization rather than treating these management routes as TV browse routes.

The default stays unchanged while its provider remains enabled for live. If it
becomes unavailable, the next default read persists the oldest enabled owned
provider. No enabled sources means catalog_id is null. An explicit foreign or
disabled selection fails, without changing the existing default.

Historical pre-admin/pre-retirement notes about the old matches UI and active
legacy media controls are superseded by the reviewed frontend/runtime checkpoints
above. Account/admin now uses the v2 management contracts. Deliberately retained
addon metadata/Stremio wrappers, authentication, profiles and history are not
retired media/setup controls. This is still not a blanket claim of production
or all-device playback qualification.

## Explicit legacy ownership migration

Build the dedicated executable:

```sh
cargo build --locked --manifest-path server/Cargo.toml --bin provider-owners
server/target/debug/provider-owners inspect /absolute/path/source.sqlite
```

Inspection opens SQLite read-only, does not create missing schema, and prints
only provider/account IDs. Obtain account IDs from the authenticated account
administration surface and have the operator review the owner map. Never infer
ownership from ordering, an account name, the first account, or an owner role.

The JSON map uses provider IDs as keys and account IDs as values. For example,
`{"12":34}` assigns example provider 12 to example account 34; these are not
production IDs. Include every currently unassigned provider exactly once. IDs
must be positive; duplicate keys and disabled/missing accounts are rejected.
Already-assigned providers cannot be reassigned by this tool.

After approval, stop normal writers and use new paths in a private directory:

```sh
install -d -m 700 .migration-v2/reviewed-run
# Place the reviewed owners.json in that directory before proceeding.
server/target/debug/provider-owners apply /absolute/path/source.sqlite \
  .migration-v2/reviewed-run/owners.json \
  .migration-v2/reviewed-run/before.sqlite \
  .migration-v2/reviewed-run/advanced.json \
  FULL_40_CHARACTER_BACKEND_SOURCE_COMMIT --confirm-ownership
```

The final argument is the backend source revision used to build the reviewed
tool. The original ownership checkpoint preceded its image build. The executable
is now packaged in the reviewed full image; image/browser fixture evidence does
not authorize deploying it or running it against production data.

The operation holds BEGIN IMMEDIATE while it:

1. Copies a consistent database through SQLite's online backup API, including
   committed WAL data. It never raw-copies an active database file.
2. Checks backup integrity, makes the backup standalone, and syncs it.
3. Streams a schema-versioned advanced-configuration export, recording the
   declared source revision and backup SHA-256. File data and directory entries
   are synced before changing the source database.
4. Initializes additive v2 tables and applies the complete validated owner map
   within the same transaction, preserving existing IDs and VOD mappings.
5. Persists initial account live defaults and commits the assignment.

Backup/export files must not exist. On Unix they require an owner-private
directory and are created mode 0600. The full backup contains all private data.
The advanced export also contains secrets (including IPTV credentials) and is
marked contains_secrets=true. It includes connections/live IDs, provider route
flags, pools, family
lineup/matching rules, guide settings/mappings, health configuration and catalog
scheduling. Environment values (including VIPTV_WARP_PROXY) are not collected;
the operator must retain the existing private deployment environment separately.
Authentication sessions and viewing history are not copied into the
advanced export; they remain in the complete backup. Derived guide programmes,
run logs and other runtime caches are not a portable configuration contract.

Keep both artifacts offline and private; never commit or upload them. The ignored
.migration-v2 directory is a convenience, not a substitute for access controls.
Failed attempts may leave private artifacts for inspection. Do not overwrite them
on retry. Check the returned error and use fresh paths after correcting the map.

## Rollback boundary

This ownership command performs no automatic restore or destructive cleanup.
The separately confirmed retirement command is documented in RUNTIME_RETIREMENT.md.
Invalid maps roll back the source transaction, including newly initialized v2
schema; successful assignment
does not delete accounts, profiles, progress, matches or retired configuration.

Do not replace a live database with an older backup: that can discard later
viewing history. Any restore requires stopped writers, separate approval and a
fresh preservation backup of the current database. Likewise, do not roll a public
multi-tenant installation back to legacy code that ignores account ownership.
Full coordinated rollback remains acceptance work before production cutover.

## Offline provider credential encryption

This command is implemented and fixture-tested, **not approved for production
use yet**. Reviewed client/runtime source adoption supersedes the historical
pending-cutover note, but production migration is not complete. Legacy provider
renew/update/delete/pool, family matching and external provider XMLTV namespaces
are retired for all sources and return `client_update_required`; they are not
an operational fallback after this command. Raw v2 catalogs, native Xtream guide
reads and source discovery use the encrypted reader.

After explicit ownership assignment and separately approved downtime, stop all
backend processes/readers/writers. Supply the same operator-managed
`VIPTV_SECRETS_KEYRING` used for gateway keys, retain it securely, and use fresh
private backup/export paths:

```sh
server/target/debug/provider-owners encrypt /absolute/path/source.sqlite \
  .migration-v2/reviewed-run/before-encryption.sqlite \
  .migration-v2/reviewed-run/before-encryption.json \
  FULL_40_CHARACTER_BACKEND_SOURCE_COMMIT --confirm-encryption
```

The backup/export durability checks are identical to ownership assignment. No
encryption writes occur before those artifacts are synced. Every provider must
already have an explicit owner. URL, username and password are sealed together
using the existing authenticated vault, bound to account, provider ID and the
`xtream` purpose. The legacy columns are emptied and an explicit format marker
prevents missing ciphertext from falling back to plaintext. Cached Xtream
payloads are invalidated because providers can embed credentials in them.
Provider IDs, live/VOD rows, manual matches, accounts, profiles and progress stay
unchanged. Re-running with new artifact paths validates existing ciphertext;
it does not rotate/re-encrypt already encrypted records.

After commit, the command checkpoints/truncates WAL, vacuums SQLite, and performs
a second checkpoint. A failure here reports
`encryption_committed_cleanup_required`: **the encryption transaction already
committed**. Stop remaining readers/writers and investigate; do not assume a
rollback or overwrite the database from an old backup. A fresh-path retry with
the same valid keyring repeats validation and cleanup.

Synthetic recovery acceptance on 2026-09-30 holds an actual old WAL reader
through the encryption transaction. The real checkpoint reports
`encryption_committed_cleanup_required` after commit, while credential rows are
already sealed and backup/export remain private. The fixture then records later
progress and a new history row, closes the reader, and retries with new artifact
paths. Retry validates existing ciphertext without rotating it, compacts the old
plaintext and rebuilds FTS while retaining both later history writes and source/
match identity. This is not permission to overwrite current data with the older
backup, and does not qualify a coordinated old-image/client rollback. The full
backend suite at this historical checkpoint passed 210 tests with two opt-in
media fixtures
ignored; strict Clippy, formatting and the extraction inventory passed.

Missing/wrong keys, changed ownership and damaged ciphertext fail closed.
Running the new backend supplies the vault to the provider reader. Native
direct admission for migrated providers is independent of legacy cross-provider
pools and retains the configured connection allowance. HTTP provider URLs are
not upgraded or rejected merely for using HTTP.

This encrypts the provider credential tuple, not the entire SQLite database.
The **backup and export intentionally contain plaintext**. Retired configuration,
external copies, logs, filesystem snapshots and physical storage remnants are not
securely erased by SQLite compaction. Keep artifacts private and apply the
operator's retention policy; never commit or upload them. Reviewed client source
adoption is separate from production deployment.
Operator-managed private-network exceptions, bulk key rotation, production
migration and coordinated rollback remain unqualified; this is not a complete
encrypted-secrets acceptance claim. Addon
storage and reviewed ownership/encryption commands are now covered by
[the addon management/migration contract](ADDONS_V2.md).
