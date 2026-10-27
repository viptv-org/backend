# Simkl server integration (experimental v3)

Simkl credentials stay on the backend and are encrypted with the existing secret vault, bound to both account and profile. The server must have `SIMKL_CLIENT_ID`, `SIMKL_CLIENT_SECRET`, `VIPTV_SECRETS_KEYRING`, and its existing pinned HTTPS browser origin configured. Register `/api/v3/simkl/callback` on that origin as the Simkl OAuth redirect URI. Application secrets are never returned to clients. Configuration does not create a Simkl user link; the user must authorize each profile in a browser/dashboard session.

The server advertises only Discovery, Authorization, Search, Metadata, and Tracking for this adapter. Simkl is absent from bootstrap descriptors when the application or vault is unconfigured. A private metadata connection is available for each selected profile when the server is configured; tracking always requires user authorization. Some Simkl application configurations also require a linked user for search or metadata. A saved profile configuration must explicitly assign the corresponding Simkl capabilities; profiles without a saved override use their enabled connections. Profiles without an explicit saved browsing choice default to Simkl on a configured server and to Stremio otherwise. An explicit Stremio choice stays Stremio. PUT the existing profile integrations endpoint with `browsing_plugin` set to `simkl` or `stremio`; Simkl selection requires an enabled Discovery assignment.

## API

Every profile route requires a live VIPTV session with that exact selected profile. Parent restrictions apply. OAuth initiation and unlinking additionally require a browser account session, keeping credential management in the dashboard.

| Method and route | Input / result |
|---|---|
| GET `/api/v3/profiles/:profile/simkl` | `{configured, linked, connection, tracking: {pending, failed}}`; no credential material |
| POST `/api/v3/profiles/:profile/simkl/authorize` | `{authorization_url, expires_in_seconds}`; navigate in a browser |
| GET `/api/v3/simkl/callback` | Simkl `state`, exact `iss=https://simkl.com`, and `code` or `error`; redirects to `/?simkl=linked` or `/?simkl=cancelled` |
| DELETE `/api/v3/profiles/:profile/simkl` | Unlinks that profile locally and invalidates outstanding authorization states |
| GET `/api/v3/profiles/:profile/simkl/search?category=movie&query=…&page=1` | Engine `CatalogPage`; categories `movie`, `tv`, `anime`; 24 results/page, pages 1–20 |
| GET `/api/v3/profiles/:profile/simkl/meta/:category/:id?offset=0` | Engine `DetailSnapshot`; positive numeric Simkl title ID; Verified TV/anime episodes default to 24/page; optional `limit=1..128` |
| POST `/api/v3/profiles/:profile/simkl/watched` | `{category:"movie",id:123}` or `{category:"tv",id:456,season:1,episode:2}`; `{recorded:true,…}` only after upstream confirmation |

The explicit watched action and authenticated playback/history completion events can upload exact watched items. TV watch updates first verify the requested season/episode in Simkl's own episode list. Unnumbered specials and conflicting identities are not inferred. Movie IDs and episode IDs remain distinct in the engine's projections. Metadata calls validate the returned title ID. Disabling a profile capability, unlinking, changing the selected profile, expiring a session, or changing policy invalidates work before a queued request is sent and again before its result is returned. An upstream mutation already dispatched cannot be recalled.

OAuth state is random, stored only as a hash, expires after ten minutes, is single-use, and is bound to the originating session, account, profile policy, and link revision. The link token is not placed in Home, public metadata, or any client response. Public metadata operations first use the application key without a user token. When Simkl rejects that request for authorization, a linked profile may retry with its own encrypted OAuth token. Authenticated responses are never stored in the shared public metadata cache. Disconnect removes local authorization and invalidates pending work. It does not call an upstream revocation endpoint.

## Current limits and evidence

A configured application and a real user's authorization are required for private watchlist and tracking operations. Public v2 calendar, scoped CDN genre/timeframe controls, read-only linked-profile watchlists, verified anime episode projections, and movie/TV/anime source and subtitle mappings are implemented. Continuous inbound synchronization into VIPTV history/favorites, PRO/VIP custom-list routes, country/network/year discovery outside CDN rankings, and additional external mapping providers remain pending.

Anime episodes require the provider's explicit aired TVDB season/episode mapping; unnumbered or conflicting records are excluded rather than assigned a season. Source and subtitle lookup resolves authoritative IMDb title IDs plus verified aired coordinates. The original Simkl identity remains attached to playback authority, progress and library state. Missing or conflicting mappings return HTTP 422 with `metadata_mapping_unavailable` before addon lookup. Public title and episode documents share the bounded, coalesced one-hour metadata cache.

Anime progress and tracking are implemented with verified provider identities. Synthetic fixtures cover scrobble/history dispatch, missing mappings with zero writes, and exact film/episode responses. A read-only real Naruto document independently confirms S2 E1 maps to sequential episode 36; no real user history or scrobble writes were used for that mapping qualification.

The generic `/api/v3/bootstrap` and profile Home endpoints assemble Simkl Trending Movies, TV Shows, and Anime when Simkl is selected. One combined public `today_100.json` document serves the three rows, coalesced and cached for one hour without user credentials. Home retains its two-second assembly deadline and presents failed rows for slow or failed upstreams. `/api/v3/discover` routes movie/series/anime discovery and text search to the selected provider; unsupported controls are rejected. Generic `/api/v3/meta/:kind/:id` accepts projected identities such as `simkl:movie:123` and uses the selected provider. Explicit provider switches never merge Stremio and Simkl metadata in one browsing configuration.

The engine tests cover stable title/episode identities, missing and conflicting IDs, duplicate episode coordinates, type mismatches, and exact source mappings. In-process server fixtures cover routed OAuth, encrypted storage, account/profile isolation, expired/revoked/single-use state, disabled configuration, exact episode tracking, metadata mismatch, unlinking, default provider routing, public cache coalescing, OAuth-required metadata fallback without shared caching, explicit provider selection, bounded slow Home assembly, canonical playback identity, and mapped movie/TV addon source and subtitle requests. No production server or real account was modified.

API schemas were checked with Context7 `/simkl/api` against the official [authentication](https://github.com/simkl/api/blob/master/_autodocs/api-reference/authentication.md), [content discovery](https://github.com/simkl/api/blob/master/_autodocs/api-reference/content-discovery.md), and [sync](https://github.com/simkl/api/blob/master/_autodocs/api-reference/sync.md) documentation. Simkl's current documentation is at [api.simkl.org](https://api.simkl.org).

## Authenticated history and playback events

`PUT /api/v3/profiles/:profile/progress` accepts `{record: PlaybackRecord, parent_identity?: MediaIdentity, ended?: boolean}` using the existing shared engine types. Positions and durations are milliseconds. Server activity dates and the existing 95% completion policy are authoritative; client `updated_at_millis` and `completed_at_millis` are ignored. Known-runtime Ended records the actual duration as the final position. Unknown-runtime Ended retains the observed position and duration zero; it does not infer completion or queue external history tracking. Ordinary Stremio/opaque identities persist in selected-profile history without requiring Simkl. Episode parent identities remain opaque to clients; the server verifies Simkl parent IDs and exact aired coordinates before persistence. The existing seconds-based progress PUT remains supported.

Successful authenticated completion writes and explicit watched history corrections queue `/sync/history` only for verified Simkl movie, TV episode, anime film or anime episode identities with enabled Tracking and a linked profile. The queue has at most 128 pending operations, four workers, per-profile serialization, and a 30-second deadline. Durable receipts coalesce an item within one link revision; failed/unconfirmed work can retry on a later authenticated completion update after 60 seconds. No automatic rewatch flag is sent. Queued jobs are session/profile/policy/link/configuration-bound; the dispatch and response revalidate those facts. Local history saving does not wait for provider traffic. Profile Simkl status includes safe `tracking: {pending, failed}` receipt counts. Pending tasks live in process memory; after a restart, later authenticated updates can retry stale receipts rather than restoring an old session's mutation authority.

`POST /api/v3/profiles/:profile/playback-events` is a provider-neutral boundary. Its body is `{attempt_id, sequence, identity: MediaIdentity, parent_identity?: MediaIdentity, event: "start"|"pause"|"stop", position_millis, duration_millis, ended?: boolean}`. An attempt starts with sequence 1 and a start event; subsequent sequences increase. The attempt's media identity is immutable, replacement retires the old attempt, and queued superseded events cannot dispatch. A busy queue does not consume an event sequence or retire an attempt. Once an event is admitted, its intent supersedes older events even if provider processing later fails; repeated admitted sequences return conflict and a subsequent event uses a fresh increasing sequence. Provider authorization rejection returns `unavailable/tracking_authorization_required`, rather than a VIPTV session-expiry status. No provider or credential configuration returns `{state:"ignored",provider_id:"simkl",reason}` without provider traffic. An enabled Tracking connection without an authoritative mapping returns `{state:"unavailable",provider_id:"simkl",reason:"metadata_mapping_unavailable"}`. An unknown runtime is represented as unavailable except for start at observed position zero.

Supported Simkl lifecycle requests use `/scrobble/start`, `/scrobble/pause`, and `/scrobble/stop`; TV bodies contain `show.ids.simkl` and `episode:{season,number}`. Anime episode bodies use `anime.ids.simkl` and the exact provider sequential episode number with its documented season 1. That number is looked up by the verified Simkl episode ID, checked against its TVDB coordinates; list offsets and season arithmetic are never used. Verified anime films send `anime` without an episode block. Anime history uses the corresponding `anime[]` title or exact nested episode, and requires upstream confirmation. The shared server resolver rejects an ambiguous reverse mapping before dispatch: distinct regular episodes cannot share an absolute number or aired TVDB pair, and a repeated Simkl episode ID is not selected by row order. Specials with a different ID/type do not substitute for a regular episode even when their displayed number matches. Simkl stop normally completes at 80%, while VIPTV completes at 95%. This adapter maps incomplete stop events to pause; stop is sent only at VIPTV completion or an explicit known-runtime Ended event. Confirmed responses are `{state:"recorded",provider_id:"simkl",action,completed}` after the provider returns the matching movie/episode identity and action. Start/pause are not completion records. Timestamped history completion uses Simkl's supported `watched_at` field. No inferred whole-series completion is used. Anime film completion requires verified movie metadata, and episode completion never marks an entire anime series.

The server fixture suite verifies opaque Stremio persistence, server dates, 95% completion, receipt coalescing, queued unlink/configuration revocation with zero mutation calls, exact TV parent/episode checks, scrobble policy, and retired/replayed attempts. Application lifecycle wiring and real-user OAuth/device acceptance remain separate qualification. Current primary schemas come from the official [sync reference](https://github.com/simkl/api/blob/master/_autodocs/api-reference/sync.md) and [scrobble reference](https://github.com/simkl/api/blob/master/_autodocs/api-reference/scrobble.md), fetched through Context7 `/simkl/api`.

## Real-provider qualification (2026-10-13)

`scripts/start-real-provider-qa.py` starts the isolated real-provider runtime on loopback ports 18185/18445 and prints its private run directory. Build the debug server first; use the recorded process IDs for cleanup. The local `/qa/` page provides sign-in, profile selection, link status and the Simkl Connect action. Register its displayed callback (`https://viptv.local.test:18445/api/v3/simkl/callback`) in the Simkl application settings before user consent; do not replace a production callback just to run QA.

An isolated server with a fresh database, keyring and owner profile returned real Simkl Home rows and movie/TV/anime detail over trusted local HTTPS. Ted Lasso (`simkl:tv:1359610`) returned 24 episode projections; the first episode resolved to 140 stream candidates from three existing configured Stremio addons. Only addon configuration was read from the old disposable QA database and re-registered through the authenticated API. No account/history or Simkl token was copied. The first PenguPlay source reached native direct-ready playback admission; an independent media probe confirmed H.264 1920×960 video, AAC audio and 1855.968 seconds. Native Kite rendering remains a separate acceptance step.

The configured client requires user authorization for search. After the user registered the isolated callback and consented, a fresh device session selected profile 1 and verified `linked: true`, movie search returned 24 Interstellar matches, TV search returned one Ted Lasso match, and typed history returned an empty list. Every request and logout returned HTTP 200. This read-only qualification made no progress or tracking writes; it does not qualify production deployment or native media rendering.

## OAuth2 token lifecycle

The backend uses `https://simkl.com/oauth2/authorize` and `https://api.simkl.com/oauth2/token`. Server-side PKCE uses S256 with an encrypted verifier; the callback requires the exact issuer `https://simkl.com` on both success and cancellation. Token exchange and refresh use form encoding and request `media:read media:write`. Access and refresh tokens, granted scope and access expiry are stored together in the account/profile-bound encrypted vault. A process-wide refresh owner coalesces concurrent expired-token reads, rechecks session/profile/configuration/link authority before dispatch and after the response, and preserves the link revision during refresh. Unlinking while a refresh is queued prevents dispatch; unlinking during an already dispatched refresh prevents restoring credentials. Missing write scope, invalid expiry and malformed tokens are rejected before storage. Existing legacy token links remain readable; new links use the OAuth2 document. A horizontally scaled deployment needs a shared refresh owner or separate grants per instance.

Current primary contracts are the official [OAuth2 browser flow](https://api.simkl.org/api-reference/oauth2-authorization-code) and [token lifecycle](https://api.simkl.org/api-reference/oauth2-tokens), fetched from the current official `llms-full.txt`; older indexed examples still describe `/oauth/token`. The client performs proactive refresh near stored expiry. Upstream authorization rejection remains an honest provider failure; it never expires the VIPTV session or silently changes the user's consent.

## Secure direct admission and real gateway qualification

Native direct preparation performs a bounded media range probe before returning an authorized delivery: at most 32 concurrent probes, eight seconds including capacity/DNS/IO, four redirects, and validated/pinned public DNS addresses. TLS verification remains enabled. Each hop rechecks session/profile/policy/source authority. Cross-origin redirects remove all declared source headers; an HTTPS-to-HTTP redirect is classified before connecting and is never rewritten to an invented HTTPS URL. Unsafe, unavailable or unqualified direct transport selects an enabled account-authorized gateway; without one it fails explicitly rather than publishing an unchecked direct delivery. Generic protocol tests inject an explicit test-only probe stub; `App::new` and the real qualification host use the actual transport.

The production playback-gateway at revision `6cb702c` was built and run in an isolated private directory with fresh scoped integration keys and trusted local viewer HTTPS. A fresh backend device session discovered the actual PenguPlay Ted Lasso source and forced gateway delivery. Real FFprobe/FFmpeg preparation produced HLS with H264/AAC copying; playlist and segment requests returned 200 and a TLS-verified media read decoded a video frame. Playback DELETE and logout returned 200. This was a real gateway service, not the earlier deterministic FFmpeg test peer. Native presentation acknowledgment is a separate application qualification. The source declared no request headers. Experiment gateway revision `7993210` now mediates header-bearing nonlive binary media through owned ranged capabilities; arbitrary headers are removed on every cross-origin redirect, and a real authenticated MP4 fixture qualified FFprobe/FFmpeg remux and frame decode without source credentials in child arguments. Header-bearing live, manifest and nonranged sources fail closed until their mediators are implemented.

Production gateway control currently requires public destinations and public certificate roots. The isolated `cfg(test)` host maps only control requests to the owned loopback production gateway while leaving its engine, media delivery, account registry, existing database/vault and OAuth unchanged. An operator-scoped trusted control-origin/CA policy for private self-hosted gateways remains pending; no global private-address or TLS bypass was added.

`POST /api/v2/playback` also accepts optional `resume: true` (default false). A zero-position nonlive request reads authoritative progress for the exact selected profile and the source grant's original canonical media ID, before transport/IMDb mapping. Explicit positive positions win. Completed or manually watched items restart at zero; unknown-duration incomplete items retain their actual observed position. Idempotent retries retain the initial admission snapshot even if progress later changes. The client cannot supply a replacement media ID or infer absolute positions from a card progress ratio.


## Typed library, calendar and provider controls

`GET /api/v3/profiles/:id/library?offset=0&limit=40` returns a typed `CatalogPage`.
`PUT` accepts `{ "card": Card }`; `DELETE /api/v3/profiles/:id/library/:kind/:item`
removes the exact canonical identity. These idempotent operations share existing
favorites rows with legacy clients, retain typed provider/mapping/parent metadata,
and fence the selected profile. Kids profiles store and read only approved
presentation data. Home's My List row uses the same typed cards. The local shared
engine has matching persistent, paged library operations and UniFFI methods.

`GET /api/v3/calendar?type=series|movie|anime&start=YYYY-MM-DD&end=YYYY-MM-DD&skip=0`
returns `{ entries, next_cursor, complete }`, with 100 entries by default and an optional `limit` of 1–500 per page.
Each entry contains a stable calendar `id`, a typed title `card`, UTC `date`,
`episode_title`, `season`, and `episode`. Calendar row IDs are separate from
playback identities; a TV schedule does not invent a provider episode ID, and
anime sequential coordinates do not become aired episodes. The default window
is today ±3 days and accepted windows span at most 33 days. One bounded public
v2 CDN document is coalesced/cached per type, without user authorization.
Session, selected provider, profile assignments and kids policy are rechecked.
An unsupported provider returns an explicit availability error.

The Calendar merges two sources. `library` entries are the released and
upcoming episodes of the profile's library series (`favorites` rows of type
`series`, so series a Stremio import saved are included), for any browsing
integration. Their air dates come from the episode documents Continue Watching
reads for new-episode facts, with its provider choice: the Simkl episode list
when Simkl is the browsing integration, otherwise the metadata add-on's
`videos[].released`. Only when that provider cannot serve a series (a `tt…`
Stremio import without a Simkl mapping in a Simkl profile, or a `simkl:`
series in a Stremio profile) is the series' own provider read instead.
`public` entries are Simkl's public schedule above, only when Simkl is the
browsing integration. A public episode of a library series (any shared
identity such as `imdb:tt…`, plus the same season/episode, or the same UTC day
when coordinates are missing) appears once, as `library`, also when the
library shows it under another `type` (a `tt…` anime series is in the series
calendar, so its public anime copy is dropped). Library series feed
`type=series` (Simkl `simkl:anime:` series feed `type=anime`); `type=movie` has
public entries only. Per profile, at most 50 library series (most recently
added first) are looked up, four at a time with an 8-second limit each and a
12-second total deadline. The episode list (episodes within 180 days of now)
is cached for 30 minutes, keyed by account, configuration, add-on revision and
the set of library series, and concurrent reads share one fetch; a list with a
failed or unfinished lookup is kept for 15 seconds only. Kids (restricted)
sessions get no library entries and start no lookups. The route is available
when the profile has library series or Simkl is the browsing integration,
matching bootstrap navigation's `calendar` destination and the
discovery-controls `calendar` availability; otherwise it returns 422. When one
source fails (other than session, permission or configuration changes), the
other source's entries are returned and `sources` omits the failed one.

Wire shape. All additions are optional for older clients; existing fields and
public entry ids are unchanged:

```json
{
  "entries": [{
    "id": "stable calendar row id",
    "card": { "...": "typed series Card (library card, or Simkl title card)" },
    "date": "2026-10-28T04:00:00Z",
    "episode_title": "Chapter 2",
    "season": 1,
    "episode": 2,
    "source": "library | public",
    "episode_card": { "...": "library only: typed episode Card, parent_identity = the series" }
  }],
  "next_cursor": "100",
  "complete": false,
  "sources": ["library", "public"],
  "refresh_after_millis": 15000
}
```

`sources` lists the sources that answered. `refresh_after_millis` is present
only while some library series are still being looked up (a cold fetch hit its
deadline). The list can still grow then, so `skip` cursors are not stable:
after that delay, read the window again from the first page. Without it, pages
of one window are stable for the cache lifetime. `episode_card` is omitted on
public entries.

`GET /api/v3/discovery-controls?type=movie|series|anime` publishes shared
`QueryControl` values and capability availability for the selected provider.
Stremio catalogs expose their actual genres and custom options. Simkl genres come
from the current per-type CDN index; timeframe choices are today/week/month.
Genre files cover this month only, up to 500 ranked titles; the generic discover
route accepts `genre` and the `timeframe` option in encoded `extras`, with 24-card
pages. It rejects unknown genres/options and contradictory genre/timeframe
combinations. These rankings do not claim exhaustive country/network/year
coverage. The current official docs request that TV apps avoid thin V1 genre
endpoints pending provider V2; those broader filters remain pending.
Primary contracts: [v2 calendars](https://api.simkl.org/api-reference/calendar)
and [dynamic trending genre files](https://api.simkl.org/api-reference/trending).

Managed gateway HLS delivery reports `position: 0` and additive
`timeline_offset_seconds` from the gateway's actual applied source seek origin,
which can differ from requested position when packet copying selects a preceding
keyframe. `duration` remains the full source duration. Clients add the offset to
observed presentation time for progress/tracking and never seek it a second time.
Direct deliveries retain their requested seek position and report offset zero.
Renewal preserves the initial delivery snapshot. The shared decoder preserves
this field as optional `timelineOffsetSeconds` for legacy response compatibility.


`GET /api/v3/discover?type=all&search=...` combines at most three advertised
media types from the selected metadata integration into one canonical
`CatalogPage`; it never selects or mixes another browsing plugin. Broad text
search starts at offset zero and rejects ambiguous catalog/filter/paging input.
The total result projection is bounded to 200 cards and the complete operation
has a 12-second deadline. Specific-type discovery keeps existing pagination.

Source job reads accept optional `wait_ms` up to 10,000 milliseconds alongside
`after`. A read returns immediately for pending events or a terminal job; otherwise
it awaits the job notification and returns partial events as soon as a provider
finishes. The server revalidates session/profile/job authority once a second while
waiting and again before projection. Timeout yields the ordinary current event
snapshot, never fabricated source success.

`GET /api/v3/profiles` returns typed profile cards, CRUD/avatar availability,
account parent-PIN state and explicit unsupported per-profile-PIN availability.
Bootstrap includes the same object as `profile_management`. Restricted sessions
mark other targets `locked: true`, `lock_kind: "parent"`, and
`selection_requires_parent: true`; unlocking the existing account parent PIN
permits the selection without claiming a local per-profile PIN. POST/PATCH use
`name` plus an optional catalog avatar: `avatar_style` (one of the account
`AVATAR_STYLES` worlds) and a 1-based `avatar_choice` bounded by that world's
size, validated by the account profile rules. Cards project the account
record's `avatar_style`, `avatar_choice` (null without a catalog choice) and
`avatar_url`, so a choice made through either API shows on every client. The
legacy `avatar_id` (published preset IDs in `avatar_ids`) stays accepted and is
projected only while no catalog choice exists, so a catalog choice always takes
precedence; a catalog write clears it and one request cannot set both.
The profile integrations configuration (`GET`/`PUT /api/profiles/:id/integrations`)
projects the same account avatar as `avatar_style`, `avatar_choice` and
`avatar_id`, read from the account record on every load; configurations saved
before these fields existed keep loading.
DELETE protects the primary profile and preserves exact account ownership. The v3 parent aliases
retain existing PIN hashing, rate limits, short session-bound grants and rotation
invalidation. Kids bootstrap remains a scoped, policy-filtered read.


### Shared title presentation and related titles

`Card.category` distinguishes `movie`, `series`, and `anime` without changing playable canonical identity kinds. Simkl anime search objects use `type: tv/movie`, while full anime records use `type: anime` plus `anime_type`; both shapes retain the `simkl:anime:` namespace. All-category search ranks exact and alternate-title matches before broader matches, using provider rating votes to order equal relevance.

Optional `Card.metadata` carries year, runtime minutes, genres, rating/votes, certification, and alternate titles. `DetailSnapshot.recommendations` is an additive list (default empty) from the selected metadata provider's existing recommendation/similar fields. Stremio can fall back to a bounded genre catalog lookup; native hosts reuse their current scoped catalog cache. Anime episode playback uses only explicit provider TVDB aired coordinates, never an invented season.

Initial Home heroes exclude personal library rows and prioritize trending catalogs. Hero and Continue Watching share bounded, coalesced metadata fetches. Continue cards display the parent title, overview and logo while retaining their exact episode identity, progress and resume subtitle. Public artwork is optional and uses only an explicit verified IMDb mapping; it does not change metadata ownership. A slow artwork request cannot discard completed metadata.

One credential-free public trending document warms at server startup. Home allows a four-second assembly budget while startup warming is active (otherwise two), then up to four seconds for metadata and two for optional artwork. Existing profile/revision/kids checks still fence the final response. Bootstrap also includes the first typed library page and preferences snapshot to avoid separate initial hydration requests.


### Five featured titles and episode resume captions

Home includes additive `featured: Card[]` (default empty), with at most five distinct movie/series/anime titles from trending catalogs. Personal rows, live channels and episodes are excluded. `hero` mirrors the first featured card for older clients. Native enrichment batches at most seventeen deduplicated parents (five featured plus twelve continuation titles). Server title/artwork fetches run with concurrency three, within four-/two-second request budgets; startup warms five public records independently. Logos are verified as real PNG/WebP assets from explicit IMDb mappings, without downloading an entire alternate-provider series record just for artwork.

Simkl trending runtime strings and structured release dates normalize directly into typed metadata. Continued episodes format verified coordinates before remaining time (`S1 E1 · 22m left`); absent seasons use `E1`, and movies display only remaining time. Metadata enrichment preserves this caption and exact playback identity.


### Account dashboard and minimal client reads

The dashboard connection card uses only the v3 status, authorize and DELETE routes above. `linked` is the connection state; `connected`, `authorize_url`, `/sync` and `/lists` belong to the retired experimental interface and are not served by this host. Read-only import into the unified Library uses the separately documented scoped import receipt, not those legacy routes. Authorization is an explicit browser account operation, requires the selected profile and parent authority, and returns a URL rather than tokens. Device bearer sessions can read allowed status but cannot initiate authorization or unlink. The browser card cancels retired-profile reads/actions and validates the HTTPS Simkl authorization recipient before navigation. Callback return performs a fresh status read; it does not poll, import or expand scopes.

For browser startup, `GET /api/auth/status` supplies authentication/CSRF state, then `GET /api/v3/bootstrap` supplies identity, `profile_management`, prepared Home, the first typed library page and preferences. Do not issue separate initial Home, profiles, preferences or library reads when those snapshots suffice. A dashboard that needs only account/profile identity can use `/api/auth/me` after status; its existing `profiles` array avoids another `/api/profiles` read. Bootstrap library pagination remains authoritative: the first page does not represent every favorite when `complete=false`. Complete toggle state comes from bootstrap `library_membership`; do not hydrate all card pages for icons. Nullable `library_import` controls one aggregate refresh after import completes; see [Library contract](LIBRARY_V3.md).

Screen reads stay aggregated: one discovery response per applied query; one library snapshot per Apply, followed only by its typed cursor or active `refresh_after_millis`; calendar supports a bounded 500-entry page; details starts with one 24-episode window and authoritative season offsets, then automatically reads bounded 96-episode windows for the selected season; Live initially fetches a channel page, then only the selected channel's guide. Source discovery starts one scoped job and uses `wait_ms=10000` event polling instead of fixed short-interval polling. Published opaque sources can be selected early; discovery cancellation preserves published grant authority. Best selection uses one terminal host-ranking request, whose bound matches the discovery source cap. Images use negotiated direct/proxy delivery without per-card metadata calls. All page, job and cache publication retains account/profile/configuration/kids fences.

Read-only import into the main Library is now implemented: `library_import` and `library_membership` are additive bootstrap receipts, and the generic Library holds imported and locally saved titles together. Existing local progress/removals win; original provider events and verified paused sessions are imported without any provider write. See [Library import contract](LIBRARY_V3.md) for refresh, provenance and conflict rules. Linking alone does not claim a completed merge; the bounded task reports its actual result.


### Focused anime identity checks

Paused imports now derive movie/TV/anime identity from the playback wrapper rather than the first watchlist entry with a matching number. Anime pauses can use a unique absolute episode number from the already scoped provider episode document; supplied ID/absolute/TVDB hints must all agree. Imported Next uses the same full-document conflict checks. This preserves AniDB season1 numbering for provider tracking and aired coordinates for source admission, without translating between them by arithmetic or list offset.

Focused synthetic HTTP fixtures cover absolute52 mapped to airedS2E1, conflicting hints, accepted ID aliases, duplicate regular absolute numbers, conflicting aired mappings, foreign-profile/local-history preservation and rejected tracking with zero provider writes. Existing lifecycle/scrobble tests remain required. No real-account refresh, tracking mutation or physical anime playback is part of this qualification. Current upstream conventions were checked through Context7 and the official [playback documentation](https://api.simkl.org/api-reference/simkl/get-playback-sessions) and [OpenAPI](https://api.simkl.org/openapi.json).
