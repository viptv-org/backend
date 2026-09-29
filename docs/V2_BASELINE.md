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
