# Simkl server integration (experimental v3)

Simkl credentials stay on the backend and are encrypted with the existing secret vault, bound to both account and profile. The server must have `SIMKL_CLIENT_ID`, `SIMKL_CLIENT_SECRET`, `VIPTV_SECRETS_KEYRING`, and its existing pinned HTTPS browser origin configured. Register `/api/v3/simkl/callback` on that origin as the Simkl OAuth redirect URI. Application secrets are never returned to clients. Configuration does not create a Simkl user link; the user must authorize each profile in a browser/dashboard session.

The server advertises only Discovery, Authorization, Search, Metadata, and Tracking for this adapter. Simkl is absent from bootstrap descriptors when the application or vault is unconfigured. A private metadata connection is available for each selected profile when the server is configured; tracking always requires user authorization. Some Simkl application configurations also require a linked user for search or metadata. A saved profile configuration must explicitly assign the corresponding Simkl capabilities; profiles without a saved override use their enabled connections. Profiles without an explicit saved browsing choice default to Simkl on a configured server and to Stremio otherwise. An explicit Stremio choice stays Stremio. PUT the existing profile integrations endpoint with `browsing_plugin` set to `simkl` or `stremio`; Simkl selection requires an enabled Discovery assignment.

## API

Every profile route requires a live VIPTV session with that exact selected profile. Parent restrictions apply. OAuth initiation and unlinking additionally require a browser account session, keeping credential management in the dashboard.

| Method and route | Input / result |
|---|---|
| GET `/api/v3/profiles/:profile/simkl` | `{configured, linked, connection}`; no credential material |
| POST `/api/v3/profiles/:profile/simkl/authorize` | `{authorization_url, expires_in_seconds}`; navigate in a browser |
| GET `/api/v3/simkl/callback` | Simkl `state`, `code` or `error`; redirects to `/?simkl=linked` or `/?simkl=cancelled` |
| DELETE `/api/v3/profiles/:profile/simkl` | Unlinks that profile locally and invalidates outstanding authorization states |
| GET `/api/v3/profiles/:profile/simkl/search?category=movie&query=…&page=1` | Engine `CatalogPage`; categories `movie`, `tv`, `anime`; 24 results/page, pages 1–20 |
| GET `/api/v3/profiles/:profile/simkl/meta/:category/:id?offset=0` | Engine `DetailSnapshot`; positive numeric Simkl title ID; TV episodes paged by 24 |
| POST `/api/v3/profiles/:profile/simkl/watched` | `{category:"movie",id:123}` or `{category:"tv",id:456,season:1,episode:2}`; `{recorded:true,…}` only after upstream confirmation |

The explicit watched action and authenticated playback/history completion events can upload exact watched items. TV watch updates first verify the requested season/episode in Simkl's own episode list. Unnumbered specials and conflicting identities are not inferred. Movie IDs and episode IDs remain distinct in the engine's projections. Metadata calls validate the returned title ID. Disabling a profile capability, unlinking, changing the selected profile, expiring a session, or changing policy invalidates work before a queued request is sent and again before its result is returned. An upstream mutation already dispatched cannot be recalled.

OAuth state is random, stored only as a hash, expires after ten minutes, is single-use, and is bound to the originating session, account, profile policy, and link revision. The link token is not placed in Home, public metadata, or any client response. Public metadata operations first use the application key without a user token. When Simkl rejects that request for authorization, a linked profile may retry with its own encrypted OAuth token. Authenticated responses are never stored in the shared public metadata cache. Disconnect removes local authorization and invalidates pending work. It does not call an upstream revocation endpoint.

## Current limits and evidence

A configured application and a real user's authorization are required. Isolated real-user OAuth2 linking and authenticated search were qualified on 2026-10-13; production deployment and application playback acceptance remain separate work. Public v2 calendar and scoped CDN genre/timeframe controls are implemented. Continuous inbound watchlist synchronization, anime episode mappings/tracking, country/network/year discovery outside the CDN rankings, and additional external mapping providers remain pending. Anime title metadata is supported; its episode list is intentionally empty until an ordering contract is implemented. Source and subtitle lookup resolve Simkl-qualified movie identities through their authoritative IMDb IDs. TV episodes additionally require an exact match in the provider episode list; supplied coordinates must agree. The original Simkl identity remains attached to playback authority and library state. Missing or conflicting mappings return HTTP 422 with `metadata_mapping_unavailable` before any addon lookup; anime episodes remain unsupported. Public title and TV episode documents share a bounded, coalesced one-hour metadata cache so detail, source, and subtitle requests reuse the same evidence.

The generic `/api/v3/bootstrap` and profile Home endpoints assemble Simkl Trending Movies, TV Shows, and Anime when Simkl is selected. One combined public `today_100.json` document serves the three rows, coalesced and cached for one hour without user credentials. Home retains its two-second assembly deadline and presents failed rows for slow or failed upstreams. `/api/v3/discover` routes movie/series/anime discovery and text search to the selected provider; unsupported controls are rejected. Generic `/api/v3/meta/:kind/:id` accepts projected identities such as `simkl:movie:123` and uses the selected provider. Explicit provider switches never merge Stremio and Simkl metadata in one browsing configuration.

The engine tests cover stable title/episode identities, missing and conflicting IDs, duplicate episode coordinates, type mismatches, and exact source mappings. In-process server fixtures cover routed OAuth, encrypted storage, account/profile isolation, expired/revoked/single-use state, disabled configuration, exact episode tracking, metadata mismatch, unlinking, default provider routing, public cache coalescing, OAuth-required metadata fallback without shared caching, explicit provider selection, bounded slow Home assembly, canonical playback identity, and mapped movie/TV addon source and subtitle requests. No production server or real account was modified.

API schemas were checked with Context7 `/simkl/api` against the official [authentication](https://github.com/simkl/api/blob/master/_autodocs/api-reference/authentication.md), [content discovery](https://github.com/simkl/api/blob/master/_autodocs/api-reference/content-discovery.md), and [sync](https://github.com/simkl/api/blob/master/_autodocs/api-reference/sync.md) documentation. Simkl's current documentation is at [api.simkl.org](https://api.simkl.org).

## Authenticated history and playback events

`PUT /api/v3/profiles/:profile/progress` accepts `{record: PlaybackRecord, parent_identity?: MediaIdentity, ended?: boolean}` using the existing shared engine types. Positions and durations are milliseconds. Server activity dates and the existing 95% completion policy are authoritative; client `updated_at_millis` and `completed_at_millis` are ignored. Known-runtime Ended records the actual duration as the final position. Unknown-runtime Ended retains the observed position and duration zero; it does not infer completion or queue external history tracking. Ordinary Stremio/opaque identities persist in selected-profile history without requiring Simkl. Episode parent identities remain opaque to clients; the server verifies Simkl parent IDs and exact aired coordinates before persistence. The existing seconds-based progress PUT remains supported.

Successful authenticated completion writes and explicit watched history corrections queue `/sync/history` only for verified Simkl movie/TV episode identities with enabled Tracking and a linked profile. The queue has at most 128 pending operations, four workers, per-profile serialization, and a 30-second deadline. Durable receipts coalesce an item within one link revision; failed/unconfirmed work can retry on a later authenticated completion update after 60 seconds. No automatic rewatch flag is sent. Queued jobs are session/profile/policy/link/configuration-bound; the dispatch and response revalidate those facts. Local history saving does not wait for provider traffic. Profile Simkl status includes safe `tracking: {pending, failed}` receipt counts. Pending tasks live in process memory; after a restart, later authenticated updates can retry stale receipts rather than restoring an old session's mutation authority.

`POST /api/v3/profiles/:profile/playback-events` is a provider-neutral boundary. Its body is `{attempt_id, sequence, identity: MediaIdentity, parent_identity?: MediaIdentity, event: "start"|"pause"|"stop", position_millis, duration_millis, ended?: boolean}`. An attempt starts with sequence 1 and a start event; subsequent sequences increase. The attempt's media identity is immutable, replacement retires the old attempt, and queued superseded events cannot dispatch. A busy queue does not consume an event sequence or retire an attempt. Once an event is admitted, its intent supersedes older events even if provider processing later fails; repeated admitted sequences return conflict and a subsequent event uses a fresh increasing sequence. Provider authorization rejection returns `unavailable/tracking_authorization_required`, rather than a VIPTV session-expiry status. No provider or credential configuration returns `{state:"ignored",provider_id:"simkl",reason}` without provider traffic. An enabled Tracking connection without an authoritative mapping returns `{state:"unavailable",provider_id:"simkl",reason:"metadata_mapping_unavailable"}`. An unknown runtime is represented as unavailable except for start at observed position zero.

Supported Simkl lifecycle requests use `/scrobble/start`, `/scrobble/pause`, and `/scrobble/stop`; TV bodies contain `show.ids.simkl` and `episode:{season,number}`. Simkl stop normally completes at 80%, while VIPTV completes at 95%. This adapter maps incomplete stop events to pause; stop is sent only at VIPTV completion or an explicit known-runtime Ended event. Confirmed responses are `{state:"recorded",provider_id:"simkl",action,completed}` after the provider returns the matching movie/episode identity and action. Start/pause are not completion records. Timestamped history completion uses Simkl's supported `watched_at` field. No unsupported endpoint or inferred whole-series/anime completion is used.

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
`name` and optional `avatar_id` from the published preset IDs; DELETE protects the
primary profile and preserves exact account ownership. The v3 parent aliases
retain existing PIN hashing, rate limits, short session-bound grants and rotation
invalidation. Kids bootstrap remains a scoped, policy-filtered read.
