# Account-owned addon secrets

Current contract at backend `8823114`: runtime addon credential readers and
authenticated addon installs are encrypted-only. This supersedes the historical unkeyed
installation/read-fallback descriptions, not the retained metadata compatibility
wrappers below. Stored legacy records remain preserved for explicit offline
migration; runtime does not silently encrypt, assign owners or read their secrets.
Reviewed client source adoption is recorded in [V2 operations](V2_OPERATIONS.md),
not inferred from these fixtures. Production cutover, operator-managed private-
network exceptions and bulk key rotation remain separately unqualified.

## V2 management and protected requests

- GET `/api/v2/addons?limit=50&cursor=…` returns `{items,next_cursor}`, with a
  maximum of 200 and account-bound cursors. Records redact manifest URLs and
  include bounded logo metadata when available. A locked/unreadable configuration
  gets a safe per-record error rather than leaking its URL or blocking the list.
- POST `/api/v2/addons` accepts only `manifest_url`. It validates and downloads
  the manifest, rechecks the captured authorization, and encrypts it before save.
  A missing keyring refuses before download or write. Existing owned credentials
  are checked before this v2 install fetch; legacy plaintext is not converted
  online.
  A concurrent deletion/reconfiguration conflicts rather than resurrecting a
  deleted source or overwriting a newer manifest.
- PATCH `/api/v2/addons/:id` accepts `enabled`; DELETE removes that owned addon.
  Writes require an account session, not a paired viewing-device session.

Paired devices may read redacted metadata/icons with a selected, authorized
profile. Parent restrictions still apply. Logo metadata belongs to the authorized
account and may itself contain addon-issued access data; responses remain private
and non-cacheable. Image rendering, HTTP artwork delivery and platform UI adoption
are not proved by the metadata fixtures.

V2 discovery and configured-key addon requests use shared protected JSON egress.
HTTP and HTTPS are supported. Each request/redirect resolves, validates and pins
all destination addresses. Private/reserved destinations, embedded URL userinfo,
fragments and HTTPS-to-HTTP downgrades are rejected. Up to ten public redirects
are supported, without automatically copying the original query credentials,
cookies, Authorization or Referer. Host proxy environment settings are ignored.
Xtream callers retain their separate no-redirect policy.

Protected caches/flights are account-scoped and distinct from legacy fetches;
they cannot reuse a response obtained through the old network policy. Manifest
installs/checks have two slots separate from twelve viewing-fetch slots, so slow
installs do not occupy the browsing pool. Downloads retain response-size and
timeout bounds. Test loopback exceptions are explicit and compiled for fixtures,
not end-user configuration.

The retained `/api/addons` GET/PATCH/DELETE wrappers preserve their array/record
metadata shapes, account scope and parent/session policy. GET always redacts
`manifest_url`; PATCH changes only `enabled`, and DELETE removes an owned entry.
These metadata operations can inspect, disable or remove archived legacy rows
without decrypting them; enabling one does not make its credentials usable.
The retained POST wrapper also requires a keyring before downloading and writes
only sealed credentials. Its route name is not an unkeyed installation mode.
These wrappers, plus `/api/catalogs`, `/api/discover` and `/api/meta`, are distinct
from retired media/setup/organizer namespaces that return `client_update_required`.

## Storage and runtime behavior

The addon manifest URL can contain a token, and the manifest can repeat it in
logos, links or configuration data. Both are sealed together with the operator
keyring, authenticated against the account, addon ID and `addon` purpose. Names,
IDs, enabled flags and account ownership remain ordinary metadata: this is not
whole-database encryption.

All new account-owned installations require a configured keyring and encrypted
storage.
Reinstalling an already encrypted URL preserves its ID, while replacing the
manifest advances a small configuration revision. Playback checks use that
revision rather than loading/hashing a potentially large encrypted manifest.
New encrypted installations do not reuse deleted addon IDs. Addon downloads are
bounded at 32 MiB; encrypted addon documents have a separate 33 MiB serialized-
payload limit. Oversized documents fail rather than falling back to plaintext.
Normal gateway/provider secrets retain their 256 KiB limit.

Settings responses use `credentials_encrypted:true` and `manifest_url:null` for
encrypted entries; compatibility metadata also returns `manifest_url:null` for
legacy rows. Internal account-scoped catalog/source readers decrypt only when
needed. Version-zero credentials fail with `source_credentials_migration_required`;
missing keys, wrong owners or damaged/missing ciphertext fail safely, never
falling back to stored plaintext. V2 management lists expose per-record safe
configuration errors; compatibility catalog arrays contain only usable sources
and preserve their existing shape. See the reviewed client pins rather than the
historical old-UI checkpoint for adoption evidence.

Removing the keyring cannot reopen plaintext installation, including for an
unmigrated account or one that deleted every addon. Re-adding a legacy URL does
not silently migrate it online: strict credential inspection returns the safe
migration/configuration refusal and preserves the backup-first boundary.
Encrypted-only runtime behavior does not mean every stored legacy row has been
migrated or that metadata wrappers must be removed.

V2 discovery rechecks addon ownership/enabled state before publishing late
results and when returning cached producer events. Revoked entries are redacted
without changing sequence positions. This supplements playback authorization;
it does not make legacy routes or native clients v2-compliant automatically.

## Reviewed offline migration

The historical `provider-owners` executable also handles addon ownership. Inspect
read-only first:

```sh
provider-owners inspect-addons /absolute/path/source.sqlite
```

Inspection returns only unassigned IDs and existing ID/account pairs. Legacy
initialization no longer grants unassigned addon secrets to the first owner-role
account. Supply an explicit complete map for unassigned addons (for example,
`{"7":11}` uses illustrative IDs), retaining existing assignments:

```sh
provider-owners apply-addons /absolute/path/source.sqlite \
  /private/new-run/addon-owners.json \
  /private/new-run/before-ownership.sqlite \
  /private/new-run/before-ownership.json \
  FULL_40_CHARACTER_BACKEND_SOURCE_COMMIT --confirm-ownership

provider-owners encrypt-addons /absolute/path/source.sqlite \
  /private/new-run/before-encryption.sqlite \
  /private/new-run/before-encryption.json \
  FULL_40_CHARACTER_BACKEND_SOURCE_COMMIT --confirm-encryption
```

Use a private owned directory, fresh artifact paths, stopped backend processes
and the reviewed operator `VIPTV_SECRETS_KEYRING`. Follow the backup, permission
and rollback rules in [V2 operations](V2_OPERATIONS.md). Known pre-account addon
schemas can be converted inside the ownership transaction; unknown extra columns
fail rather than being silently discarded. Assignment requires valid active
accounts; encryption refuses unassigned or missing owners. Existing disabled
accounts retain ownership of their own encrypted records.

Both mutations create and sync a complete SQLite backup and private export
before changing the source. Encryption preserves IDs, flags, manifests and
ownership, replaces plaintext URL/manifest columns with markers, then performs
offline database/WAL cleanup. Re-runs validate ciphertext and use new artifact
paths. Backups and pre-encryption exports intentionally still contain secrets.

Compaction also regenerates the external-content VOD search index. SQLite warns
that [VACUUM can change implicit row IDs](https://www.sqlite.org/lang_vacuum.html);
its [FTS5 rebuild command](https://www.sqlite.org/fts5.html#the_rebuild_command)
recreates the derived index from the current content table. Stable public VOD IDs
and manual matches are not rewritten.

If cleanup fails after commit, `encryption_committed_cleanup_required` means the
encryption transaction already committed. Do not assume rollback or restore an
old database over new history. External backups, snapshots, logs and physical
storage remnants are not securely erased by SQLite cleanup.

## Evidence

Fixtures cover owner binding, redacted settings, preserved IDs on reinstall,
non-reuse after deletion, no missing-key downgrade, larger encrypted manifests,
legacy ownership without implicit grants, complete-map enforcement and rollback,
private backup/export contents, compaction/search rebuild, HTTP installation,
late/cached source revocation and executable confirmation/keyring gates.
Additional fixtures cover v2 management roles/cursors, private-address rejection,
public and cross-origin redirects, loops and downgrade refusal, stale reinstall
and revoked-session writes, cache trust separation and independent browsing slots.
These are synthetic checkpoint fixtures, not production migration or physical-
device/UI acceptance. Subsequent strict-read and legacy-POST refusal evidence is
recorded in V2_OPERATIONS.md; historical tests of unkeyed behavior do not define
today's contract. Client pin adoption, production migration/rollback, private-
network exceptions and key rotation must each retain their own evidence boundary.
