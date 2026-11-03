# BE-002 foundation

Startup initializes additive ownership/default/index tables without assigning
any legacy sources. The v2 HTTP routes use the bounded queries; legacy routes and
their current web consumers remain until the coordinated client cutover.

## Executable migration/storage groundwork

The v2 module provides explicit all-or-nothing provider ownership assignment,
persisted account live defaults, isolated per-request overrides, and account-
bound cursor pages/search. Tests cover ambiguous/incomplete/disabled ownership,
cross-account access, default stability/fallback and bounded unmatched results.
No implicit provider ownership migration or public source-sharing grant occurs.

## HTTP and migration

- GET/PUT `/api/iptv/matches` lists and edits only account-owned candidates.
  Pages default to 50 and reject sizes above 200, cross-account cursors and
  unknown query fields. Editing a foreign ID is indistinguishable from a missing
  ID. Operator role does not bypass ownership.
- GET/PUT `/api/iptv/live-default` persists the account default and validates
  explicit changes. Paired-device sessions and locked kids profiles cannot use
  these account-management endpoints. No viewing-client swap button was added.
- `provider-owners` inspects read-only and requires an explicit complete owner
  map for assignment. It creates a private SQLite online backup and streamed,
  versioned advanced-configuration export before changing the source. Invalid
  owner maps roll back; IDs, progress and manual VOD mappings are preserved.
- Migration library and executable fixtures verify WAL capture, non-overwrite,
  private permissions, confirmation and no credential output.

Not complete: admin/viewing-client adoption, account-scoped source CRUD/discovery,
encrypted credentials, catalog/index lifecycle, retirement of legacy modules,
full cutover/rollback qualification and backend gateway integration. See
[V2_OPERATIONS.md](V2_OPERATIONS.md) before using the migration executable.

## Gateway configuration

Account-owned gateway registration/check/update/grant APIs validate scoped
keys over a bounded, address-pinned HTTPS control client. Gateway integration
keys are encrypted with an operator-supplied, versioned AES-GCM keyring; there
is no plaintext fallback or implicit public/family grant. See
[GATEWAYS_V2.md](GATEWAYS_V2.md) for setup and the exact remaining boundaries.

An opt-in interoperability fixture issues a real scoped key against the
independent gateway executable and registers/checks it through the backend
router. It does not prove playback forwarding, public HTTPS deployment or
provider-secret migration.

## Playback control

The /api/playback lifecycle supports native direct delivery and independent
gateway-managed delivery, with scoped source validation, active affinity,
priority/capacity selection and no backend media-byte relay. A real isolated
container fixture verifies HLS playback/renewal/release through the independent
gateway with the backend engine idle. See PLAYBACK_V2.md for exact scope and gaps.

No current viewing client has been cut over, and legacy discovery/playback,
encrypted provider migration and remaining acceptance work are still pending.
