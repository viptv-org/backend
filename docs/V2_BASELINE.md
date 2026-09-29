# BE-002 foundation checkpoint

Implementation branch: `refactor/backend-v2`. Production remains unchanged.
Startup now initializes additive ownership/default/index tables without assigning
any legacy sources. The v2 HTTP routes use the bounded queries; legacy routes and
their current web consumers remain until the coordinated client cutover.

## Reproduction

`python3 scripts/benchmark-vod-matches.py` creates only in-memory synthetic SQLite
fixtures. It does not connect to providers or read the production database. The
v2 query is loaded from the exact SQL used by the Rust implementation.

Measured on this development host, five iterations, 2026-09-29:

| Fixture rows | Legacy materialized / returned | Legacy median | V2 materialized / returned | V2 median |
| --- | --- | --- | --- | --- |
| 10,000 | 10,000 / 3,334 | 11.79 ms | 51 / 50 | 0.11 ms |
| 100,000 | 100,000 / 33,334 | 145.66 ms | 51 / 50 | 0.12 ms |

The legacy 100k response contains about 4.03 MB of JSON. V2's first-page item
array is 5,913 bytes (cursor/envelope excluded). The v2 default query uses the
unmatched index and does not require the legacy temporary ORDER BY B-tree.
These are warm synthetic SQL/materialization/serialization results, not measured
production HTTP latency or admin browser-render timings.

## Executable migration/storage groundwork

The v2 module provides explicit all-or-nothing provider ownership assignment,
persisted account live defaults, isolated per-request overrides, and account-
bound cursor pages/search. Tests cover ambiguous/incomplete/disabled ownership,
cross-account access, default stability/fallback and bounded unmatched results.
No implicit provider ownership migration or public source-sharing grant occurs.

## HTTP and migration checkpoint

- GET/PUT `/api/v2/iptv/matches` lists and edits only account-owned candidates.
  Pages default to 50 and reject sizes above 200, cross-account cursors and
  unknown query fields. Editing a foreign ID is indistinguishable from a missing
  ID. Operator role does not bypass ownership.
- GET/PUT `/api/v2/iptv/live-default` persists the account default and validates
  explicit changes. Paired-device sessions and locked kids profiles cannot use
  these account-management endpoints. No viewing-client swap button was added.
- `provider-owners` inspects read-only and requires an explicit complete owner
  map for assignment. It creates a private SQLite online backup and streamed,
  versioned advanced-configuration export before changing the source. Invalid
  owner maps roll back; IDs, progress and manual VOD mappings are preserved.
- Full backend suite: 219 passed, two existing real-media tests ignored. Strict
  Clippy across all targets passes. Migration library and executable fixtures
  verify WAL capture, non-overwrite, private permissions, confirmation and no
  credential output. No production database was read or changed for these tests.

Not complete: admin/viewing-client adoption, account-scoped source CRUD/discovery,
encrypted credentials, catalog/index lifecycle, retirement of legacy modules,
full cutover/rollback qualification and backend gateway integration. See
[V2_OPERATIONS.md](V2_OPERATIONS.md) before using the migration executable.

## Gateway configuration checkpoint

Account-owned gateway registration/check/update/grant APIs now validate scoped
keys over a bounded, address-pinned HTTPS control client. Gateway integration
keys are encrypted with an operator-supplied, versioned AES-GCM keyring; there
is no plaintext fallback or implicit public/family grant. See
[GATEWAYS_V2.md](GATEWAYS_V2.md) for setup and the exact remaining boundaries.

Current backend suite: 224 passed, three opt-in fixtures skipped by the default
run. Strict all-target Clippy passes. The new opt-in interoperability fixture
also passed separately against the independent gateway executable, issuing a
real scoped key and registering/checking it through the backend router. It does
not prove playback forwarding, public HTTPS deployment or provider-secret migration.

## Playback control checkpoint

The /api/v2/playback lifecycle now supports native direct delivery and independent
gateway-managed delivery, with scoped source validation, active affinity,
priority/capacity selection and no backend media-byte relay. A real isolated
container fixture verifies HLS playback/renewal/release through the independent
gateway with the backend engine idle. See PLAYBACK_V2.md for exact scope and gaps.

Current default backend suite: 230 passed, four opt-in fixtures skipped. The new
isolated real-media fixture passed separately; strict all-target Clippy passed.
No current viewing client has been cut over, and legacy discovery/playback,
encrypted provider migration and remaining acceptance work are still pending.
