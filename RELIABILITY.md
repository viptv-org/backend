# REL-001 — actionable API failures, 2026-09-28

Provider connection capacity, server playback capacity, expired sources,
upstream denial/unavailability and unsupported delivery now carry stable
`error_code` values with safe recovery text. Account/profile authorization codes
retain their existing semantics. Both string error conversions use the same
status mapping. Legacy clients still receive the `error` text field.

The new response-level capacity test passes. All 53 API integration tests pass
(two existing real-media tests remain ignored). The family connection-limit
scenario now checks its explicit code and actionable text, not generic retry
copy. No production migration or deployment was performed. Frontend gitlinks
promote the separately tested dashboard and TV-web sources for the next build.
