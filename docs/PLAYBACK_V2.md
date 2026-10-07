# Playback v2 control contract

Status: implemented with reviewed client migration candidates; not deployed.
The isolated cleanup candidate removes embedded execution and refuses retired
routes with `client_update_required`. Its production binary does not link the
old engine or relay media. Runtime cutover/offline retirement still require
separate review.

## Start and lifecycle

POST /api/v2/playback takes a backend-issued stream_id and a stable request_id:

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
and renewal interval. Poll GET /api/v2/playback/:id until ready or terminal.
When ready, delivery.kind is direct or gateway. Direct delivery includes the
original URL/required headers; this deliberately discloses them to an authorized
device. Gateway delivery contains an absolute URL under the registered gateway
origin/path prefix, without an integration key or arbitrary upstream JSON.

POST /api/v2/playback/:id/heartbeat renews the viewer. DELETE releases only that
viewer. Status reads do not renew. Use a 20-second heartbeat and respect the
returned expiry. Resuming after backgrounding should renew/validate before
trusting a cached URL. Terminal responses include an actionable error and stable
error_code; no provider diagnostics, credentials or gateway error text are echoed.

The same request ID/body within the same account/profile/session reuses its
playback ID. A changed body conflicts. Released/failed/expired requests do not
restart implicitly; an intentional retry needs a new request ID. Startup is
bounded to 45 seconds including gateway selection, and gateway create calls are
separated from lifecycle-control concurrency so starts do not monopolize renewal.

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

## Evidence and limitations

Normal native admission defaults off. An explicit development operator may set
`VIPTV_NATIVE_TORRENT_SCOPED_POLICY` to the closed JSON schema shown in
`.env.example`: `decision`, `account_id`, `device_session_id`, `platform` and
`max_active_grants`. The only admitted decision is
`scoped_experimental_sticky_quarantine_v1`, the platform is `android_tv`, and
the grant limit is one or two. Account and stable session-family values must
match the authenticated paired-device principal exactly; a display name is not
identity evidence. Malformed configuration fails startup. All negotiation,
exact-source/index proof and request-tombstone checks still apply.

This opt-in records the owner's narrow experimental development decision, not
general native qualification. Android still needs a measured normal artifact
receipt for its configured TV cohort and successful runtime/cache/clock facts.
Unconditional two-second settlement under stalled OS IO remains unproven;
failed settlement retains work/cache reservations and sticky quarantine.
Owned-private media evidence does not qualify sustained public-peer egress,
human audio/remote or distribution acceptance. Protocol support remains
separate from admission policy, and ordinary delivery behavior is unchanged.

Keep real scope identifiers in ignored private service configuration. Activate
only the authorized development environment after its actual device checks and
preserve the running data, sign-in and origin. Rollback stops/joins client-owned
native work, releases/tombstones grants, removes this operator configuration and
restores the previously sealed disabled normal APK with its signing identity
and retained app data. A service restart loses in-memory grants but preserves
request tombstones; do not treat advertisement removal as native retirement or
discard failed-settlement accounting.

Router fixtures verify direct playback without an embedded worker; mandatory
gateway policy; no private-gateway fallback; capacity selection and affinity;
independent release/renewal; source mutation; safe remote failures; same-origin
media URL projection; and cancellation while the gateway is still starting.

The opt-in isolated_backend_gateway_real_media_lifecycle test runs only inside
a disposable Docker container with network none. It asserts no interface other
than loopback exists, then adds a public-classified fixture address solely inside
that namespace. No public/production network or production egress exception is
used. With the real gateway executable and FFmpeg, it verifies HLS/segment bytes
come from the gateway, heartbeat succeeds, stop denies media, and the backend
embedded engine stayed idle in historical pre-cleanup runs. The cleanup candidate
removes it altogether and requires its own exact-revision rerun. NET_ADMIN is needed only for that isolated fixture,
not production operation. Public HTTPS ingress and browser/device rendering are
not covered by this fixture.

Account-owned discovery, bounded live catalogs and explicit ownership/provider/
addon encryption tools are implemented and fixture-tested. Client candidates
include TV-web `ecec486`, Roku `d46fe66`, Android `c3ebe7b` / quality transport
evidence `df2e334`; desktop evidence is tracked by the design ledger. Client
unit/browser fixtures do not establish provider or physical-decoder integration.

Remaining: coordinated merges/builds and downtime approval; production backup/
ownership/encryption/retirement/recovery; operator private-network exceptions;
bulk key rotation; public TLS/redirect checks; physical background/foreground,
4K/audio/subtitles and failure/restart/stress cases; full multi-output/input
accounting. Root's isolated media evidence applies to its recorded source
revision, not automatically this cleanup candidate. No production approval.
