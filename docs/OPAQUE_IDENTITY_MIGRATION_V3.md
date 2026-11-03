# Private identity isolation: implementation plan

This is active implementation work following the canonical child-consumer gate. A temporary 422 for private IDs is a boundary during migration, not completion of the requirement.

## Current keys

| Record | Current key / authority |
| --- | --- |
| Backend favorites, progress, watch counts | profile + kind + raw ID |
| Backend continuation cache | profile + parent raw ID + current raw ID; origin only inside cached JSON |
| Kids observations / ambiguity | account + kind + raw ID |
| Kids approvals | profile + kind + raw ID |
| Source job / registered stream | session/profile lease + kind/raw ID + source configuration; metadata origin not retained |
| Gateway playback proof | session/profile lease + kind/media ID/exact VOD + source configuration |
| Subtitle selection | session/profile/configuration + kind/raw ID + account add-on revision |
| Verified canonical card cache | session/profile/configuration + metadata origin + kind/raw ID + add-on revision |
| Shared native favorites/playback | host connection/account/profile + kind/raw ID |
| Membership snapshot | identities only; cannot distinguish two private providers' same ID |

## Stable connector incarnation

Persist an additive random incarnation for each live configured connector. Preserve it across credential edits, manifest refresh, enable/disable and ordinary revision changes. A delete followed by reinstall or numeric-ID reuse gets a fresh incarnation. Existing backend `addons.id` is not AUTOINCREMENT, so numeric ID alone is insufficient. Revision is retained separately for fetch/cache/grant fences; it is never part of the durable history/favorite key.

Do not assign current connector incarnations to old ambiguous policy, library or progress rows. They remain explicitly legacy. Canonical public identities retain their existing cross-provider identity rules; a client-supplied external-ID claim does not make a private ID canonical.

## Smallest complete additive migration

1. Introduce a typed optional metadata origin (provider, connector and incarnation) on cards/history transport, separate from provider display/routing text. Validate ownership, assignment, incarnation and current revision at host boundaries. Carry only server-verified provenance into policy observations and playback grants.
2. Keep canonical/legacy tables and their import compatibility. Add origin-scoped private tables for favorites/progress/watch counts/continuation and policy observations/approvals/episode ambiguity. Private parent edges remain in the same origin. Reads include owned legacy records honestly; identity-only mutations do not target another origin's rows.
3. Extend membership with scoped identities while preserving existing canonical identities. KMP and native keys must include origin for private cards. Removal of an owned saved private record remains possible after its connector is disabled/deleted, without granting access to its content.
4. Carry origin through source Job → StreamEntry → gateway Proof, subtitle Selection, next-episode request/cache and progress/history recording. Every admission, poll, heartbeat, caption load and resume lookup rechecks the same origin-specific title authority. Mutable revisions revoke stale access without erasing saved records.
5. Preserve legacy opaque records as legacy authority. New explicit private-origin consumers require matching verified origin; they never inherit a legacy same-ID grant. Canonical identities retain verified shared matching without merging arbitrary provider-private strings.
6. Test two assigned connectors with identical private IDs and different metadata/episodes, restart persistence, credential/enablement/manifest edits, connector deletion and same-ID recreation, account/profile boundaries, legacy migration/revocation, source/playback/subtitle/next/resume, and canonical cross-provider continuity. Shared Rust changes are made in canonical core first, generated and synchronized through the normal native workflow.

No storage/API migration is deployed to production during this work. All migration and HTTP tests use disposable data.

