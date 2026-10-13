# Canonical episode windows

`GET /api/v3/meta/:kind/:id?offset=0` returns at most 24 exact episode cards and the next global cursor. `offset` is forwarded through the generic selected-provider adapter; it does not require a provider-specific client endpoint.

`DetailSnapshot.seasons` is additive, defaulting to an empty list for old responses. Each entry contains `season` (nullable), `episode_count`, and `offset`. Offsets refer to the same canonical sequence used by episode windows. Explicit seasons are ordered numerically, including Season 0; unknown-season episodes form a final null group. Identifiers and parent identity remain unchanged. The index comes from the already-fetched provider episode document, so it requires no extra source/provider reads. Server policy filtering occurs before index construction and paging.

A client can select a season by requesting its offset, retain and deduplicate previously fetched cards, then request `season.offset + loaded_count` until the selected season's known count is loaded. A 24-card window may include the beginning of the next season; those cards remain valid and can be retained. The next global cursor alone does not imply that the selected season has more episodes.

Native `cachedDetail` can derive an unvisited window from the scoped body already supplied to `projectDetail`. It retains at most four series documents (8 MiB), expires them after five minutes, validates configuration scope, and registers returned exact episode cards for subsequent source admission. This avoids re-downloading the full addon metadata document for each season/page. No new UniFFI method or signature is needed.

Qualification: real Naruto generic API pages expose 220 verified episodes across five seasons; Season 2 starts at offset 35. Synthetic tests cover unordered provider data, duplicate IDs, unknown season, specials, terminal cursor, profile authority, and one provider episode read across multiple server windows. Actual user/library data remains in private local proof artifacts.
