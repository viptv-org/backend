# Engine-free current-data rollback fixture

This is a selected-image development rehearsal, not permission to migrate or
roll back production. It does not restore retired globally scoped media/family
code, replace current history from an old backup or downgrade the v2 protocol.

## Selected compatible release pair

- Prior engine-free backend: source `205a70a06527a9b054e96b84e726e86a51b22fe1`,
  image `sha256:9b2393c866d55321fabd21e61aba86035915e0e1ad1b28108bfb449cb9f94827`.
- Current cursor-bound backend: source `27a0296c69755e50412a6efeb9ad52564e029fd3`,
  image `sha256:5b5900c8697b89e3519a31fb6685e21e75343b0e3caa3861de697b1aead8b6ef`.
- Both full images contain dashboard `93c9316` and viewing `db9c5ab`, with Core8
  v2 consumers. The independent gateway is not replaced by this fixture.

The prior image still lacks the current over-bound cursor-encoder refusal. A
rollback can reintroduce that known oversized-identifier paging failure; this is
not a reason to truncate IDs or resurrect legacy clients. The fixture uses
ordinary provider IDs and verifies its exact tested compatibility, not every
possible future schema/image or source format.

```sh
sudo -n python3 scripts/check-rollback-images.py \
  sha256:9b2393c866d55321fabd21e61aba86035915e0e1ad1b28108bfb449cb9f94827 \
  205a70a06527a9b054e96b84e726e86a51b22fe1 \
  sha256:5b5900c8697b89e3519a31fb6685e21e75343b0e3caa3861de697b1aead8b6ef \
  27a0296c69755e50412a6efeb9ad52564e029fd3
```

Root access is required only for this local test's offline SQLite inspection of
the uniquely created, labelled Docker volume. No caller volume/database path is
accepted; names and ownership labels are revalidated before access and removal.
Existing images are checked by exact digest, source label and UID10001; never
pulled. A setup-only root helper uses CHOWN/FOWNER on this newly created mount.
Normal servers are network-none, nonroot/read-only/cap_drop ALL, with no ports or
host directories. The synthetic keyring is random and never printed or reused
as application configuration. No developer environment or production data is read.

## Actual rehearsal

The fixture first registers a synthetic member/profile through the real current
HTTP API and obtains browser and native sessions. With all normal writers
stopped, it seeds provider/raw-live/VOD/match identities, a historical composite
favorite, progress, a hidden queue entry and archived quality JSON. Shipped
`provider-owners` commands explicitly assign addon ownership, encrypt provider
and addon credentials, create private backup/exports and retire a reviewed
synthetic family table. Raw account/profile/history/source IDs remain unchanged.

It starts prior → current → prior on **the same current database**, without
restoring any older snapshot. Each image accepts the original native session,
browses both cursor directions and the persisted default, reads encrypted addon
configuration and actually resolves an encrypted live source into an opaque
redacted handle. Management uses the browser session; paired/native device
restrictions are not bypassed. Historical composite playback remains unavailable
and old media routes require a client update.

The current image writes a later position (456.75 seconds), an additional watched
title (99 seconds) and quality-free Japanese/audio/autoplay preferences through
the real API. The rollback image sees those later writes. Offline quick/FK checks
and exact row-count/hash comparisons cover 22 preserved tables, including active
session hashes, ownership, ciphertext, raw indexes, source matches, default,
favorites, hidden queue and the retirement marker. Only the deliberate progress,
preferences and mirrored autoplay write may change before the new baseline;
after rollback every sampled table must be byte-equivalent at the row level.
Archived `quality:1080p` stays stored but is absent from the active preference
response, and the retired table must not reappear.

2026-09-30: all four setup/image groups passed with those exact images. Private
evidence is `/tmp/viptv-rollback-images-hgbxvsis`; its summary contains table
counts/hashes, not row contents, passwords or keyring. The exact test containers
and volume—including synthetic DB and private preservation artifacts—were
removed; only private diagnostic summaries remain. Early setup/fixture-contract
failures are not application rollback failures or counted acceptance.

## Production decision boundary

Before any separately approved cutover, identify and retain exact backend,
gateway, frontend and all client release artifacts, plus the existing private
origin/keyring/environment. Stop writers, make a fresh consistent preservation
snapshot, verify IDs/history/configuration, and select a rollback image whose
current-data compatibility has actually been tested. Keep the gateway stable
unless its independent compatibility/lease-drain gate has passed too.

This pair does not prove legacy-production→v2→legacy restoration. Do not blindly
replace the current database with a pre-encryption/pre-retirement backup: newer
history would be lost and old family/global APIs could violate ownership. Prefer
a reviewed forward fix or a compatible engine-free release. Preservation
artifacts require their own private retention and routing/ownership review.
Missing keys are not fixed by restoring plaintext. Never `down -v` production.

Backend restart may invalidate in-memory source/playback handles; clients must
reconnect rather than assume an uninterrupted stream. This HTTP fixture does not
qualify browser TLS, public ingress, active gateway/media migration, physical
devices, native/PiP/HDR stress, operator secrets or production deployment. Those
remain independent checklist gates.
