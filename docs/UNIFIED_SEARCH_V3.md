# Unified text search (experimental v3)

Unified search uses every enabled Search capability assigned to the selected profile. It does not change the profile's browsing plugin or Discovery/Metadata defaults. Simkl remains server-only.

## Search

`GET /api/search?query=<encoded text>` requires an authenticated session and selected profile. Trimmed text must contain 1–256 characters and no control characters. Unknown query parameters are rejected.

The response is `{ "rows": [...], "complete": true|false }`. Each row contains:

| Field | Meaning |
| --- | --- |
| `id` | Stable `simkl:anime`, `simkl:movie`, `simkl:tv`, or `addon:<addon>:<kind>:<catalog>` |
| `title` | Anime, Movies, TV Shows, or the catalog's name and type, for example `Popular Movies` |
| `provider_id` | `simkl` or `stremio` |
| `addon_id` | Numeric add-on ID, or null for Simkl |
| `state` | `Ready`, `Empty`, or `Failed` |
| `cards` | Existing engine Card objects; no generated DTO changes |
| `message` | Null, or a safe retry message when some results failed |

Simkl has three independent category rows, with up to 24 results each. Up to 32 assigned Stremio add-ons contribute one row per search catalog resource: a movie and a series catalog that share an ID are separate rows, so are additional catalogs of one add-on, and identical catalog IDs in different add-ons never share an identity. A row retains up to 200 distinct `(kind,id)` results from its own catalog, so a title another row also holds stays in both. Equal titles in different rows remain distinct. Catalogs requiring unavailable extra selections are not guessed.

A row title names the catalog and its type (`Popular Movies`), and adds the add-on name, or failing that the catalog ID, when another row would otherwise share the title. Titles are decided across every planned row before any of them load.

Each row has a four-second budget from its start that covers the provider fetch, card projection, ranking and metadata-origin stamping, and only its own catalog can fail it. The whole request answers within ten seconds, below the clients' fifteen-second request timeout, and a row that misses the deadline comes back `Failed` with the retry message instead of holding the response. A partly malformed provider page can be `Ready` with its cards and a message; a valid empty result is `Empty`; a failed result with no cards is `Failed`. Provider errors and reflected response bodies are not exposed. `complete` means all planned rows completed without provider failures; it does not promise an exhaustive provider catalog or a total search-result count. This endpoint has no pagination cursor.

### Progressive delivery

`GET /api/search/stream` answers the same query with `text/event-stream`; rows reach the client as each provider finishes instead of after the slowest one. A row still fails at its own four-second budget, and every planned row answers within the ten-second search deadline, so the stream completes without waiting on a provider that hangs. `GET /api/parent/search/stream` is the parent-scoped equivalent. Both accept the same query parameters and apply the same identity, scope, capability, profile, kids-policy and revision checks as their snapshot endpoints. A request that cannot be answered at all (empty or over-long query, wrong profile, refused parent scope) still fails before the stream starts, with the snapshot endpoint's status and body.

Each event is `event: <name>` plus one `data:` JSON line:

| Event | Data | Meaning |
| --- | --- | --- |
| `plan` | `{"rows": [...]}` | Every planned row in display order, before any provider answers. Rows carry `state: "Loading"`, empty `cards` and a null `message`; parent rows carry empty `items` and no `cards`. |
| `row` | A complete row object | Sent once per row, in completion order. The object has the snapshot row shape for the same `id`, so a client replaces the planned row with it; ids are unique per stream. |
| `done` | `{"complete": true\|false}` | Every planned row was sent and authority still matched. `complete` has the snapshot endpoint's meaning. |
| `error` | `{"status": <code>, "error": <message>}` | The lease, profile policy, configuration revision or add-on revision changed mid-stream. No `done` follows, and any row sent before it came from the authority captured at `plan`. |

The plan order never changes, so a client renders pending rows where they will stay and does not reorder or duplicate them when a later row arrives. Cancelling the request stops delivery: the remaining provider work is dropped unsent. The snapshot endpoints remain unchanged for clients that do not consume events.

Credential refresh is a separate bounded transaction: once begun, a canceled search cannot discard a rotated Simkl token before sealing it. Refresh admission waits at most 15 seconds; its exchange/persistence phase has a 20-second bound. Existing lease, profile, capability, configuration and link-revision checks still apply.

## Metadata provenance

Cards in Simkl rows retain `provider_id=simkl`. Cards in add-on rows use `provider_id=addon:<id>`. Preserve that field through selection, details, episodes, saved library items and history.

Request details with:

- `GET /api/meta/<kind>/<id>?provider_id=simkl`
- `GET /api/meta/<kind>/<id>?provider_id=stremio&addon_id=<id>`
- `GET /api/meta/<kind>/<id>?provider_id=stremio` for generic Stremio metadata, including aggregated fallback recommendations.

`offset` and `limit` remain available. Without explicit provenance, the existing browsing-plugin routing remains unchanged. Details also accept `metadata_provider_id` and `metadata_addon_id` as aliases of `provider_id` and `addon_id`.

An explicitly named add-on is asked for the title even when its manifest does not list the requested type or ID prefix, because manifests under-declare what they serve. An add-on whose manifest lists no `meta` resource is not a metadata connector and cannot be named. When the named add-on cannot answer, details fail with `The selected metadata add-on did not return this title`; another add-on never answers in its place.

A provider document is the requested title when it returns the requested ID, or when it asserts that ID among its external IDs: `imdb`, `tmdb`, `tvdb`, `mal`, `kitsu`, `anilist` and `anidb`, including the underscore-prefixed cross-ids AIOMetadata emits (`_tmdbId`, `_kitsuId`, `_anidbId`, …). Asserted IDs are merged into the title's `identity.ids`, and a document that returns a different ID in the same namespace is a different title. Episode IDs may use a different namespace than the title (`mal:20` with `kitsu:11:N` episodes); they keep their own IDs and the title as `parent_identity`.

Explicit origins must be enabled, account-owned and assigned to the selected profile. A source add-on needs an enabled Search or Metadata assignment. Metadata can come only from assigned Metadata connectors. Prefer the source add-on's metadata capability when enabled; a search-only add-on may delegate to other assigned Stremio metadata connectors. Foreign, disabled or unassigned origins are refused before provider access.

Details, episodes and direct metadata recommendations retain the requested `addon:<id>` origin. Recommendations aggregated from other discovery add-ons retain generic `stremio` provenance.

## Source discovery

`POST /api/v2/streams` accepts optional `metadata_provider_id=simkl|stremio` and `metadata_addon_id`. The add-on ID requires Stremio. Preserve `series_id` from an episode's parent so a cache miss can fetch the correct parent metadata.

Provenance is validated before discovery and before SDK cache use. Untrusted client IDs remain lookup hints. An add-on's opaque identifier cannot invoke Simkl mapping merely because it starts with `simkl:`. Legacy requests without these fields retain their previous behavior.

Verified metadata keys include session identity, profile configuration revision, metadata origin, media kind/ID and account add-on revision. Home, details and continuation cache evidence under the revision captured before fetching it. Configuration changes cannot relabel an old response as current authority. SDK publication checks current configuration, lease, source proof and add-on revision.

## Isolation and verification

Every row uses account-scoped connections, profile assignments and existing kids-policy filtering. Final lease/configuration/add-on revision validation rejects the entire search response after authority changes, including rows fetched earlier. Provider failures alone affect their rows.

Regression coverage lives in `server/src/simkl/tests/unified_search.rs`, `server/src/simkl/tests/tokens.rs`, and the engine-v3 library/cache tests. It covers provider-default independence, foreign/disabled/unassigned origins, metadata delegation, sibling timeouts, malformed/empty/large results, equal IDs across metadata origins, canceled OAuth rotation, and mid-flight profile/policy/configuration changes.

Implementation and tests are experimental source changes. They do not activate a deployment, restart a live service, install an app, or modify production data.

### Local verification, 2026-10-15

The final backend test build used `--locked -j 1 --lib --features live-runtime,vsources-runtime`. Relevant filters on that build passed **104 tests**:

| Filter | Passed |
| --- | ---: |
| `simkl::tests` (including all seven grouped-search regressions and cancellation-safe OAuth rotation) | 67 |
| `auth::tests` (including device denial and refresh replay isolation) | 30 |
| `sources::metadata_tests` | 2 |
| `engine_v3::continuation::tests` | 3 |
| Existing exact-family library/Home metadata reuse regression | 1 |
| Delayed metadata/original add-on revision regression | 1 |

The source metadata batch covered 942 cases; the final recorded run took 284 ms. These are isolated synthetic-provider tests, not physical-TV, deployment or production-provider qualification.

## Continuation and subtitle provenance

`POST /api/profiles/:profile/continue/next` accepts the existing `identity` and `parent_identity`, plus optional `metadata_provider_id` and `metadata_addon_id` as on source discovery. Explicit origins use assigned metadata even when the browsing default differs. The returned next Card retains its origin. Current episode/parent coordinates must agree with provider metadata; upcoming, ambiguous, unknown or exhausted episodes do not manufacture a successor or start playback.

The shared continuation cache records metadata origin and captured profile/add-on revisions. A cached next episode from another add-on with the same opaque IDs, or from old configuration, cannot replace the current provider's continuation entry. Legacy cache records remain readable under their prior contract.

`GET /api/subtitles/:kind/:id` accepts the same optional metadata-origin fields alongside existing `videoHash`, `videoSize` and `filename` options. Subtitle-provider capability selection remains independent. An explicitly Stremio-owned ID resembling `simkl:` does not invoke Simkl identity mapping. Foreign/disabled/unassigned metadata origins are refused before provider access.

KMP uses a dedicated optional next-episode job, fenced by playback input, profile, connection, metadata origin and parent. It keeps playback running on lookup failure. Old-server 404 fallback and local paging use actual loaded/provider metadata identities only; no successor IDs are synthesized. An ended stream may retry unavailable next metadata, but playback always waits for an explicit user action.

### Broader backend verification

After the continuation/subtitle additions, the full backend library suite with `live-runtime,vsources-runtime`, one Cargo job and two test threads passed **456 tests, zero failures, ten pre-existing ignores**. All ten grouped-search/provenance cases passed. This supersedes the earlier targeted 104-test checkpoint for that source state; subsequent Live favorites/profile-age additions have their own gates.
