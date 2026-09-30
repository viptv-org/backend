# Populated actual-image admin qualification — 2026-09-30

This is synthetic local qualification, not deployment or complete admin acceptance.
Runtime image `sha256:5b5900c8697b89e3519a31fb6685e21e75343b0e3caa3861de697b1aead8b6ef`
is source `27a0296c69755e50412a6efeb9ad52564e029fd3`, dashboard `93c9316` and
viewing client `db9c5ab`. The harness uses unchanged actual local API responses,
trusted Chrome HTTPS, a fresh internal bridge, an owned synthetic volume and
loopback port18446. No private environment, production data or shared8443 stack
is loaded. An internal bridge is not universal kernel/host egress isolation.

The offline seed tool alone adds Python. Shipped `create-admin` initializes
schema/password hashing; explicit synthetic members/profiles/ownership/defaults
and raw rows are inserted offline. Shipped migration CLI encrypts provider/addon
records. Refresh rows are cancelled so no subscription import starts. Gateway
metadata is an **unverified offline seed**, encrypted using the actual imported
`server/src/secret_store.rs` Vault, not handwritten crypto. That source is unchanged
from runtime27a and SHA256 is
`eeae62ba8e6f11fa9f4e5cece966a35e37fcfeebc71279614a2e1eb325e4dde4`.
Its helper has a separate locked manifest; three actual Vault tests and strict
Clippy pass. Tested seed-tool image was
`sha256:d27053a2077e66dcf27820c2d857161677728a308dfc2412e64e067a06ca43fb`.

Build only the seed helper from the listed public files, never an entire private
workspace context:

```sh
tar -cf - scripts/populated-seed.Dockerfile scripts/populated-seed.py \
  scripts/populated-vault-helper/Cargo.toml scripts/populated-vault-helper/Cargo.lock \
  scripts/populated-vault-helper/src/main.rs server/src/secret_store.rs \
  | sudo docker build --pull=false --build-arg RUNTIME_IMAGE=viptv:qualification-27a0296 \
      -f scripts/populated-seed.Dockerfile -t viptv:populated-seeder-27a -

python3 scripts/check-populated-runtime.py --sudo \
  --certificate /mnt/ALPH/code/viptv-org/.local-https/certs/viptv.local.test.crt \
  --key /mnt/ALPH/code/viptv-org/.local-https/certs/viptv.local.test.key
# Repeat with --many-providers for the separate 208-owned-provider case.
```

Final baseline and many-provider runs passed at1440×900 and390×844. Baseline:

- Actual UI default-live and scope saves, encrypted addon metadata/redaction.
- Actual100k-row SQL dataset,20 cursor pages with ≤50 rows/response and20 rendered
  DOM rows, selected owned metadata-match save.
- Actual foreign-account list isolation and hidden-existence mutation refusal.
- Actual operator account UI reads and owned gateway/grant API reads/saves;
  invalid peer check returned502 `gateway_dns_unavailable`, without secret leakage.
  No successful network-verified gateway registration/capacity claim is made.

Confirmed **pre-existing open gaps**, not hidden by bounded DOM:

1. At both viewports virtual/model extent grew from50 to1050 rows after20 pages.
   `useCursorResource` concatenates rows and retains historical duplicate/cursor
   guards; this does not establish bounded long-traversal data/metadata retention.
2. With208 owned providers, the real API could read tail provider605 and its VOD
   row, but the UI dropdown contained only200 provider options and omitted605.

Private final evidence: `/tmp/viptv-populated-ppbxh39r` (baseline),
`/tmp/viptv-populated-qw39o9e3` (many providers). Directories0700, files0600;
browser output/source echoes can contain synthetic credentials and never ship.
Actual result JSON is parsed separately from CLI source echo. Earlier harness
dependency/locator failures do not count as application regressions or acceptance.
The exact task-created initial CLI artifacts were moved privately to
`/tmp/viptv-populated-cli-recovery.QdAh2V`, not committed.

Owned browsers/proxies/containers/networks/volumes were removed and port18446 is
closed. No UI/runtime/frozen wire changes, physical device/media qualification,
real subscriptions, production migration or deployment occurred. The two gaps
remain separate follow-up work; this checkpoint must not be described as full
populated/operator or bounded-model completion.
