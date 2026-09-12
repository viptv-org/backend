# Integration fixtures and acceptance tests

Tests use synthetic media and fake provider credentials. They do not contact a real subscription or Roku. `smoke.py`, `incremental_providers.py`, and `account_smoke.py` mutate accounts/profiles/provider/addon state and therefore run only against a disposable database. `live_deployment_check.py` is deliberately non-destructive.

## Local disposable server

Generate/serve the fixture:

```sh
mkdir -p artifacts
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=640x360:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 20 -c:v libx264 -preset ultrafast -profile:v main \
  -pix_fmt yuv420p -g 120 -c:a aac -ac 2 \
  -movflags +faststart -n artifacts/fixture.mp4
python3 tests/mock_upstream.py --media artifacts/fixture.mp4
```

Build the server, initialize a disposable owner offline, then start it:

```sh
cargo build --manifest-path server/Cargo.toml --locked
rm -f artifacts/smoke.sqlite
export VIPTV_AUTH_ORIGIN=https://127.0.0.1:18080
export VIPTV_DATABASE=artifacts/smoke.sqlite
export VIPTV_MEDIA_DIR=artifacts/hls
printf '%s\n' 'account-acceptance-owner-password' | \
  server/target/debug/viptv-server create-admin acceptance-owner 'Acceptance Owner'
VIPTV_BIND=127.0.0.1:18080 VIPTV_DASHBOARD_DIST=dashboard/dist \
  server/target/debug/viptv-server
```

In another terminal run the owner-cookie smoke suite. Credentials are environment values, never command-line arguments:

```sh
VIPTV_TEST_URL=http://127.0.0.1:18080 \
VIPTV_TEST_ORIGIN=https://127.0.0.1:18080 \
VIPTV_TEST_USERNAME=acceptance-owner \
VIPTV_TEST_PASSWORD=account-acceptance-owner-password \
VIPTV_TEST_FFPROBE="$(command -v ffprobe)" \
VIPTV_TEST_FFMPEG="$(command -v ffmpeg)" \
python3 tests/smoke.py
```

`smoke.py` logs in through browser cookies with exact Origin/CSRF handling. It checks administrative authorization, redaction, profile favorites/progress, provider synchronization, addon discovery, incremental sources, FFmpeg HLS/transmux/transcode, unrestricted audio-track selection, subtitles, heartbeat, and cleanup. `VIPTV_TEST_PLAYBACK=0` skips media startup.

`account_smoke.py` deliberately creates a public member and paired device to prove zero-profile registration, one-account pairing, remote avatar profile creation, device non-admin scope, and revocation. Run it only against the disposable instance:

```sh
VIPTV_TEST_URL=http://127.0.0.1:18080 \
VIPTV_TEST_ORIGIN=https://127.0.0.1:18080 \
VIPTV_TEST_PUBLIC_USERNAME=acceptance-member \
VIPTV_TEST_PUBLIC_PASSWORD=account-acceptance-member-password \
python3 tests/account_smoke.py
```

## Provider isolation

Start a second fixture with delayed series detail:

```sh
python3 tests/mock_upstream.py --port 19091 --series-delay 3 --media artifacts/fixture.mp4
```

Then authenticate the imported `smoke` browser session in-process and run:

```sh
VIPTV_TEST_URL=http://127.0.0.1:18080 \
VIPTV_TEST_ORIGIN=https://127.0.0.1:18080 \
VIPTV_TEST_USERNAME=acceptance-owner \
VIPTV_TEST_PASSWORD=account-acceptance-owner-password \
python3 tests/incremental_providers.py
```

The script proves a fast provider can emit a playable event while a slow provider remains in progress and cleans up the disposable provider rows.

## Isolated Docker acceptance

After building `viptv:local`, run on a Docker host:

```sh
sudo bash scripts/container-check.sh
```

The wrapper uses a unique Compose project and test image, an internal-only network, fake credentials, temporary owner/profile/provider/addon state, generated media, bounded deadlines, and cleanup. It never loads the production `.env` or volume. The validation server creates its owner through the offline command, and browser smoke uses cookie/Origin/CSRF authentication.

## Live deployment check

`live_deployment_check.py` uses an **existing** owner account from `VIPTV_TEST_USERNAME` and `VIPTV_TEST_PASSWORD`. It checks health, unauthenticated rejection, cookie login, FFmpeg status, profile readability, container identity/isolation, SQLite permissions, and optional metadata search. It never registers users or creates/modifies profiles, providers, addons, favorites, or progress.

```sh
export VIPTV_TEST_USERNAME='existing-owner'
export VIPTV_TEST_PASSWORD='read-from-private-secret-store'
export VIPTV_TEST_ORIGIN='https://viptv.syek.tech'
python3 tests/live_deployment_check.py --env-file /secure/path/to/deployment.env --check-cinemeta
unset VIPTV_TEST_USERNAME VIPTV_TEST_PASSWORD
```

Do not place credentials in command arguments, repository files, terminal recordings, or published logs.

## Long rolling-HLS soak

`tests/sustained_playback.py` accepts an owner-only file containing a short-lived paired-device access token and an already discovered opaque source ID. This is intentionally a device-auth playback test, not an administrator shortcut:

```sh
python3 tests/sustained_playback.py \
  --base http://127.0.0.1:18080 \
  --device-token-file artifacts/soak-device-token \
  --stream-id-file artifacts/soak-stream-id \
  --expected-seconds 7200 --force-transcode --expect-mode transcode
```

The soak is real time because rolling HLS is paced. It audits sequence continuity, target duration, segment availability, heartbeat, decoded duration, natural ENDLIST, DELETE, and cleanup. Delete token/source files immediately afterward.

## Static deployment and host checks

```sh
python3 tests/test_host_check.py
bash -n scripts/host-check.sh scripts/container-check.sh
```

`test_host_check.py` uses fake Docker commands and verifies the read-only preflight, exact HTTPS auth-origin requirement, owner-only `.env`, redacted errors, and Compose fallback. `validate_deployment.cjs` validates YAML against a supplied official Compose schema and checks nonroot/read-only/no-secret/no-bootstrap invariants without loading `.env`:

```sh
npm install --prefix artifacts/deployment-tools --ignore-scripts --no-audit --no-fund ajv@8.17.1 yaml@2.8.2
curl -fL https://raw.githubusercontent.com/compose-spec/compose-spec/main/schema/compose-spec.json \
  -o artifacts/compose-schema.json
NODE_PATH=artifacts/deployment-tools/node_modules \
  node tests/validate_deployment.cjs artifacts/compose-schema.json
```

## Component gates

```sh
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path server/Cargo.toml --locked --all-targets
npm --prefix dashboard test -- --run
npm --prefix dashboard run build
```

Simulator and synthetic-media success do not certify physical Roku playback, real providers, DRM, or production throughput.
