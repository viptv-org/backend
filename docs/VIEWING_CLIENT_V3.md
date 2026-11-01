# Shared viewing-client migration

This experimental host contract serves KMP, TV-web and Roku. Existing v1/v2 APIs remain available. Generated canonical content DTOs come from core `a7495e753ba9bdace3eb83d7cf99d86b9ed7f200`; authentication and playback retain their established HTTP envelopes. Client adapters translate those envelopes without interpreting provider IDs, opaque row IDs or source URLs.

## Startup and navigation budget

Reuse the existing browser cookie session or paired-device bearer session. Cookie mutations require the CSRF token from `GET /api/auth/status`; never put credentials in media URLs. Pairing uses `/api/auth/device/code`, `/api/auth/device/token` and the existing refresh flow. A token poll that isn't granted answers 400 with `error` and `error_code` set to `authorization_pending` (keep polling), `access_denied` (the account holder declined) or `expired_token` (request a new code). `POST /api/auth/profile` selects a profile with its current lock authority. `GET /api/profiles` and bootstrap `profile_management` advertise supported management/parent-lock operations, including per-profile leave PINs ([parental controls](PARENTAL_CONTROLS_V3.md)).

After authentication/profile selection, **one** `GET /api/bootstrap` supplies identity, profile management, Home, first Library page, complete lightweight `library_membership`, preferences, `library_import` and the first twelve typed continuation entries. An unselected session has nullable profile content. Do not immediately refetch Home, every Library page, every row, or every card's metadata. Library membership is for toggle state; it is not a requirement to hydrate all saved cards. Profile/connection replacement retires cached results and outstanding operations.

### Change signals

`GET /api/profiles/:id/revisions` is the cheap freshness check for a visible screen (for example every 15 s while Home is in the foreground). It only reads a few SQLite rows; it never runs add-on, Simkl, metadata or Home work. It returns opaque strings:

```json
{"library":"12","configuration":"3","addons":"9f…","policy":"0","preferences":"2","profiles":"5.0"}
```

| Token | Changes when |
|---|---|
| `library` | progress, favorites/library, hidden queue, history clear, Simkl/Stremio imports for this profile |
| `configuration` | this profile's integration configuration |
| `addons` | the account's add-ons (same value as `GET /api/catalogs/revision`; the fixed kids value for a restricted profile) |
| `policy` | this profile's kids setting or title approvals |
| `preferences` | this profile's preferences |
| `profiles` | any profile of the account is created, deleted, renamed or has presentation/avatar changes, a kids `enabled`/`max_age`/rating/`allow_unrated` or blocked-title change, the account parent PIN is set/changed/removed, or this session's parent unlock changes |

Compare values only for equality; never parse them or compare them by order. A missing row reads as a stable default. Authorization matches the other selected-profile routes: 401 without a session, 403 when no profile is selected or `:id` is not the session's selected profile (including another account's profile), 400 for a non-numeric id. Restricted kids sessions may poll without a parent grant. Responses are `Cache-Control: no-store`.

Bootstrap includes the same object as top-level `revisions` (null when no profile is selected). The server reads it **before** building Home, so a write that commits during the build is observed as a change on the next check, not lost. A client stores the bootstrap tokens and refreshes only the areas whose token changed.

`HomeRow.title` is display-ready; clients render it as is. An add-on catalog row says its type ("Popular Movies", "Popular Series") unless the catalog name already does ("Top Movies"), and when two add-on rows would still share a title each gets ` · <add-on>` with the add-on's manifest name ("Popular Movies · Cinemeta"). The host decides this over all add-on rows before they load, so titles don't change with load order or failed rows. Continue Watching, My List and the Simkl rows keep their fixed titles.

Every `HomeRow` has optional `browse: HomeBrowseAction`, default null for old snapshots. Its exact shape is:

```json
{"target":"Discover","query":[{"id":"type","values":["series"]},{"id":"catalog","values":["simkl-trending"]}]}
```

`QueryValue` uses `{id,values:string[]}` as generated. Current row actions have one value per field: serialize `id=values[0]` into `/api/discover`; a Stremio row also supplies its authoritative `addon_id` (native plans use `connection_id` internally). `Library` uses `/api/profiles/:id/library` and supplies the actual default pairs `source=favorites`, `sort=date_added`, `type=all`, `show=all`. `Continue` uses `/api/profiles/:id/continue`, with an empty query by default. Append only the endpoint's cursor parameter when paging. A missing action means that row has no advertised drill action. Display labels and row IDs are never executable queries.

Open Browse with `/api/discovery-controls` and `/api/discover`; choices and compatible defaults are host-owned. [Typed catalog selections](DISCOVERY_V3.md) define backward-compatible array transport, selection limits and explicit connection/catalog identity. Text search is one `/api/discover?type=all&search=...` request using the selected metadata owner. Details use `/api/meta/:kind/:id`, initially 24 episodes; automatically load selected-season windows with `offset` and `limit=96` or `128` using the authoritative `seasons` index. [Episode windows](EPISODES_V3.md) define canonical cursors and limits. Related cards and complete featured presentation are included in existing aggregates; no per-card client metadata fanout is needed.

## Typed continuation

`GET /api/profiles/:id/continue?offset=0&limit=40` returns:

```json
{"scope":{"connection_id":"remote","account_id":"1","profile_id":"1","revision":3},"revision":10,"entries":[],"next_cursor":null,"complete":true}
```

The scope values follow generated `Scope` types; consumers retain them unchanged. Offset is 0–10000, limit 1–100. Each entry is `{card,record,status,previous_identity,next_episode}`. Status spelling is `Resume`, `Next`, `Pending`, `CaughtUp`, `Upcoming`, or `Unavailable`. An entry the host can attribute carries its metadata origin: `card.metadata_origin` names the exact scope that serves the title (a Stremio import's rows are attributed to the add-on that publishes them, a Simkl-linked title to Simkl) and `card.provider_id` names that producer, with `parent_identity` set to the series. Clients can then start finding sources on the tap without loading Details first. An entry whose origin the host cannot know keeps its previous shape (no `metadata_origin`, `library` provenance), and the client falls back as before.

A caught-up entry (`CaughtUp`, `Upcoming`, or `Pending`: the finished episode is its `card`) may also carry `next_episode: {identity, released_at_millis}`: the episode the viewer is waiting for and, when the release document gave one, its air date in milliseconds since the epoch. A client labels the row from it instead of only showing the finished episode ("S3 E5" or, with the date, "S3 E5 · Nov 4"). The field is absent when the show is over, when nothing further is listed, and when the next episode is already out (it is the entry's `card` then, status `Next`), so its absence is not an error and older entries decode without it. It is metadata about the title, like `card.subtitle`: the client must not treat it as a playable card or manufacture a history record from it.

**Clock rule:** apply `record.position_millis` and `duration_millis` only for `Resume` when the record and rendered card refer to the same media identity. For `Next`, `record` is the actual completed prior history anchor, `previous_identity` names that anchor, and `card` is the verified next episode. Its progress is null and playback starts at zero. Never transfer the previous episode's position or duration to the next card. No target history record is manufactured. Other statuses do not assert resume progress.

Resolve the next episode from one selected-provider metadata document:

```http
POST /api/profiles/1/continue/next
Content-Type: application/json

{"identity":{"kind":"series","id":"exact-current-id","ids":[],"episode":{"order":"aired","season":1,"episode":2},"edition":null},"parent_identity":{"kind":"series","id":"exact-parent-id","ids":[],"episode":null,"edition":null}}
```

Response: `{"status":"Next","item":Card}` or `{"status":"CaughtUp|Upcoming|Unavailable","item":null}` (the status is one enum value, not the literal pipe expression). IDs and coordinates must be copied from a host-issued episode card. The host validates exact current/parent identity, unique canonical ordering, known airing, profile restrictions and configuration/session authority before publishing. Unknown airing/mapping is `Unavailable`; known future/unaired is `Upcoming`. Calls have an eight-second metadata deadline and can prepare the existing queue cache, without writing history/progress or exporting tracking. Preparation and imported transitions share the existing queue.

**Up next:** a finished episode whose next episode of the same order and season family (specials stay apart) is already released becomes `Next` for the first released episode the profile has not finished, whether or not that release came after the finish: an imported finish is dated by the import, so the episode it is behind on still plays next. Its card keeps the series title and art; `subtitle` is `"S2 E1 · Episode title"`, and an in-progress episode is resumed instead. A target prepared by `continue/next` also projects when the stored release document cannot corroborate it (stale facts, a list that no longer holds the anchor): the prepared document is the profile's own metadata for that series, read under the profile's current account, configuration and add-on revision, and the row it was prepared for need not carry provider provenance (a Stremio import has none). A target read from a different explicit add-on than the one the episode was played from never projects; two add-ons may mean different titles by the same opaque episode id.

**New episodes:** when the episode a profile finished last was the newest one released, and a later episode is released after that finish, that entry also carries a compact badge first in `badges`: `"+N"`, N being the released episodes the profile has not finished (`"+1"`, `"+2"`). Clients speak it as "1 new episode" / "N new episodes". The entry is ordered by that first release time instead of the finished episode's activity, so it moves up once; later releases while that episode is unwatched only raise N. A backlog that was already released when the profile finished an episode, and an in-progress episode, never get the badge and never move up; My List alone never does. The host keeps, per profile and series, the episodes after the finished one with their air times (`continuation_releases`, never evicted), so badges, ordering and the next-episode verdict the Watched badge reads flip by themselves when a stored air time passes. `continue/next` stores the document it fetched; Home loads and Stremio imports start a background run for the profile's own session that refreshes every due series in rate-limited batches until none is due (resumable: each series has a persisted next check). A series is checked again five minutes after its next known air time, bypassing the add-on response cache, daily without a known date, weekly once its newest episode is six months old, and with back-off (1 h doubling to a week) while its metadata is unavailable. The episode list comes from the profile's browsing integration, or from the series' own provider when that cannot serve it, as for the Calendar: a `tt…` Stremio import Simkl has no mapping for is read from the metadata add-ons in a Simkl profile, a `simkl:` series from Simkl in a Stremio profile. A series Simkl does map stays on Simkl and backs off while Simkl fails, so its facts never alternate between providers. A refresh that changes a badge, order or verdict bumps the `library` revision. A Continue Watching card without `landscape` takes stored art: the still of its own episode from that document, else the series backdrop it keeps, else the backdrop of the title's My List card (Stremio imports fill it); restricted profiles skip the My List step. Nothing is fetched for it, so the art survives a server restart.

Hide or restore a title with:

```http
PUT /api/profiles/1/continue/visibility
Content-Type: application/json

{"identity":MediaIdentity,"parent_identity":null,"hidden":true}
```

Use the entry card identity and its parent when present. Response is `{scope,revision,identity,hidden}`; `identity` is the resolved title identity and can be reused for undo with `hidden:false`. The host requires existing scoped history or a verified prepared target. Unknown/forged targets fail; hiding changes only queue visibility. A hidden title stays hidden, replays included, until an episode is released after it was hidden; it then returns with its new-episode badge at that release, and the next Home load un-hides it for good. Existing history/status correction remains `POST /api/profiles/:id/progress/correct`, with `{id,type,name,poster,action:"watched"|"unwatched"|"position",position?,duration?,series_id?,season?,episode?}` copied from the exact card/parent. Correction clocks use seconds; typed progress clocks use milliseconds. It is an explicit user action, not a fabricated playback event.

## Source and playback lifecycle

Source discovery remains `POST /api/v2/streams`, with `{type:card.identity.kind,id:card.identity.id,name:card.title,poster:card.poster,card}` and, for episodes, `series_id` from the exact parent plus `season`/`episode` from its verified coordinates. Client-supplied presentation/mappings are hints; the host resolves authoritative metadata. Poll `GET /api/v2/streams/:id?after=...&wait_ms=10000`, publish partial candidates and permit early manual selection. On terminal completion, rank the complete bounded set with `POST /api/profiles/:id/source-ranking` and `{"ids":["opaque-source-id"]}`. Its response `streams` retains the public candidate envelopes and structured optional `presentation`. Codec/container/size/provider display come from those fields, never filename guessing. The body may add `device_max_height` (144..=4320, the height the device shows); the quality cap is then the lower of it and the profile's `max_quality_height`. Candidates carry SRC-BEST-MATCH-001 release facts, each omitted when unknown: `release_type` (`web_dl`, `webrip`, `bluray`, `remux`, `hdtv`, `dvd`, `other_rip`, `cam`), `three_d`, `dynamic_range` (`dv`, `hdr10_plus`, `hdr10`, `hdr`, `hlg`; empty is unknown, not SDR), `audio_flags` (`dual`, `multi`, `dubbed`; never audio languages), `seeders`, and `delivery` (`direct`/`torrent`, from the transport). Ranking order: camera/3D/torrents under 5 known seeders last, language tier, cap class, resolution (highest within the cap, closest first over it), release type, the profile's `hdr_preference` (`any`/`prefer`/`avoid`), delivery and seeders. The response also names the Best match marker and the Best matches group (`best_match_id`, `best_match_ids`); both are decided over the submitted set before the display filter, and `best_match_id` is null when no row passes the hard filters. Clients never derive group membership or the marker from row position. Preferences also carry `subtitle_mode` (`always`/`foreign`/`never`); `subtitles_enabled` always equals `subtitle_mode != "never"`, and a client that sends only `subtitles_enabled` keeps `foreign` while subtitles stay on.

`DELETE /api/v2/streams/:id` cancels discovery only. Use it when leaving or selecting before completion; no extra DELETE is needed on normal completion. Already published source grants remain valid for ranking and playback until their normal scoped expiry/revocation. Server fixtures exercise both Stremio and native-SDK grants through actual v2 playback admission after cancellation, repeated cancellation and foreign-owner denial. Cancellation does not release an admitted playback viewer.

[Playback v2](PLAYBACK_V2.md) remains authoritative: POST an opaque `stream_id`, stable request ID and real platform/decoder capabilities, then poll the returned playback ID. Roku always receives gateway-safe delivery; it cannot opt into raw direct delivery by claiming capability. Respect advertised HTTPS gateway URLs, protocol, lease and errors. Do not forward API bearer tokens to media origins.

For HLS cut-at-resume delivery, `delivery.timeline_offset_seconds` is the original-media offset already applied by the gateway. Keep player start position zero and report absolute media time as offset plus presented player time. Seek that requires a new HLS cut uses new admission. Progressive original media has offset zero and uses its returned start position for player seek. Explicit `live:true` disables VOD resume/progress/seek assumptions. Gateway first-frame acknowledgement uses `{}` only after actual presentation; heartbeat may be bodyless, and DELETE releases the viewer. Decoder/native protocols retain their own advertised acknowledgements.

Persist actual VOD presentation using `PUT /api/profiles/:id/progress` with `{record:PlaybackRecord,parent_identity?,ended?}`; record clocks are milliseconds. Tracking events use `/api/profiles/:id/playback-events` and the existing attempt/sequence/typed identity/event envelope. Import reads do not emit playback events or export history. [Library/import documentation](LIBRARY_V3.md) defines conflict rules, one collection, filters, membership and bounded aggregate refresh hints.

## Subtitles, images and Live

`GET /api/subtitles/:kind/:id` returns opaque subtitle IDs. `GET /api/subtitle-content/:id` returns authenticated normalized VTT for browser-capable clients; native raw ASS/legacy-encoding support remains separate. Browsers can fetch scoped VTT and create a local blob. Existing gateway embedded-track selection is available. Roku can attach an exact scoped selection through `POST /api/v2/playback/:id/subtitles` when `delivery.external_subtitles_supported` is true. It returns a viewer-leased SRT URL with timestamps already rebased, requiring no API headers; see [external subtitle delivery](SUBTITLES_V3.md). Old gateways advertise false. Actual Roku rendering remains separately qualified; API tokens never enter caption URLs or media-origin headers.

Negotiate image delivery with `x-viptv-image-delivery: direct` for native, `proxy` for browser. Resolve returned `/api/images/...` references against the active API origin. Keep verified direct transparent logos unchanged. [Image delivery](IMAGES_V3.md) documents validation and bounds.

Bootstrap advertises the M3U plugin only when the Live runtime and credential vault are available, matching the implemented profile-owned configuration path. It advertises LiveTv/Sources, never metadata ownership.

Live uses `GET /api/profiles/:id/live/channels` with advertised connection/group/search/cursor/limit, `GET /api/profiles/:id/live/:channel/guide` for the selected channel only, and `POST /api/profiles/:id/live/:channel/sources` before the same opaque v2 admission. Typed channels expose group memberships, guide availability and explicit live identity. No startup per-channel guide flood or client URL construction is required. Legacy Roku recent/favorite IPTV collection routes are not generalized to all M3U integrations in this checkpoint; migrate the real channel/guide/source path first rather than assuming collection parity.

## Qualification boundary

Focused host fixtures cover scope replacement during metadata IO, canonical 1242-episode next lookup, honest airing statuses, restricted-profile reads/mutations, visibility undo, Next's real history anchor, explicit Home browse queries and both discovery-cancellation grant lifecycles. Full server runtime-feature suite: 405 passed, 10 intentionally ignored. Local trusted HTTPS runtime checks are recorded separately from builds. This is not production deployment, a Roku decoder claim, or evidence that an HTTP200 source contains the requested title.

Read-only trusted-HTTPS qualification on the owned fixture returned bootstrap in 348 ms, twelve continuation entries including eight prepared Next targets, five explicit row actions, and 24 cards from an executed server-authored Discover action. The forty-entry continuation page retained real anchor identities. A Naruto next lookup exactly matched the following authoritative metadata episode. Progress, favorite and tracking receipt counts were unchanged. The existing browser session was reused; no login, user-device approval or playback mutation was needed. These measured warm results are not a cold-latency guarantee.
