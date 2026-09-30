# V2 account IPTV operations

The isolated engine-free runtime/guarded offline retirement candidate is described
in [RUNTIME_RETIREMENT.md](RUNTIME_RETIREMENT.md). Reviewed clients now have v2
adoption candidates; historical checkpoint paragraphs below do not override that
source evidence or authorize production migration. The user's Android UI checkout
still requires the handoff merged. Legacy runtime behavior cannot be retained in
the cleanup candidate; retired API namespaces return `client_update_required`.

This is a development-branch contract, not authorization to migrate production.
Production deployment and migration require separate approval. No viewing UI or
playlist swap control is introduced by these endpoints.

## Reviewed frontend source checkpoint — 2026-09-30

The dashboard submodule now pins ADM-002 management commit
`040ce6b96b117d073ca44cc4d751bae3d2a9659d`; the viewing submodule pins
`ecec4860e2c5940739e2ad2ef6e30eecfd7512b2`. Account management uses the v2
connection, matches and gateway contracts, while ordinary live viewing uses
the raw cursor catalog and exact source selection. Retired organizer/setup
controls are absent from the dashboard. This supersedes the historical notes
below about the old dashboard matches route and pending client guide adoption.
The follow-up dashboard pin also rejects repeated/overlapping management pages
and retains failed-refresh source identity; its 76 unit tests and HTTPS fixture
evidence remain client-side evidence, separate from backend runtime acceptance.
Backend legacy runtime removal remains a separate isolated checkpoint.

These are source pins, not deployment evidence. Dashboard acceptance used
mocked APIs over local HTTPS; TV-web passed local unit/browser checks.
No production database or public website was modified. See the pinned repos'
validation records for incomplete real-gateway and physical-device gates.

## Exact live playback and personal guide subsets

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

Checkpoint: full backend suite 281 passed/four opt-in fixtures ignored, strict
all-target Clippy passed. The 13 catalog/ownership fixtures also passed after
extending exact live selection through native direct v2 admission and release.
Synthetic tests cover profile-bound saved subsets, default/override isolation,
hidden foreign/unassigned/composite IDs, parent gates, private source cards and
credential-proof changes. No real provider, hardware or production state was used.
Ordinary client guide/catalog cutover and retired-path removal remain pending.

## Account management API

### Private source headers checkpoint — 2026-09-30

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
checks do not qualify real gateway media playback or hardware. This checkpoint
does not remove the pre-existing version-zero plaintext credential fallback;
offline migration/retirement evidence remains a separate operational gate.

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
Legacy refresh paths reject encrypted sources instead of invoking their old
pool/lineup logic. Admin/client adoption and legacy removal are still pending.

Each handler revalidates the captured account session before querying or writing.
Selected kids profiles require parent authorization; paired TV/device sessions
cannot manage account connections. Operator accounts still see only their own
assigned providers. Viewing-catalog APIs will use their own playback/profile
authorization rather than treating these management routes as TV browse routes.

The default stays unchanged while its provider remains enabled for live. If it
becomes unavailable, the next default read persists the oldest enabled owned
provider. No enabled sources means catalog_id is null. An explicit foreign or
disabled selection fails, without changing the existing default.

Legacy management and viewing routes have not yet been removed. The current web
UI still uses its old matches route until the admin rebuild adopts this contract.
Do not mistake this checkpoint for complete multi-tenant playback isolation.

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
tool. The executable is also included in future backend image builds; packaging
was updated, but a new backend image has not been built/deployed at this checkpoint.

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

There is no automatic restore or destructive cleanup. Invalid maps roll back the
source transaction, including newly initialized v2 schema; successful assignment
does not delete accounts, profiles, progress, matches or retired configuration.

Do not replace a live database with an older backup: that can discard later
viewing history. Any restore requires stopped writers, separate approval and a
fresh preservation backup of the current database. Likewise, do not roll a public
multi-tenant installation back to legacy code that ignores account ownership.
Full coordinated rollback remains acceptance work before production cutover.

## Offline provider credential encryption

This command is implemented and fixture-tested, **not approved for production
use yet**. Client cutover and retired-feature
removal remain incomplete. Migrated connections cannot be managed by legacy
renew/update/delete/pool operations; those paths reject them with
`client_update_required`. Legacy family matching and external provider XMLTV
paths must not be used after this step. Raw v2 catalogs, native Xtream guide
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

Missing/wrong keys, changed ownership and damaged ciphertext fail closed.
Running the new backend supplies the vault to the provider reader. Native
direct admission for migrated providers is independent of legacy cross-provider
pools and retains the configured connection allowance. HTTP provider URLs are
not upgraded or rejected merely for using HTTP.

This encrypts the provider credential tuple, not the entire SQLite database.
The **backup and export intentionally contain plaintext**. Retired configuration,
external copies, logs, filesystem snapshots and physical storage remnants are not
securely erased by SQLite compaction. Keep artifacts private and apply the
operator's retention policy; never commit or upload them. Client adoption,
operator-managed private-network exceptions, bulk key rotation and cutover are still
pending, so this is not a complete encrypted-secrets acceptance claim. Addon
storage and reviewed ownership/encryption commands are now covered by
[the addon management/migration contract](ADDONS_V2.md).
