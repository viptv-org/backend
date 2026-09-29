# V2 account IPTV operations

This is a development-branch contract, not authorization to migrate production.
Production deployment and migration require separate approval. No viewing UI or
playlist swap control is introduced by these endpoints.

## Account management API

Authenticated account sessions use:

- GET `/api/v2/iptv/matches?provider_id=…&kind=movie&search=…&limit=50&cursor=…`
- PUT `/api/v2/iptv/matches` with `vod_id`, `metadata_id`, `type` (movie/series).
- GET `/api/v2/iptv/live-default`
- PUT `/api/v2/iptv/live-default` with `catalog_id`.

The matches response is `{items, next_cursor}`. No full-library count or candidate
array is produced. Limit is 1–200; default 50. Cursors are tied to account and
filters. Source URL/username/password fields are never returned here. Invalid
cursors and queries return actionable messages plus stable error_code values.

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
Full coordinated rollback and encrypted-credential migration remain acceptance
work before production cutover.
