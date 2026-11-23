# Playback v2 control contract

Status: implemented with reviewed client migration candidates; not deployed.
The isolated cleanup candidate removes embedded execution; the retired routes'
`client_update_required` stubs were removed after the 2026-10-09 cutover. Its production binary does not link the
old engine or relay media. Runtime cutover/offline retirement still require
separate review.

## Start and lifecycle

POST /api/playback takes a backend-issued stream_id and a stable request_id:

```json
{
  "request_id": "client-generated-request-id",
  "stream_id": "backend-issued-source-id",
  "client": {
    "platform": "roku",
    "can_play_direct": false,
    "max_width": 3840,
    "max_height": 2160,
    "video_codecs": ["h264", "hevc"],
    "audio_codecs": ["aac"]
  },
  "position": 0,
  "force_gateway": false,
  "audio_track": null,
  "subtitle_track": null
}
```

Platforms: android, android_tv, desktop, web, tizen, webos, roku, vizio. Capability
dimensions describe real decoder limits, not screen layout or a user quality cap.
Codec lists use canonical families, not MIME codec strings. Track indices are
absolute input indices. Live playback starts at zero.

Optional `conversion` is `auto` (default), `audio`, `video`, or `audio_video`.
Explicit conversion, track indices, `audio_language` or `subtitles_off` require
an authorized gateway even on a direct-capable native client. Optional
`preferred_audio_language` and `preferred_subtitle_language` travel to gateway
planning or, for native direct delivery, its preferences metadata. Language tags
are bounded to 35 ASCII alphanumeric/hyphen characters. `subtitles_off` conflicts
with a subtitle index/preference and is rejected rather than silently ignored.
The full choice participates in idempotency; changing it requires a new request
ID. This extension requires a matching updated gateway and client contract.

New admissions snapshot the authenticated selected profile's stored audio and
enabled subtitle languages, unless explicitly overridden. Subtitle-off and an
explicit subtitle track suppress default subtitle-language injection. Preference
reads revalidate the resource lease and execute off the async runtime thread.
The caller's original body determines idempotency: changing saved preferences
does not alter an admitted session or make its retry conflict. A new request ID
uses the new defaults. Historical quality values remain archived in private
storage/export/backup but are absent from active preferences. A quality-only
write refuses the retired control; no cap replaces actual decoder dimensions.
Router fixtures verify direct
metadata, gateway forwarding, overrides, subtitle-off and stable retries. This
does not establish actual device track qualification.

The initial 202 response includes the backend playback id, status, expires_at
and renewal interval. Poll GET /api/playback/:id until ready or terminal.
When ready, delivery.kind is direct or gateway. Direct delivery includes the
original URL/required headers; this deliberately discloses them to an authorized
device. Gateway delivery contains an absolute URL under the registered gateway
origin/path prefix, without an integration key or arbitrary upstream JSON.

POST /api/playback/:id/heartbeat renews the viewer. Direct/gateway clients may
send no body, `{}` or `{"buffering":{...}}`; native torrent heartbeat sends no
body or `{"buffering":{...}}`. Any other top-level field is refused and bodies
are capped at 2 KiB. `buffering` holds optional stats for the window since the
previous heartbeat, written to the "Playback lease state" log line:
`rebuffer_count` (integer, ≤ 10000), `rebuffer_ms` (integer, ≤ 3600000),
`buffered_ahead_seconds` (number, 0–86400), `throughput_kbps` (integer,
≤ 10000000), `source_bitrate_kbps` (integer, ≤ 1000000), `video_decoder` and
`audio_decoder` (≤ 64 chars of letters, digits, `.`, `_`, `-`, space) and
`dropped_frames` (integer, ≤ 10000000). Unknown, mistyped or out-of-range stats
are dropped without failing the heartbeat. Never send URLs or file names. DELETE releases
only that viewer. Status reads do not renew. Use a 20-second heartbeat and respect the
returned expiry. Resuming after backgrounding should renew/validate before
trusting a cached URL. Terminal responses include an actionable error and stable
error_code; no provider diagnostics, credentials or gateway error text are echoed.

POST /api/playback-failures records one client-declared playback failure and
answers 204. It needs a profile session (403 `profile_required` otherwise). The
body is a closed JSON object of at most 1 KiB: `cause` (`renderer`,
`undecodable`, `stall`, `start_timeout` or `open_failed`), `first_frame`
(boolean), and optional `operation` (≤ 64 letters, digits, `_`, `.`, `-`),
`status` (HTTP status 100–599), `position_ms` (integer, ≤ 31 days) and
`playback_id` (≤ 128 letters, digits, `_`, `.`, `-`, `:`). Anything else is 400
`invalid_playback_failure`. Each accepted report writes one WARN "Playback
failure reported" line with account and profile tags, the fields above and the
`session_tag` of `playback_id` when that playback belongs to the calling
session; the raw id is never logged. Each session may log 30 reports a minute;
further reports still answer 204 and are dropped.

Gateway answers keep their own classification instead of a gateway-connectivity
failure: `playback_starting` while a viewer is not ready yet (retryable, never
terminal), `caption_invalid` and `caption_capacity` for a subtitle the gateway
rejected or has no room for, `gateway_worker_unavailable` for a lost torrent
worker, `gateway_not_ready` while the gateway is not admitting, and
`playback_expired` when the media behind a ready viewer is gone. Codes the
backend does not know are classified by HTTP status, never echoed.

The same request ID/body within the same account/profile/session reuses its
playback ID. A changed body conflicts. Released/failed/expired requests do not
restart implicitly; an intentional retry needs a new request ID. Startup is
bounded to 45 seconds including gateway selection, and gateway create calls are
separated from lifecycle-control concurrency so starts do not monopolize renewal.

## Prepared delivery

POST /api/playback with `"prepare": true` resolves a stream without starting
it, so a client can line up the next episode while the current one still plays.
The body is a normal start body and the answer is `201` with status `prepared`,
`delivery: null`, `expires_at`, `renew_after_seconds` and a plan:

```json
{"id": "backend-playback-id", "status": "prepared", "delivery": null,
 "live": false, "plan": {"kind": "gateway", "position": 0.0, "live": false},
 "expires_at": 1762000000, "renew_after_seconds": 20}
```

`plan.kind` is `direct`, `gateway` or `native_torrent`; `plan.position` is the
position the start would use, so a resume is already applied. A preparation does
no upstream work: it opens no gateway session, takes no provider connection,
probes no direct URL and registers no viewer. The plan is therefore advisory and
`delivery` after promotion is the authoritative answer.

A prepared playback lives on the ordinary 60-second lease, renewed by the same
heartbeat, and is released by the same DELETE. Keep it alive with a 20-second
heartbeat while the current episode plays. A session may hold four preparations
at once; more answers `playback_capacity`.

POST /api/playback/:id/promote adopts it. The optional body
`{"replaces": "<playing playback id>"}` releases that playback first, including
its gateway viewer and its provider connection, and only then runs the start, so
the two deliveries never hold one upstream input at once. Without `replaces` the
promotion behaves like an ordinary start and can fail with
`provider_connection_limit` while the outgoing stream is still playing. The
answer is the usual `202` starting; poll the same id until ready. Promotion
reuses the source resolved at preparation, so no discovery, ranking or
stream_id rotation happens at handoff. Promoting a released, expired or failed
preparation answers its terminal response and touches nothing else; promoting a
started one is idempotent and returns its current state.

## Download admission

POST /api/playback with `"purpose": "download"` gives a device a delivery it can
save to storage. `purpose` defaults to `"playback"`; any other value is
`invalid_playback_request`. The body is otherwise a normal start body; `prepare`,
`resume`, `replace_source`, a non-zero `position` and the decoder-start route are
`invalid_playback_request`. Profile preferences are not applied.

The answer is `200` and final; there is no id, lease, heartbeat or DELETE:

```json
{"purpose": "download", "status": "ready",
 "delivery": {"kind": "direct", "url": "https://...", "headers": {},
  "format": "original", "size": 7340032000, "expires_at": null}}
```

`size` is the full byte length when the source reports it, else null.
`expires_at` is the URL's expiry in Unix seconds when known, else null; on
expiry, discover the source again and resume with a Range request.
`required_audio_index` is present when the source has one. A torrent source on a
session that negotiated the version 2 runtime answers
`{"kind": "native_torrent", "format": "original", "size", "expires_at": null,
"grant"}`, where `grant` is the playback `native_torrent` grant: start the
runtime with it before `grant.expires_at`; it is never renewed.

Only direct delivery is admitted. A live channel, a source or device that needs
the gateway (Roku, Vizio, `can_play_direct: false`, `force_gateway`, a
conversion or track choice, a torrent without the negotiated runtime) or a URL
the direct probe can't reach answers `406 download_unsupported`. Source grants,
profile and session fences and provider connection limits are checked as for
playback. Each profile may make 60 download requests per hour, refused ones
included; more answer `429 download_rate_limited`. Download admission writes no Continue Watching,
viewing, progress or scrobble state and logs one `Download admission accepted`
line with session and profile tags and the delivery kind.

## Authorization and routing

Sources must have been issued by backend discovery for the exact caller scope.
Provider ownership, addon ownership, enabled scopes and configuration fingerprints
are rechecked at use and during the lease. Unassigned legacy providers are not
playable via v2. Arbitrary source URLs are not accepted in playback requests.
Retired WARP/embedded-egress headers fail explicitly instead of being forwarded
or silently bypassed; migrate the source's routing configuration first.

Roku and Vizio always require an authorized gateway, even if a client falsely
claims direct capability. Other clients reporting direct capability use direct
delivery unless force_gateway is set. Browser and webOS HTML direct delivery additionally need
HTTPS and no custom upstream headers. HTTP IPTV remains supported for native
delivery and as gateway input. No gateway means an early gateway_required error
for a device/source that requires it—not an attempt at embedded transcoding.

For managed playback, a currently authorized active session for the same account
and source establishes gateway affinity. That precedes priority/capacity ranking.
Otherwise healthy permitted gateways are checked in priority order with bounded
parallelism and an eight-second selection deadline. Capacity hints are advisory;
the gateway remains authoritative at admission. No unauthorized/family fallback
or cross-gateway session migration is performed.

Account/profile/session, source changes and gateway grants/revisions are checked
again after startup and on renewal. A background reaper expires inactive leases
and releases revoked managed viewers. Cancellation during startup suppresses late
publication and releases the gateway viewer once its ID is known. If a network
failure prevents release, the independent gateway lease remains the final bound.
Direct URLs already disclosed to a device cannot be remotely erased; clients
must stop native playback when their control authorization is lost.

## Skip segments

GET /api/playback/:id/segments answers the playing episode's opening, recap
and credits intervals in milliseconds on the original media timeline:

```json
{
  "segments": [
    {"kind": "recap", "start_ms": 0, "end_ms": 30000},
    {"kind": "intro", "start_ms": 310571, "end_ms": 400571},
    {"kind": "credits", "start_ms": 1290000, "end_ms": 1380000}
  ]
}
```

`kind` is `intro`, `recap` or `credits`; at most one of each is returned. The
array is empty when no source knows the episode, when the item is not a series
episode, or when the lookup fails. An empty answer is normal and never blocks
playback. The route reads the playback's own item and follows the existing
playback authorization, lease and error contract; live playback answers with no
segments.

POST /api/segments answers the same shape for a title played without a playback
session, such as a downloaded file. The body is `{"card": <Card>}`, the card the
client plays, capped at 64 KiB with no other fields; external ids must be short
tokens of letters, digits and `_ - . :`. It needs a profile session (401
without a session, 403 without a profile), allows 120 lookups per profile per
hour (`429 segments_rate_limited`), and answers `400 invalid_segments_request`
for a malformed body. Lookups, caches and the empty-answer rules are the
playback route's.

The server always returns the segments it knows: the profile's "Skip intros and
recaps" preference is applied by the client, which also uses `credits.start_ms`
to time the next-episode panel. Segments come from four upstream sources,
queried most-specific first and merged so the first confident answer per kind
wins: AniSkip (MAL release number), Anime Skip (AniList id and release number),
TheIntroDB (TVDB/TMDB/IMDb + season and episode) and SkipDB (IMDb + season and
episode). Cached answers live 24 hours on a hit, one hour on no data and 60
seconds after a failure; each source spaces its upstream requests to a
deployment-bound rate cap. Each source is off unless the operator sets
`VIPTV_ANISKIP=1`, `VIPTV_ANIMESKIP=1`, `VIPTV_THEINTRODB=1` or
`VIPTV_SKIPDB=1`; the `_BASE` spelling of each (`VIPTV_ANISKIP_BASE`,
`VIPTV_ANIMESKIP_BASE`, `VIPTV_THEINTRODB_BASE`, `VIPTV_SKIPDB_BASE`)
overrides that source's upstream origin for testing. Anime Skip is a GraphQL
endpoint that only answers with an `X-Client-ID`; `VIPTV_ANIMESKIP_CLIENT_ID`
sets it and defaults to the shared public client id the project publishes, which
is limited to 60 requests per minute, so a deployment should register its own.
Its timestamps mark only where a section starts, so an interval ends at the next
timestamp, spans longer than ten minutes are dropped rather than guessed, and its
mixed and preview markers contribute nothing. A
release that carries only a MAL, AniDB, AniList or Kitsu id is resolved to its
TVDB, TMDB and IMDb ids through a pinned, daily-refreshed Fribb anime-lists
mapping (`VIPTV_FRIBB_BASE` overrides the origin), so the TVDB- and IMDb-keyed
sources can answer it. Episode numbering comes from the resolved release layout,
so an episode whose mapping is missing or ambiguous answers with no segments
instead of a guess.

## Evidence and limitations

Native torrent admission is enabled by default for every authenticated supported
Android and Android TV client. There is no operator enable setting or account or
paired-family allowlist. Clients must still negotiate v1, advertise the closed
native capability, and provide an authorized exact VOD source with explicit file
selection. Account/profile/session, source ownership and current configuration
proofs are revalidated on admission, poll and renewal. At most two active native
grants are admitted per authenticated session: outgoing playback and one prepared
candidate. Releasing or expiring a grant frees that session's capacity. Explicit
server-side conversion and unsupported clients retain authorized gateway delivery.

Default admission does not establish hardware, public-peer or distribution
qualification. Unconditional two-second settlement under stalled OS IO remains
unproven; failed settlement retains work/cache reservations and sticky quarantine.
Owned-private media evidence does not qualify sustained public-peer egress or
human audio/remote acceptance. Preserve data, sign-in and origin during development
activation. A service restart loses in-memory grants but preserves request
idempotency and cancellation tombstones; client rollback must stop/join owned
native work without discarding failed-settlement accounting.

Router fixtures verify direct playback without an embedded worker; mandatory
gateway policy; no private-gateway fallback; capacity selection and affinity;
independent release/renewal; source mutation; safe remote failures; same-origin
media URL projection; and cancellation while the gateway is still starting.

The opt-in isolated_backend_gateway_real_media_lifecycle test runs only inside
a disposable Docker container with network none. It asserts no interface other
than loopback exists, then adds a public-classified fixture address solely inside
that namespace. No public/production network or production egress exception is
used. With the real gateway executable and FFmpeg, it verifies HLS/segment bytes
come from the gateway, heartbeat succeeds, and stop denies media. NET_ADMIN is needed only for that isolated fixture,
not production operation. Public HTTPS ingress and browser/device rendering are
not covered by this fixture.

Account-owned discovery, bounded live catalogs and explicit ownership/provider/
addon encryption tools are implemented and fixture-tested. The KMP app is the
only supported client; its unit/browser fixtures do not establish provider or
physical-decoder integration.

Remaining: coordinated merges/builds and downtime approval; production backup/
ownership/encryption/retirement/recovery; operator private-network exceptions;
bulk key rotation; public TLS/redirect checks; physical background/foreground,
4K/audio/subtitles and failure/restart/stress cases; full multi-output/input
accounting. Root's isolated media evidence applies to its recorded source
revision, not automatically this cleanup candidate. No production approval.
