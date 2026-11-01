# Profile-scoped discovery controls

`GET /api/discovery-controls?type=movie|series|anime` describes the selected profile's single metadata owner. Stremio catalogs carry raw `id`, `type`, `connection_id`, numeric `addon_id`, friendly `name` and additive `connection_name`. IDs are unique only within a connection and media type. Select the explicit tuple; never infer provider identity from labels or an opaque ID.

Catalog `controls` use canonical `QueryControl`. `maximum_selections:null` adds no provider-specific constraint; MultiChoice has a global64-value bound and SingleChoice always allows at most1. Stremio missing `optionsLimit` means1, explicit0 is not selectable, and advertised lists with limit>1 become MultiChoice with effective cap min(limit,64). Required flags and advertised defaults survive both original and normalized manifests. Empty advertised lists remain Text, rather than fabricated multi-choice menus.

Existing scalar `genre` and JSON `extras:{"sort":"year"}` remain valid. New clients may submit a JSON-encoded `values` GET parameter:

```json
[{"id":"genre","values":["Action & Adventure","Drama, Crime"]}]
```

Use `/api/discover?type=movie&catalog=<raw-id>&addon_id=<advertised-number>&skip=0&values=<encoded-json>`. Native `LocalEngine.planDiscover(profile,queryJson)` accepts the same optional array inside its JSON object, with `connection_id` instead of numeric `addon_id`. Method signatures are unchanged. Empty/missing array preserves scalar behavior. The parameter has an8192-byte decoded bound; native query JSON has a16384-byte bound. At most16 custom selection IDs and64 values per selection; individual values max1024bytes and must match advertised options where present.

Arrays require an explicit catalog and connection owner. Hosts reject duplicate IDs/values, unknown or foreign controls, scalar/vector conflicts, excessive selections and missing required inputs before provider fetches. Catalog switches must clear incompatible selection/search state or submit freshly validated controls. Optional empty selections are removed; a verified advertised default is supplied when present. Keep the complete selected query unchanged while advancing skip using the returned cursor.

The shared planner emits one catalog request with repeated individually escaped keys, e.g. `genre=Action%20%26%20Adventure&genre=Drama%2C%20Crime`. Commas and ampersands inside values are preserved. There is no CSV joining or per-selection request fanout; combined-choice matching semantics belong to the addon. Aggregated text search remains its existing bounded contract.

Home catalog rows submit the selections their catalog requires. A row for a catalog with a required extra carries that extra's advertised default, or else its first advertised option, in the row's browse action and in the row request itself, so the row and its retry issue the same satisfiable call. A catalog whose required extra is not selectable (`optionsLimit` 0), advertises no option, or requires a search term never becomes a row: no client can satisfy it, so no permanently failing row is published.

Legacy type-only defaults remain: remote chooses the first satisfiable catalog; native can merge the first satisfiable catalog from each assigned connection. A single explicit catalog/owner gives consistent one-request pageable behavior on both hosts.

Simkl currently advertises scalar genre/timeframe controls over its bounded public trending feeds. Nonempty array selections are rejected; no undocumented network, exclusion or advanced discovery parameters are sent. Its official V1 genre API documents single-value path segments and asks TV/console clients to await richer V2 results. Library/watchlist filters are a separate contract.

Focused synthetic qualification covers raw/normalized control parity, required/default/zero/large limits, escaped repeated keys, cursor preservation, scalar compatibility, native connection/profile fences and API rejection with zero upstream calls. These are host contract tests, not deployed or physical playback claims. See [official Stremio manifest](https://github.com/Stremio/stremio-addon-sdk/blob/master/docs/api/responses/manifest.md), [official repeated-key transport](https://github.com/Stremio/stremio-core/blob/master/src/types/query_params_encode.rs), and [current Simkl OpenAPI](https://api.simkl.org/openapi.json).
