# Rust backend extraction and coordinated delivery

## Current v2 implementation contract

The approved [BE-002 backend/gateway plan](https://github.com/viptv-org/design/blob/25322b50df6511d6f8fd0de67522017b9ae388e1/BACKEND_V2.md)
and [ADM-002 admin rebuild](https://github.com/viptv-org/design/blob/25322b50df6511d6f8fd0de67522017b9ae388e1/ADMIN_V2.md)
supersede the extraction-only constraints below for the coordinated v2 cutover.
Preserve account/profile/history/source identities, require explicit legacy
provider ownership, and move media execution into the independent gateway.
HTTP and HTTPS IPTV sources remain supported. Account/admin UI adoption and
client protocol updates are tracked separately; this branch is not deployed.

DESIGN_REF intentionally remains the baseline visual/asset pin while this
non-visual backend adoption is in progress. No visual parity or completed
cross-platform adoption is claimed. See docs/V2_OPERATIONS.md and the design
implementation ledger for current evidence and incomplete work.

## Historical extraction contract

Move server/, operational scripts, container acceptance and Compose deployment files verbatim from vynxc/viptv@7d6b413. Pin the independently maintained React web repository at dashboard using a git submodule. Keep the HTTP wire contract, SQLite schema/migrations, account/profile IDs, auth origin, provider/addon settings and playback behavior unchanged.

## Historical delivery (superseded by AGENTS.md)
Pull-request and main CI run fmt, strict Clippy, backend tests including real FFmpeg, pinned-web bundle integrity, Docker build and isolated container acceptance. Successful version tags publish an immutable GHCR image and release metadata. Production rollout uses the existing guarded deployment procedure with all three Compose overlays; image publication is not a claim of production rollout. The named production volume must be explicitly retained when moving checkout directories.

## Acceptance
- Original server source and web source match their migration manifests.
- All required CI checks pass; container smoke checks execute against the candidate image.
- dashboard is an exact reviewed commit; CI verifies a checksummed distributable web bundle tied to that source commit; web CI validates its own source. Deploy keys are disabled by repository policy.
- Compose keeps viptv_viptv_data regardless of the new checkout folder name.
- Rollback uses the prior immutable image and current data; never replace current history with historical counts.

## Shared logic
The Rust backend remains the authoritative module for account/profile policy, catalog, source preparation, continuation, queue and history. Clients share versioned HTTP contracts and fixtures first. Add generated TypeScript/Kotlin clients after a reviewed schema exists. Rust FFI or a separate shared core is deferred until real duplicate policy justifies its packaging cost; BrightScript remains a thin presentation/transport client where feasible without changing it in this migration.
