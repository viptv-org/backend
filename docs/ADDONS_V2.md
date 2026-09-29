# Account-owned addon secrets

This is an implementation checkpoint, not a completed client cutover or approval
to migrate production. Addon network-policy hardening, guarded v2 management
adoption, bulk key rotation and removal of legacy paths remain open.

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
New encrypted installations do not reuse deleted addon IDs. A dedicated addon
payload bound preserves the existing 32 MiB manifest response allowance plus URL
overhead without increasing the normal 256 KiB gateway/provider-secret limit.

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
These use synthetic data; no production migration or device/UI acceptance is
claimed. Protected addon transport and complete client adoption remain required.
