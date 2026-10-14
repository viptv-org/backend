# Shared viewing-client migration

This experimental host contract serves KMP, TV-web and Roku. Existing v1/v2 APIs remain available. Generated canonical content DTOs come from core `a7495e753ba9bdace3eb83d7cf99d86b9ed7f200`; authentication and playback retain their established HTTP envelopes. Client adapters translate those envelopes without interpreting provider IDs, opaque row IDs or source URLs.

## Startup and navigation budget

Reuse the existing browser cookie session or paired-device bearer session. Cookie mutations require the CSRF token from `GET /api/auth/status`; never put credentials in media URLs. Pairing uses `/api/auth/device/code`, `/api/auth/device/token` and the existing refresh flow. `POST /api/auth/profile` selects a profile with its current lock authority. `GET /api/v3/profiles` and bootstrap `profile_management` advertise supported management/parent-lock operations; remote per-profile PIN is unavailable.

After authentication/profile selection, **one** `GET /api/v3/bootstrap` supplies identity, profile management, Home, first Library page, complete lightweight `library_membership`, preferences, `library_import` and the first twelve typed continuation entries. An unselected session has nullable profile content. Do not immediately refetch Home, every Library page, every row, or every card's metadata. Library membership is for toggle state; it is not a requirement to hydrate all saved cards. Profile/connection replacement retires cached results and outstanding operations.

Every `HomeRow` has optional `browse: HomeBrowseAction`, default null for old snapshots. Its exact shape is:

```json
{"target":"Discover","query":[{"id":"type","values":["series"]},{"id":"catalog","values":["simkl-trending"]}]}
```

`QueryValue` uses `{id,values:string[]}` as generated. Current row actions have one value per field: serialize `id=values[0]` into `/api/v3/discover`; a Stremio row also supplies its authoritative `addon_id` (native plans use `connection_id` internally). `Library` uses `/api/v3/profiles/:id/library` and supplies the actual default pairs `source=favorites`, `sort=date_added`, `type=all`, `show=all`. `Continue` uses `/api/v3/profiles/:id/continue`, with an empty query by default. Append only the endpoint's cursor parameter when paging. A missing action means that row has no advertised drill action. Display labels and row IDs are never executable queries.

Open Browse with `/api/v3/discovery-controls` and `/api/v3/discover`; choices and compatible defaults are host-owned. Text search is one `/api/v3/discover?type=all&search=...` request using the selected metadata owner. Details use `/api/v3/meta/:kind/:id`, initially 24 episodes; automatically load selected-season windows with `offset` and `limit=96` or `128` using the authoritative `seasons` index. [Episode windows](EPISODES_V3.md) define canonical cursors and limits. Related cards and complete featured presentation are included in existing aggregates; no per-card client metadata fanout is needed.

## Typed continuation

`GET /api/v3/profiles/:id/continue?offset=0&limit=40` returns:

```json
{"scope":{"connection_id":"remote","account_id":"1","profile_id":"1","revision":3},"revision":10,"entries":[],"next_cursor":null,"complete":true}
```

The scope values follow generated `Scope` types; consumers retain them unchanged. Offset is 0–10000, limit 1–100. Each entry is `{card,record,status,previous_identity}`. Status spelling is `Resume`, `Next`, `Pending`, `CaughtUp`, `Upcoming`, or `Unavailable`.

**Clock rule:** apply `record.position_millis` and `duration_millis` only for `Resume` when the record and rendered card refer to the same media identity. For `Next`, `record` is the actual completed prior history anchor, `previous_identity` names that anchor, and `card` is the verified next episode. Its progress is null and playback starts at zero. Never transfer the previous episode's position or duration to the next card. No target history record is manufactured. Other statuses do not assert resume progress.

Resolve the next episode from one selected-provider metadata document:

```http
POST /api/v3/profiles/1/continue/next
Content-Type: application/json

{"identity":{"kind":"series","id":"exact-current-id","ids":[],"episode":{"order":"aired","season":1,"episode":2},"edition":null},"parent_identity":{"kind":"series","id":"exact-parent-id","ids":[],"episode":null,"edition":null}}
```

Response: `{"status":"Next","item":Card}` or `{"status":"CaughtUp|Upcoming|Unavailable","item":null}` (the status is one enum value, not the literal pipe expression). IDs and coordinates must be copied from a host-issued episode card. The host validates exact current/parent identity, unique canonical ordering, known airing, profile restrictions and configuration/session authority before publishing. Unknown airing/mapping is `Unavailable`; known future/unaired is `Upcoming`. Calls have an eight-second metadata deadline and can prepare the existing queue cache, without writing history/progress or exporting tracking. Preparation and imported transitions share the existing queue.

Hide or restore a title with:

```http
PUT /api/v3/profiles/1/continue/visibility
Content-Type: application/json

{"identity":MediaIdentity,"parent_identity":null,"hidden":true}
```

Use the entry card identity and its parent when present. Response is `{scope,revision,identity,hidden}`; `identity` is the resolved title identity and can be reused for undo with `hidden:false`. The host requires existing scoped history or a verified prepared target. Unknown/forged targets fail; hiding changes only queue visibility. Existing history/status correction remains `POST /api/profiles/:id/progress/correct`, with `{id,type,name,poster,action:"watched"|"unwatched"|"position",position?,duration?,series_id?,season?,episode?}` copied from the exact card/parent. Correction clocks use seconds; typed progress clocks use milliseconds. It is an explicit user action, not a fabricated playback event.

## Source and playback lifecycle

Source discovery remains `POST /api/v2/streams`, with `{type:card.identity.kind,id:card.identity.id,name:card.title,poster:card.poster,card}` and, for episodes, `series_id` from the exact parent plus `season`/`episode` from its verified coordinates. Client-supplied presentation/mappings are hints; the host resolves authoritative metadata. Poll `GET /api/v2/streams/:id?after=...&wait_ms=10000`, publish partial candidates and permit early manual selection. On terminal completion, rank the complete bounded set with `POST /api/v3/profiles/:id/source-ranking` and `{"ids":["opaque-source-id"]}`. Its response `streams` retains the public candidate envelopes and structured optional `presentation`. Codec/container/size/provider display come from those fields, never filename guessing.

`DELETE /api/v2/streams/:id` cancels discovery only. Use it when leaving or selecting before completion; no extra DELETE is needed on normal completion. Already published source grants remain valid for ranking and playback until their normal scoped expiry/revocation. Server fixtures exercise both Stremio and native-SDK grants through actual v2 playback admission after cancellation, repeated cancellation and foreign-owner denial. Cancellation does not release an admitted playback viewer.

[Playback v2](PLAYBACK_V2.md) remains authoritative: POST an opaque `stream_id`, stable request ID and real platform/decoder capabilities, then poll the returned playback ID. Roku always receives gateway-safe delivery; it cannot opt into raw direct delivery by claiming capability. Respect advertised HTTPS gateway URLs, protocol, lease and errors. Do not forward API bearer tokens to media origins.

For HLS cut-at-resume delivery, `delivery.timeline_offset_seconds` is the original-media offset already applied by the gateway. Keep player start position zero and report absolute media time as offset plus presented player time. Seek that requires a new HLS cut uses new admission. Progressive original media has offset zero and uses its returned start position for player seek. Explicit `live:true` disables VOD resume/progress/seek assumptions. Gateway first-frame acknowledgement uses `{}` only after actual presentation; heartbeat may be bodyless, and DELETE releases the viewer. Decoder/native protocols retain their own advertised acknowledgements.

Persist actual VOD presentation using `PUT /api/v3/profiles/:id/progress` with `{record:PlaybackRecord,parent_identity?,ended?}`; record clocks are milliseconds. Tracking events use `/api/v3/profiles/:id/playback-events` and the existing attempt/sequence/typed identity/event envelope. Import reads do not emit playback events or export history. [Library/import documentation](LIBRARY_V3.md) defines conflict rules, one collection, filters, membership and bounded aggregate refresh hints.

## Subtitles, images and Live

`GET /api/v3/subtitles/:kind/:id` returns opaque subtitle IDs. `GET /api/v3/subtitle-content/:id` returns authenticated normalized VTT for browser-capable clients; native raw ASS/legacy-encoding support remains separate. Browsers can fetch scoped VTT and create a local blob. Existing gateway embedded-track selection is available. External-addon captions on Roku still need actual device qualification of scoped HTTP header delivery or a gateway-leased rendition; never solve this by putting API tokens into URLs or forwarding them to a different origin. No speculative subtitle gateway endpoint is advertised.

Negotiate image delivery with `x-viptv-image-delivery: direct` for native, `proxy` for browser. Resolve returned `/api/v3/images/...` references against the active API origin. Keep verified direct transparent logos unchanged. [Image delivery](IMAGES_V3.md) documents validation and bounds.

Live uses `GET /api/v3/profiles/:id/live/channels` with advertised connection/group/search/cursor/limit, `GET /api/v3/profiles/:id/live/:channel/guide` for the selected channel only, and `POST /api/v3/profiles/:id/live/:channel/sources` before the same opaque v2 admission. Typed channels expose group memberships, guide availability and explicit live identity. No startup per-channel guide flood or client URL construction is required. Legacy Roku recent/favorite IPTV collection routes are not generalized to all M3U integrations in this checkpoint; migrate the real channel/guide/source path first rather than assuming collection parity.

## Qualification boundary

Focused host fixtures cover scope replacement during metadata IO, canonical 1242-episode next lookup, honest airing statuses, restricted-profile reads/mutations, visibility undo, Next's real history anchor, explicit Home browse queries and both discovery-cancellation grant lifecycles. Full server runtime-feature suite: 405 passed, 10 intentionally ignored. Local trusted HTTPS runtime checks are recorded separately from builds. This is not production deployment, a Roku decoder claim, or evidence that an HTTP200 source contains the requested title.

Read-only trusted-HTTPS qualification on the owned fixture returned bootstrap in 348 ms, twelve continuation entries including eight prepared Next targets, five explicit row actions, and 24 cards from an executed server-authored Discover action. The forty-entry continuation page retained real anchor identities. A Naruto next lookup exactly matched the following authoritative metadata episode. Progress, favorite and tracking receipt counts were unchanged. The existing browser session was reused; no login, user-device approval or playback mutation was needed. These measured warm results are not a cold-latency guarantee.
