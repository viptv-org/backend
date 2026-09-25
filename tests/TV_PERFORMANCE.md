# Addon latency corrections — 2026-09-25

The addon fetch path now coalesces simultaneous identical upstream URLs before
acquiring a network slot. A cancelled initializer does not strand another
waiter; completed errors do not become permanent cache entries. Cache pressure
removes the oldest-expiring entries instead of clearing every cached result.
Series metadata stops waiting for alternate providers once its episode art is
complete. Response shapes, deterministic metadata preference, account filtering,
upstream validation, timeouts and bounded concurrency remain unchanged.

Validation: `cargo test --locked --manifest-path server/Cargo.toml` passed 206
tests with two existing ignored tests. `cargo clippy --manifest-path
server/Cargo.toml --all-targets -- -D warnings` passed. Added tests assert 16
concurrent callers produce one upstream hit, cancellation permits recovery, and
complete primary metadata returns without waiting for a two-second secondary.

The user-reported temporary tunnel returns HTTP 530. Its original cold-cache
600ms versus 6–7s addon timings could not be reproduced; no external-addon
latency guarantee or production deployment claim is made. These changes were
kept separate from the existing playback/server working-tree edits.
