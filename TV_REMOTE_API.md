# Remote viewing clients — additive API contract

The TV-web rebuild consumes the unversioned `/api/*` surface and bootstrap
`contract_version: 3`. These additions are shared with the concurrent Roku work;
existing fields/routes remain compatible. They are local source changes, not a
claim that production has adopted them.

## User intent and presentation

- `POST /api/profiles/:id/viewing-action` accepts `{card, restart?: boolean}`.
  It resolves the verified title/episode, continuation and profile preferences
  and returns `{card, label, resume, source_selection}`. A lookup selector needs identity, parent identity, provider, title and metadata
  origin; omit artwork and synopsis from GET selectors. Use the returned card for
  discovery and pass `resume` to playback admission. Do not choose the first
  episode or derive resume thresholds on a client. Exact episodes are checked
  against the paged server metadata, including episodes beyond the first page.
- `GET /api/profiles/:id/addon-activation` returns `addons` entries containing
  `connection_id` and the server-confirmed `enabled` flag for this profile.
- `PUT /api/profiles/:id/addon-activation` accepts
  `{connection_id: "addon:<id>", enabled: boolean}`. The server translates this
  intent into the canonical profile assignments using current supported
  capabilities and the existing revision check. It returns `configuration`.
- Cards in bootstrap, Home, discovery and detail include `view.saved`. Library
  page cards also report `view.saved: true`. This resolves canonical/private
  identity membership on the server. It is a presentation fact, not an authority
  token; mutations still validate the card and its metadata origin.
- `Card.view` also supplies ordered `metadata_parts`, `hero_metadata_parts`
  (billboard text without the media-kind prefix), `compact_metadata_parts`,
  typed `badges: [{label, role}]`, `hero_badges`, optional `tile_badge` and
  nullable `progress`. Capability aliases, genre length and badge ordering are
  producer decisions. Render the supplied sequence; do not parse raw `badges`.
- Viewing actions include the exact episode in `label`, plus nullable `progress`
  and `remaining_label` from the server continuation record. Unknown duration
  remains null. No cast summary is advertised without a verified source.
- Home rows include `view.card_shape: "landscape" | "poster"`.
- Preferences responses include `controls`, with `id`, `group` (`playback` or
  `subtitles`), `label`, optional `description`, and `choices: [{label, value}]`.
  Render these options and send the returned preference object with the edited
  value and its `revision`. Server validation and conflict responses remain
  authoritative. No local settings store is necessary.

## Sources and playback

`POST /api/profiles/:id/source-ranking` retains `ids` and optional
`device_max_height`, and adds optional `quality_height`. Its response contains:

- `streams`: server-ranked rows, filtered by the requested height when supplied;
- `best_match_id`: the best row across the submitted set before the display filter;
- `quality_filters`: `{height, label, count}` entries for that set;
- `total`: the submitted eligible ranked set's size before the display filter;
- `selection_note`: the server preference’s explanatory text for the source panel.
- Each stream adds `view.title`, `body`, `quality_label`, `format_label`,
  `addon_label` and `provider_label`. Missing facts remain null/empty. These
  display fields cannot be used as playback selectors.

Movie/episode discovery still starts `/api/streams` and polls its returned job.
Live `/api/profiles/:id/live/:channel/sources` can return `streams` immediately;
do not invent a job ID or a polling URL for that response.

Standard direct/gateway playback envelopes now project `actions`: `pause`,
`seek`, `restart`, `change_source`, `episodes`, `next_episode`, `tracks`,
`subtitle_style`, and `report_progress`. These are server display/execution
directives, not authorization tokens. Live disables VOD pause/seek/restart,
episode navigation and profile VOD progress writes. Track selection requires
selectable, supported input indices. Unknown/absent action flags are omitted
from client controls. Native torrent envelopes remain unchanged because their
existing consumer protocol is deliberately closed.

Live channel source responses additionally return `action` with the canonical
channel Card, `label`, `resume`, and saved `source_selection`. The intent Card
contains identity/title without artwork URLs; the existing channel directory
continues to supply artwork. Live resume is resolved by the producer. Consumers
send the returned opaque source IDs to the existing source-ranking endpoint and
honor its named best match only after an explicit Watch intent and when the
returned selection preference permits it. Explicit source-picker intent still
requires a user choice. Do not hard-code Ask or select the first Live source.

An explicit source or track replacement can add this to `POST /api/playback`:

```json
{"replace_source":{"playback_id":"previous-lease-id","observed_position":63.25}}
```

Send the decoder observation, leaving the top-level `position` at zero. The
producer validates the previous lease's account/session/profile and both source
proofs, requires the same verified target, and resolves the delivery clock. It
accepts a ready lease or one released within the 60-second context window. VOD
keeps the observation without relying on a delayed history write or resume
threshold; Live starts at the live edge. The resulting HLS clock may be represented
as `position: 0` plus `timeline_offset_seconds: 63.25`. Clients execute that
returned representation. Retries remain bound to the original request ID/body;
new explicit choices use a new request ID. Invalid/revoked/mismatched contexts
fail visibly. Native torrent replacement is outside this extension.

Playback uses `/api/playback`, its returned ID, heartbeat and DELETE routes.
A cancelled/ambiguous admission can be released by
`DELETE /api/playback-requests/:request_id`. URLs in `delivery` are transient
media capabilities and are never backend control URLs. TV-web uses conservative
H.264/AAC gateway delivery and the platform decoder; it runs no local torrent,
inspection or remux engine. Track/seek changes use a replacement server lease.
Report observed progress through the existing profile progress/playback-event
APIs; completion and continuation remain server decisions.

New admissions and the v3 Settings response use one effective audio/subtitle
preference resolver. Explicit track/language/off intents win. Persisted legacy
audio/language/mode values are inherited until a v3 row is saved; when neither
row exists, both surfaces use the shared v3 defaults (Original audio, Always).
Inherited values have an opaque preference revision token that changes when the
legacy row changes. Legacy size/style enums are not newly inferred into decoder
appearance; the existing v3 appearance defaults apply until saved explicitly.
Idempotent retries keep the original admission's snapshot. Ready
delivery adds `subtitle_appearance: {font_scale, text_color, background_color}`
for decoder execution. These changes do not alter the gateway wire protocol.

The current gateway cannot decide **Foreign audio only** from its selected audio
track. That choice is omitted from advertised controls. An existing v3 `foreign`
value (including an inherited legacy value) produces a visible conflict on admission unless an explicit subtitle
intent is supplied; it never silently becomes Always or client-inferred policy.
Automatic next/countdown, device display settings and unsupported player actions
remain unadvertised. Next is explicit through the existing continuation route.
Tv3 wording remains the producer wording; KMP-specific wording is not copied
into the shared API merely for a platform's visual comparison.

Library snapshots now include `default_query`, serialized from the shared
`LibraryQuery::default()` and matching the normalized unfiltered request. Use
it for Clear/reset previews, including optional null fields; do not copy a
platform's hard-coded sort/filter defaults. This addresses the additional
Library reset request in the Roku handoff.

The shared engine import is now checked by `scripts/sync-engine.py check` and,
when the canonical checkout is available, `scripts/sync-engine.py check ../core`.
Its source revision is recorded in `server/shared/engine/SOURCE.json`. Declared
Stremio certification values survive the typed title/episode projection, which
restores rating badges and the legacy observation path. Existing origin-scoped
parental checks continue to evaluate the provider document through their own
policy implementation; presentation fields are not grants.

Roku handoff reviewed: `roku/docs/BACKEND_HANDOFF.md` (2026-10-23). Producer tests
cover membership scope, surprising ranking, filters independent of best match,
exact episode selection and v3 preference delivery. The configured QA binary
has not been replaced: its reported viewing-action 404 is expected until the
producer is built and deployed separately.

Guide responses add `view.current_program_id`, `view.now_millis`,
`view.window_start_millis` and `view.window_end_millis`. Clients draw the timeline
and move focus; they do not guess which programme is current or autoplay one.

## Navigation availability

Bootstrap now adds ordered `navigation: [{id,label}]` entries. These are shared
viewing destinations derived from cached profile assignments, cached catalog
types, search authorization and Live parental permission. No provider probes
are performed to compute them. Disabled/deleted assignments cannot keep a
destination visible. Restricted profiles keep the approved-Library discovery
routes; Live requires the same parent permission as its existing API. With no
selected profile, only the Profile destination is returned.

Consumers preserve returned ordering and labels for routes they implement,
omitting unknown/out-of-scope routes. For example, TV-web deliberately omits
Calendar even when the producer advertises it for another client. A temporary
provider failure leaves its configured route available for a visible retry;
it does not silently remove navigation. Configuration/profile revisions refresh
the projection. The shared fixture includes configured, unconfigured,
restricted and unselected-profile examples.

## Optional hosting

`VIPTV_TV_WEB_DIST=/absolute/path/to/tv-web/dist` mounts the shared viewing bundle
at `/tv`, including assets and SPA fallback. TV launchers keep their platform
query; browser TV mode is `/tv/?layout=tv`. The existing `VIPTV_TV_DIST` KMP root
and `/device` account dashboard remain unchanged. The optional mount uses the
same HTTPS/API origin and does not alter CORS or authorization.

The public Rust `router_with_tv` remains compatible; `router_with_viewers` adds
an explicit fourth optional TV-web path for hosts/tests. No environment or
production routing is changed automatically by adopting this source patch.

Shared synthetic fixtures: [`fixtures/remote-viewing-v3.json`](fixtures/remote-viewing-v3.json).
The ordered card projection is asserted against this file by producer tests;
Roku can consume it alongside its transport/lifecycle fixtures.

## Source validation — 2026-10-23

The additive action/replacement/default-query producer and canonical engine
`e42a97fd9734b850b92579e2eb10d800300e39f1` passed the complete single-threaded
server library suite: **564 passed, 0 failed, 10 ignored** after the effective
preference resolver/revision cases were added. The ignored cases
retain their external-runtime requirements. The five parental-policy regression
cases pass, including all three formerly failing certification observations.
The native torrent case now feeds an actual producer envelope to the existing
`NativeTorrentBridge` parser, guarding its closed response shape. Engine checks
pass both offline and against the canonical commit (51 managed source files).
These are source/test results, not deployment or physical-device evidence.

The opt-in ignored `engine_v3::remote_tv_acceptance::remote_tv_backend` harness
also serves the actual router and TV-web static mount from an in-memory fixture
account. It requires an explicit loopback bind, owned add-on and static output.
The associated TV-web HTTPS test exercises real device code/approval/token,
bootstrap/profile selection, durable reload, scoped catalog/detail provenance,
library save/read and preference persistence. No `/api` responses are intercepted
in that browser test. Its external add-on content is synthetic, and this harness
does not qualify gateway transcoding or torrent delivery. It never opens or
replaces the shared QA database, runtime binary or serving paths.
