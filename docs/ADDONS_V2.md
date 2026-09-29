# Account-owned addon secrets

This is an implementation checkpoint, not a completed client cutover or approval
to migrate production. Protected public-network requests and guarded v2 management
are implemented; client adoption, operator-managed private-network exceptions,
bulk key rotation and removal of legacy paths remain open.

## V2 management and protected requests

- GET `/api/v2/addons?limit=50&cursor=…` returns `{items,next_cursor}`, with a
  maximum of 200 and account-bound cursors. Records redact manifest URLs and
  include bounded logo metadata when available. A locked/unreadable configuration
  gets a safe per-record error rather than leaking its URL or blocking the list.
- POST `/api/v2/addons` accepts only `manifest_url`. It validates and downloads
  the manifest, rechecks the captured authorization, and encrypts it before save.
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

The unkeyed legacy path still exists until cutover. It rechecks encryption state
inside its save transaction, so a late legacy request cannot create plaintext
after an account becomes protected. This does not constitute removal or complete
isolation of every legacy route.

## Storage and runtime behavior

The addon manifest URL can contain a token, and the manifest can repeat it in
logos, links or configuration data. Both are sealed together with the operator
keyring, authenticated against the account, addon ID and `addon` purpose. Names,
IDs, enabled flags and account ownership remain ordinary metadata: this is not
whole-database encryption.

With a configured keyring, new account-owned installations use encrypted storage.
Reinstalling an already encrypted URL preserves its ID, while replacing the
manifest advances a small configuration revision. Playback checks use that
revision rather than loading/hashing a potentially large encrypted manifest.
New encrypted installations do not reuse deleted addon IDs. Addon downloads are
bounded at 32 MiB; encrypted addon documents have a separate 33 MiB serialized-
payload limit. Oversized documents fail rather than falling back to plaintext.
Normal gateway/provider secrets retain their 256 KiB limit.

Settings responses use `credentials_encrypted:true` and `manifest_url:null` for
encrypted entries. Internal, account-scoped catalog/source readers decrypt only
when needed. Clients must adapt URL display/edit flows; the current old UI is not
claimed to support this contract. Missing keys, wrong owners or damaged/missing
ciphertext do not fall back to plaintext.

An account that has enabled encrypted addon storage cannot create new plaintext
entries by removing the keyring, even after deleting all of its addons. Re-adding
a legacy plaintext URL does not silently migrate it online: it returns
`addon_encryption_required`, preserving the backup-first boundary. Compatibility
unkeyed writes still exist for unmigrated accounts and are pending removal at the
coordinated cutover; do not claim every installation is encrypted already.

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
These use synthetic data; no production migration or device/UI acceptance is
claimed. Complete client adoption and the remaining cutover gates are still open.
