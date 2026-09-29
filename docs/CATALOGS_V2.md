# Account-owned live catalog pages

Implements part of [BE-002](https://github.com/viptv-org/design/blob/25322b50df6511d6f8fd0de67522017b9ae388e1/BACKEND_V2.md).
These routes coexist with legacy routes on the v2 branch; clients have not cut
over and this is not a deployment claim.

## Viewing routes

- `GET /api/v2/iptv/live/channels`
- `GET /api/v2/iptv/live/categories`

Both require a valid account session and selected, authorized profile. Paired
devices can browse. Raw catalogs cannot bypass a restricted profile's parent
unlock: provider category names do not establish child suitability. Migration
of the child live policy away from retired family-lineup rules remains open.

Optional queries: `catalog_id`, `search`, `cursor`, `limit` (default 50, 1–200).
Channels additionally accept `category_id` using the original provider category
ID. Unknown parameters and invalid limits fail with `invalid_catalog_query`.
Search is a literal substring (SQLite's built-in case folding), not wildcard
syntax or linguistic/tokenized matching.

Response: `{catalog_id, generation, items, next_cursor}`. Channel items contain
`id`, `name`, `logo`, `category_id`, `category`, `epg_channel_id`. Category items
contain `id`, `name`. Nullable provider fields stay nullable. No whole-library
count, provider credentials, stream URLs, or implicit global catalog is returned.
An account without a live provider receives empty items and null metadata.

The default is persisted independently of per-request overrides. Explicitly
unavailable, disabled, foreign and nonexistent catalog IDs have the same 404
response. The channel list never merges providers or applies legacy family,
region, pool or channel-repair rules. Responses retain original logos, including
HTTP URLs; browser artwork transport still requires client adoption/verification.

Opaque page tokens bind account, requested catalog/filter, route kind, resolved
catalog and snapshot generation. They are positions, not authorization grants;
every request rechecks ownership and enabled state. Changing filters or using a
token from another account/route yields `invalid_cursor`. A refresh or changed
default yields 409 `catalog_changed`; clients must discard the old list/token and
restart. Changing the page size is supported. There is no silent mixed-snapshot
continuation.

## Storage and refresh

An additive schema records per-channel ordinals, category IDs/order and a live
generation. Existing rows retain their insertion order until the next provider
refresh; absent historical category ordering cannot be reconstructed exactly.
Fresh imports preserve stream-array and category-array order. Unlisted category
IDs referenced by channels are retained after the supplied category list.

Network fetches finish before replacement. Channel rows, category rows and the
generation increment commit in one transaction. Failure rolls all three back;
unfetched/disabled scopes retain their previous snapshot. Empty successful
snapshots still increment the generation. Page queries hold a single database
transaction, filter before `LIMIT + 1`, and use `(ordinal,id)` keyset positions.
Only a bounded result is materialized, without an offset or count query.

## Evidence and remaining work

In-process route fixtures cover account isolation, default/override/fallback,
235-channel traversal without duplicates, page bounds, original HTTP logos,
category order, changed-snapshot refusal, devices, selected-profile checks and
restricted-profile protection. Storage fixtures cover repeatable additive schema,
provider ordering, successful/empty refresh generations, and rollback after a
duplicate stream causes an insert failure following deletion.

This does not finish account-owned Xtream: connection CRUD/encrypted credentials,
owned multi-provider VOD discovery, v2 channel source/guide integration, child
policy migration, client adoption and removal of legacy routes remain pending.
No production database or live IPTV subscription was used for these fixtures.
