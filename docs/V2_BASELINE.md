# BE-002 foundation checkpoint

Implementation branch: `refactor/backend-v2`. No deployed schema or public route
has been cut over. The new storage module is deliberately not called by startup.

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

Not complete: route integration, encrypted credentials, catalog/index lifecycle,
advanced configuration export, full legacy-history migration and gateway cutover.
