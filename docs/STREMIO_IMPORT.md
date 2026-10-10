# Stremio account import — guided development implementation

The importer is in `server/src/stremio_import.rs` and the account dashboard's `src/StremioImport.tsx`. The uncommitted dev-only IMP-001 proposal is in `../design/specs/behavior/stremio-import.md`; existing design pins remain unchanged. Keep credentials, tokens, raw exports, title names/IDs, viewing dates and personal statistics out of Git. Personal repair requires the authorization and preservation checks described below.

## Account dashboard flow

Select your own unrestricted destination profile, then open **Account → Import from Stremio**. The guided flow has four stages: connect Stremio, choose optional add-ons, review named items, and confirm the chosen import. Compatible add-ons and eligible library/history rows start selected. Uncheck individual add-ons, continue without add-ons, or search/filter and exclude specific titles or episode resumes. The review shows each item's planned changes and explains entries that cannot be safely imported.

Back and Next let you revisit choices before confirmation. Returning from confirmation retains the item selection; returning to add-ons retains their selection. Changed add-on choices regenerate the review from the same read-only source snapshot, retaining exclusions for stable item handles. Changing the source options requires a fresh preview. A review revision prevents a stale screen from applying a newer selection.

Viewing data and add-on configuration are not written until you check the destination confirmation and press **Confirm import** on the final recap. Selected new add-ons are saved to encrypted account-owned storage and become available to the account's profiles. Skipping an add-on never removes existing configuration. Cancel discards the preview. Passwords are cleared after submit, scope replacement or cancellation and are not persisted in browser storage or server import records. The Windows Android checkout's ignored `.env` contains privately saved credentials for later manual use; those values do not auto-fill this screen or enter the web/Android bundles.

Supported scope: saved IMDb/metadata-verified titles, movie history/resume, exact episode resume, and metadata-verified episode watched bitfields. Milliseconds become seconds and trustworthy activity timestamps are retained. Unknown individual episode watch dates are not invented. Unverified identities/bitfields, ambiguous episode state and likes/loves remain for review. This is not an event-by-event history export.

Guided review verifies metadata without requiring add-on installation: account-owned sources take precedence, followed by selected verified add-ons, then fixed public Cinemeta for IMDb movie/series IDs only. Flagged series are checked first. Verification is limited to eight concurrent identity lookups, a 30-second batch deadline and 16 MiB of retained metadata; successful results survive a timeout, while unresolved identities remain for review. The batch deadline leaves room for add-on verification and publication within the dashboard's 45-second request timeout.

Anime ids (`kitsu:`, `mal:`, `anilist:`, `anidb:`) are remapped to IMDb through Simkl when `SIMKL_CLIENT_ID` is configured (on-demand lookups paced under Simkl's 10 GET/s rule). Review resolves each title to its IMDb show, translates its watched and resume episodes through Simkl's per-episode TVDB numbering, and keeps only episodes Cinemeta lists. Entries of one show (seasons, split cours, the IMDb entry itself) merge into one item; untranslatable state counts as needing review. Unset, anime ids remain review-only unless a selected add-on verifies them. Retained metadata keeps only video ids and numbering, so long series are not rejected for size.

Previews are bounded, session/profile-bound, and expire after ten minutes or server restart. Paired devices and restricted profiles cannot import. Apply revalidates the destination, creates a private consistent backup, and merges in one transaction. Existing favorites are not overwritten; newer/equal progress and manual corrections are preserved. Persistent profile-scoped receipts prevent replays from resurrecting imported favorites removed locally. Queue hiding is not reset. A stale preview must be recreated. An interrupted apply may have committed; retry the same preview rather than assuming rollback.

## Tagged watched completion (Plan 2)

The owner-approved extension is recorded in `../design/specs/behavior/stremio-import.md`.
It retains the existing `progress` table, with explicit import provenance and a
completion-only marker. Synthetic ancient ordering values are implementation
sorting values, not watch dates. History puts real activity first and labels
undated completion-only records **Imported — date unknown**. Such records are
excluded before Continue Watching selects the most recent activity for each title.

`watched`, `resume_active`, `completion_only` and `watch_date_known` are authoritative
backend facts. Previously watched movies/episodes can retain a real active rewatch;
position, duration, actual activity timestamp and source context are not replaced
by an imported completion. The review and history show **Watched · Rewatch in
progress** when both facts are present. Shared core gives active resume precedence
in episode selection. Playback establishes activity without discarding earlier
completion; explicit local corrections take precedence over imported assertions.

For the owner-approved import-only continuation adjustment, a verified watched
partial episode with later verified watched episodes keeps its saved position,
runtime and activity timestamp. An internal import marker makes Continue Watching
use the highest verified watched episode in the same regular/special episode
sequence as its continuation anchor. Undated completion records remain undated;
queue ordering retains the original series activity date. Normal playback and
next-episode resolution are unchanged. Newer playback, manual corrections and
missing/excluded target records take precedence over the marker. Legacy receipts
allow one context-only anchor repair of an unchanged import-owned resume; replay
cannot repeatedly reset its continuation.

Receipt versioning allows missing completions from the older importer to be
reevaluated at the same source timestamp. Repair remains idempotent and preserves
removed imported favorites, manual decisions and hidden queue titles. Personal
repair requires explicit owner authorization, a uniquely verified destination,
a read-only preview, a private consistent backup, and post-apply comparisons.
It never targets production or impersonates another account.

Routes, under `/api`:
- `POST /profiles/{profile}/imports/stremio/preview` with `{email,password,import_library,import_progress}`.
- The guided preview additionally sends `inspect_addons:true`, then `POST /profiles/{profile}/imports/stremio/{preview}/review` with the chosen `selected_addons` handles and the current `expected_review_revision` (initially zero).
- `POST /profiles/{profile}/imports/stremio/{preview}/apply` with `{confirm:true}`.
- Guided confirmation also sends `excluded_items` and the finalized `review_revision`; completed retries must use exactly the same selection and revision.
- `DELETE /profiles/{profile}/imports/stremio/{preview}` to discard.

Run `cargo test --locked --manifest-path server/Cargo.toml --lib stremio_import`, and dashboard `npm test -- --run` / `npm run build`. Synthetic tests and browser rendering do not establish real credential validity or prove a personal import. Test a separate synthetic profile first; never impersonate a personal account or apply a personal merge without the owner's explicit destination/preview authorization.

## Existing read-only command-line preview

From the workspace root, run `python3 backend/scripts/stremio-preview.py` (Python standard library only). It reads `STREMIO_EMAIL` and `STREMIO_PASSWORD` from the ignored workspace-root `.env`, logs in to Stremio, retrieves library records, validates IMDb-series watched bitfields against Cinemeta episode metadata, and prints **aggregate counts only**. It does not persist source records, query the likes service, or write to either account. Run `python3 backend/tests/test_stremio_preview.py` for offline mapping tests.

For a target-aware read-only preview, supply `--db /path/to/local-viptv.sqlite --profile-id N`. This requires an existing local SQLite database and numeric, account-owned VIPTV profile ID; it compares existing favorites and progress without writing to the database. Do not pass a production database or its private path in a public issue/log. The script does **not** provide an apply mode or generate an import file. Without a target, candidate counts are not actual additions; check the target and approve a merge policy before any import. The current preview treats non-IMDb episode flags as requiring metadata review and unsupported title IDs as requiring identity review.


## Where the data belongs in VIPTV

VIPTV accounts own profiles; personal viewing data is profile-scoped. The backend (`server/src/app_state.rs`, `auth/schema.rs`, `handlers_profiles.rs`, `library.rs`, `continuation.rs`) has SQLite `favorites(profile_id,type,id,name,poster)` and `progress(profile_id,type,id,name,poster,position,duration,updated_at,context,title_id)` records. `progress` stores **one current row per item**, not one event per play. The profile APIs expose favorites, paged viewing history, watched/unwatched corrections, episode history, and Continue Watching. `dashboard` (the `web` repository) already has My List and Viewing history screens. There is no distinct likes/loves model or Stremio account connection.

A migration should target one explicitly chosen, authorized VIPTV profile. A separate backend import path may be appropriate for preserving timestamps and transactional merge: ordinary progress/correction writes use VIPTV activity time, not the imported `lastWatched` value. Compare proposed changes against the target profile before any write, and preserve newer existing VIPTV state by default. Respect profile ownership and restricted-profile policies. Do not write directly to production SQLite from an external tool.

## Stremio read-only source

Stremio's [client API request types](https://github.com/Stremio/stremio-core/blob/development/src/types/api/request.rs) use HTTPS login and authenticated `datastoreGet` for the `libraryItem` collection. These are observed client endpoints, not a promised public integration API. A read-only local probe using account-owner-provided credentials confirmed that `datastoreGet` returns full library items with ID, name, type, poster, `removed`, `temp`, timestamps, and state fields. Stremio's [library item definition](https://github.com/Stremio/stremio-core/blob/development/src/types/library/library_item.rs) documents `lastWatched`, `timeOffset`, `duration`, `video_id`, `timesWatched`, and the `watched` bitfield. Offsets and durations are **milliseconds**; VIPTV's positions and durations are seconds.

**Do not use response-array index as recency.** Records were not in `lastWatched` order in the read-only probe. Sort by parsed `state.lastWatched` to order title-level last activity; this value is not a separate watch timestamp for each episode. A library item with `removed` or `temp` set should not automatically become a VIPTV favorite. Likewise, playback activity alone does not imply a saved title or completed watch.
A cleared, removed Stremio record may still appear in `datastoreGet` with an old `lastWatched` value. Import eligibility must not be inferred from that timestamp alone. The preview skips removed items with **no resume offset, no nonzero watched counter, and no watched-episode bitfield**, before considering favorites, progress or ID review. Removed records retaining an actual resume position or watched state remain separate candidates for review; a removed/temporary flag alone is not proof they were cleared. This rule excludes old empty history without silently discarding retained viewing state.

Stremio's episode `state.watched` is an [anchor-aligned, zlib/base64 compressed bitfield](https://github.com/Stremio/stremio-core/blob/development/stremio-watched-bitfield/src/watched_bitfield.rs); its [bit order](https://github.com/Stremio/stremio-core/blob/development/stremio-watched-bitfield/src/bitfield8.rs) is least-significant-bit first. Decode using the ordered episode metadata and anchor video ID, following Stremio's own [episode sorting and bitfield reconstruction](https://github.com/Stremio/stremio-core/blob/development/src/types/library/library_item.rs). In the read-only probe, bitfields for IMDb-identified series aligned with available Cinemeta metadata; non-IMDb identifiers still require compatible episode metadata. Never infer episode identities solely from bit positions or `timesWatched`. Explicitly report missing anchors/metadata rather than assigning episodes speculatively.

Stremio's [likes service](https://github.com/Stremio/stremio-core/blob/development/src/types/rating/request.rs) exposes `get_status` per known media ID and type. A read-only probe confirmed `watched` and `loved` statuses among known library entries. This endpoint does not establish a way to enumerate statuses for titles absent from the library. Do not map `watched` or `loved` to VIPTV favorites by default; a separate product decision and storage model would be needed for likes/loves.

The built-in user-data export is documented, but [an open Stremio bug](https://github.com/Stremio/stremio-bugs/issues/614) reports missing library fields in its JSON. Inspect a real export before relying on it instead of `datastoreGet`.

## Reference plan for future mapping expansion

1. Read Stremio data locally, without logging credentials, auth keys or raw account records. Keep requests read-only and do not retain the Stremio token after the run.
2. Preview only confident mappings: saved non-removed/non-temporary items as VIPTV favorites; exact-ID and metadata-verified episode watched flags as progress corrections; usable positions/durations converted from milliseconds to seconds. Explain that completion for episodes with unknown runtime must not invent a duration.
3. Flag unsupported title IDs, missing/changed episode metadata, ambiguous episode mapping, and already existing VIPTV rows for review. Preserve Stremio's title-level recency only where its semantics are appropriate; do not fabricate per-episode watch dates or an event-by-event viewing history.
4. Only after approving a destination profile and reviewing the preview, write through a profile-authorized backend import operation with idempotency, bounded batches, conflict policy, backups and regression tests. A settings/account-web import UI can follow once the file/API contract and interaction are specified in `design`.

Limitations: Stremio's API is undocumented as a stable third-party contract; title-level `lastWatched` and watched flags cannot reproduce individual viewing events. VIPTV has no separate likes/loves state. Automated synthetic checks are not proof of a personal import or cross-device playback.
