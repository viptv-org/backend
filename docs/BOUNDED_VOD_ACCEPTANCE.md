# Bounded VOD acceptance — 2026-09-30

Actual trusted-HTTPS candidate acceptance passed for [web issue 5](https://github.com/viptv-org/web/issues/5).
The run used a fresh private synthetic SQLite fixture, the candidate Rust server,
the candidate web distribution, and a separate Caddy listener. Existing local
listeners at 18180 and 8443 stayed running. The harness stopped its exact owned
server, Caddy and named browser processes after completion.

## Qualified artifacts

| Artifact | Exact qualification identity |
| --- | --- |
| Rust runtime source | `8b257f91b6beb8d8aa06749099e697fb454c79c5` |
| Backend packaging/pins at qualification | `8eec3da9993cb31fda8a64042b5b89e9c1eea6e3` |
| Server binary SHA-256 | `2f4899274ec3964c248aa42de81a9ae67215eb9167a6f2f620c37b8bfb8635a5` |
| Web source and dashboard gitlink | `048651e941b4aad15cea69294419600bfb8612af` |
| Actually served candidate asset | `assets/index-Df4Sh2lc.js` |
| Asset SHA-256 | `ba765d5688bd3a24a2cd3b83c8292efde0333c3b995522ca1b231bd168931ef5` |
| Both design pins | `1dc92f7b4a571df00f89cc3915aaf1165a42bf94`, `ADMIN_V2.md`, ADM-002-VOD-WINDOW |

The packaging commits after the Rust runtime commit changed documentation/design
pins and the dashboard gitlink, without changing Rust runtime source. This
evidence/harness commit follows the packaging identity above.

## Fixture, trust and complete traversal

The reused populated seeder created 100,000 provider-101 title rows, 208 providers
owned by the member account, and a distinct foreign account/provider. The actual
shipped `provider-owners encrypt` and `encrypt-addons` commands encrypted provider
and addon configuration. The fixture gateway secret was sealed using a helper
that imports the exact runtime `Vault`; the helper verifies its round trip.
SQLite itself and the title columns were not encrypted. Fixture credentials were
random, stayed in a 0700 private temporary directory, and were never published.

Readiness required `curl` HTTP 200 and `ssl_verify_result=0` using system trust.
Chrome used `ignoreHTTPSErrors=false`. The browser consumed actual HTTPS API
responses. Fault cases aborted or delayed transport; they did not fulfill
requests with fabricated catalog or save responses.

The HTTPS API traversed all 99,999 initially unmatched provider-101 identities
forward and backward, in 2,000 pages per direction, with exact scalar identity
and ordering assertions. The seeded `vod:101:000000` was already matched and
therefore intentionally excluded. Every page held at most 50 titles, exact-total
metadata was absent, and the maximum observed opaque cursor was 208 ASCII bytes
(required upper bound 4,096). Following provider metadata cursors yielded all 208
owned IDs, including 605; foreign provider 201 was excluded.

| Browser viewport | Full unmatched titles forward/back | Forward adjacent loads | Reverse adjacent reloads | Maximum retained rows / pages / cursor slots / DOM rows | Region / row height |
| --- | ---: | ---: | ---: | --- | --- |
| 1440×900 | 99,999 / 99,999 | 1,999 | 1,997 | 150 / 3 / 9 / 20 | 540px / 112px |
| 390×844 | 99,998 / 99,998 | 1,999 | 1,997 | 150 / 3 / 9 / 20 | 506.390625px / 184px |

An initial page plus 1,999 forward loads covers 2,000 pages. On reversal, the
last three pages are already retained; 1,997 adjacent reloads return to the
initial page. The desktop interaction saved `vod:101:000009`, so the subsequent
phone traversal correctly excluded that additional matched title. Incoming
frontend page identities were checked against their exact expected offsets,
including that intentional exclusion.

The harness measured actual current model row/page/cursor-slot counters at every
adjacent load, separately from scalar visited extent and rendered DOM count.
The source window retains three pages with their current cursor metadata and a
scalar extent; it has no historical VOD page map. Neither scroll height nor DOM
count alone was used as retained-model evidence. Both region heights match
60vh within browser rounding, and row heights were measured on actual elements.

## Affected-state outcomes

Both reference viewports passed these actual-API browser cases:

- Provider 605 was selectable and displayed its real tail title. Failure on the
  second provider metadata page retained the first 200 options; `Try again`
  completed all 208 options.
- Full forward/end/back-to-start traversal reloaded evicted pages. A held then
  failed jump into an evicted forward spacer preserved visible retained rows,
  position and scalar extent; retry refilled adjacent pages to the destination.
- Page Down, Page Up and both arrow directions scrolled the labelled region.
- Cancel, Escape and browser Back closed only the match dialog, made no save,
  and restored the same raw opener identity, focus and scroll position.
- A transport save failure retained the metadata draft. A real HTTP-200 retry
  updated the selected row, announced success, and made its saved ID available
  through `Edit match`.
- The actual match save invalidated the catalog revision. The stale cursor
  retained rows and offered `Refresh titles`; refresh reset the list position.
- Directional read failure retained the current window and retried that direction.
- A delayed real provider request was canceled by a new provider selection;
  the new provider won. Type and debounced search filters reset correctly.
- Actual UI signout while a real read was pending canceled the previous scope.
  Signing in as the foreign account cleared previous provider, row and dialog
  state. Pending-save account cancellation is covered separately by frontend
  regression tests, rather than claimed as an actual-browser case here.

At 390×844, Chrome CDP native simulated touch also passed: a trusted touch tap
opened matching, a Cancel touch tap dismissed it, and a swipe scrolled the
region by 439px. The touch event had `isTrusted=true`. The harness waited for
native gesture scrolling to settle before subsequent modal assertions. This is
simulated browser touch evidence, not physical-phone evidence.

Private screenshots cover the populated provider-101 list and provider-605
filtered list/dialog at both sizes. Root visual inspection found the scoped
ADM-002 typography, layout, wrapped phone details, visible focus treatment,
desktop centered dialog and phone bottom sheet, readable controls, and no
horizontal overflow. Native phone Provider values truncate within the narrow
select normally. This is visual QA rather than pixel-parity proof. Screenshots,
fixture database, credentials and raw diagnostics remain private.

## Reproduce

From the organization workspace, build the desired backend/web candidates and
the fixture Vault helper, then run the harness with their explicit paths. This
example assumes the owning repos contain the candidates:

```sh
cargo build --release --locked -j 2 --manifest-path backend/server/Cargo.toml --bins
cargo build --release --locked -j 2 --manifest-path backend/scripts/populated-vault-helper/Cargo.toml
npm --prefix web run build
python3 backend/scripts/check-bounded-vod.py \
  --server backend/server/target/release/viptv-server \
  --provider-owners backend/server/target/release/provider-owners \
  --vault-helper backend/scripts/populated-vault-helper/target/release/populated-vault-helper \
  --dist web/dist \
  --certificate .local-https/certs/viptv.local.test.crt \
  --key .local-https/certs/viptv.local.test.key
```

Prerequisites are the existing local hostname/certificate trust setup, Caddy,
Python, Chrome and `playwright-cli`. The harness refuses a TLS bypass, chooses
fresh loopback ports, seeds fresh owned data, records candidate hashes, and
prints only sanitized pass status plus its private evidence directory. Use
`--api-only` for the complete API traversal without browser acceptance. Full
browser output includes separate JSON summaries for both viewports.

Qualification covers this synthetic candidate API/admin slice. It does not
establish physical-device playback, real provider ingestion, gateway/media
qualification, or production deployment.
