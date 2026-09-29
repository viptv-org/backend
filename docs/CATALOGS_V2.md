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

## Source discovery and native Xtream guide

- `POST /api/v2/streams` starts an incremental discovery job using the existing
  Stremio-style movie/series/live request and returns `{id}`.
- `GET /api/v2/streams/:id?after=N` returns `{events,done}`. Events retain their
  monotonic `seq`, producer and sanitized stream cards with backend-issued IDs.
  The same exact account/profile/session owns the job. V2 currently uses polling;
  no new SSE endpoint is claimed.
- `GET /api/v2/iptv/guide/:channel_id` returns bounded Xtream-native programs for
  an owned raw channel. Foreign, absent, disabled and retired family-channel IDs
  receive the same `source_not_found` response.

These routes require a selected profile and currently a parent unlock for
restricted profiles, as raw catalog pages do. Source discovery considers all
enabled providers owned by the account, never just its default live catalog.
`only_provider_id` narrows that set but cannot grant access. Addons use the
existing account-scoped addon service. Source IDs feed the v2 playback API.

Candidate and sparse-detail SQL apply ownership before materialization; sparse
detail limits cannot be consumed by other tenants. Queued detail requests check
ownership, scope and credential freshness before fetching. Cached reads/writes
and late publication recheck ownership. Polling redacts a revoked provider's
previously published event while retaining its sequence position. Scoped live
lookup bypasses family-lineup mapping and uses the provider-qualified channel.

An in-process HTTP fixture verifies movies and exact episodes from three owned
providers with a different live default, while a foreign and an unassigned
provider receive no detail requests. It checks opaque source cards and original
HTTP episode URLs internally, cross-account job denial, and cached-result
redaction after revocation. A controlled in-flight revocation fixture verifies
that late series results neither publish streams nor enter the detail cache.
Additional fixtures cover sparse limits and owned raw guide reads.

This does not finish account-owned Xtream: account-scoped background refresh,
child-policy migration, detailed upstream error parity, client adoption and
removal of legacy routes remain pending. Legacy global routes still exist until
coordinated cutover; these v2 checks are not a claim that old clients are isolated.
No production database or live IPTV subscription was used for these fixtures.

An offline provider-tuple encryption migration and reader are now implemented;
see [V2 operations](V2_OPERATIONS.md). The migration is not approved for
production use yet. Account-owned connection CRUD is implemented as documented
there, but addon encryption, background refresh and client adoption remain open.
