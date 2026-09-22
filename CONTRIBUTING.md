# Contributing to VIPTV backend

Thanks for your interest. VIPTV is a multi-repository product; this repository owns the Rust API, auth, catalog and media services, and the deployment packaging.

## Workflow

1. Product behavior starts in [viptv-org/design](https://github.com/viptv-org/design). Read the pinned `DESIGN_REF` commit and [SPEC.md](SPEC.md) before changing behavior; record proposed UX changes in design first.
2. Search this repository's GitHub Issues before opening a new one; use the needs-triage, needs-info, ready-for-agent, ready-for-human and wontfix labels.
3. The shared `viptv-provider` crate is vendored from [viptv-org/core](https://github.com/viptv-org/core) into `server/provider`. Edit it in core, then run `scripts/sync-provider.sh sync ../core`; CI rejects direct edits to the vendored copy.
4. Never commit credentials, tokens, provider URLs or private topology. Production migrations must preserve the named `viptv_viptv_data` volume.
5. Validate before pushing (from `server/`): `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked -- --include-ignored --test-threads=2` (set `VIPTV_TEST_FFMPEG`/`VIPTV_TEST_FFPROBE` for the real-media tests), plus the Docker build and `scripts/container-check.sh` for packaging changes.

## License

Contributions are licensed under the GNU General Public License v2.0 only (see [LICENSE](LICENSE)). By contributing you agree your work is licensed under it.
