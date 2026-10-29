# backend

Rust server (`server/`): API, auth, catalog, continuation, image proxy, and the
mounted web app. Read the workspace `AGENTS.md` first.

## Toolchain

- `RUSTUP_TOOLCHAIN=1.98` (dependencies need 1.98).
- Give each worktree its own `CARGO_TARGET_DIR`; worktrees sharing the default
  target overwrite each other's test binaries.
- Keep worktrees outside the workspace (`/tmp/viptv-wt/...`): the host probes
  new localhost ports there, which reaches test fake servers.
- The repo isn't `cargo fmt` clean; match the surrounding style by hand instead
  of reformatting files.

## Checks

- `cargo test --manifest-path server/Cargo.toml`
- `cargo test --manifest-path server/Cargo.toml --features live-runtime --no-run`:
  default runs never compile the feature-gated tests.
- `python3 scripts/sync-engine.py check`

The same three run in `.github/workflows/checks.yml` on pull requests into `edge`
and `main` and on pushes to `edge`.

## Tests

Fake servers in tests must ignore `HEAD` and other safe-method requests to `/`:
the host's port probe sends them, and counting them makes assertions flaky.

## Engine snapshot

`server/shared/engine/` is a copy of `core`'s engine. Never edit it by hand:
change `core` first, then `python3 scripts/sync-engine.py sync` against that core
commit so `check` passes.
