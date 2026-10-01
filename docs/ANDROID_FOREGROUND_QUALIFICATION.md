# Isolated native foreground fixture

`scripts/check-android-foreground-fixture.py` runs an actual backend binary with
a private synthetic database and real account/profile/device authentication.
It is a fixture controller, not a backend implementation or deployment.

Pass explicit paths for the inactive synthetic seed, actual gateway-generated
finite media configuration, private gateway namespace owner record, backend
binary, existing local certificate/key/CA and a fresh private artifact directory.
The caller supplies the isolated provider address and emulator bridge host:

```sh
python3 scripts/check-android-foreground-fixture.py \
  --previous-fixture "$SYNTHETIC_SEED" \
  --gateway-source "$GATEWAY_SOURCE" \
  --gateway-private "$GATEWAY_OWNER" \
  --server "$BACKEND_BINARY" \
  --certificate "$LOCAL_CERTIFICATE" --key "$LOCAL_KEY" --ca "$LOCAL_CA" \
  --artifacts "$FRESH_ARTIFACTS" \
  --isolated-address "$ISOLATED_PROVIDER_ADDRESS" \
  --emulator-host "$EMULATOR_BRIDGE_HOST"
```

The controller checks the synthetic keyring/configuration markers, exact seeded
`qa_*` accounts/profiles, synthetic provider names and expected gateway schema
marker before copying through a read-only SQLite backup connection. Existing
synthetic providers/addons are disabled in the new database before the service
starts; no production database, provider credential or developer env is read.
The backend runs as the caller's UID/GID with supplementary groups cleared in
the existing disposable gateway network-none namespace. The addon controller
checks that namespace differs from the host and has only loopback. Its supplied
public-classified address remains inside that namespace, preserving actual
backend private-address rejection. No host address or TLS trust changes occur.

A minimal real addon supplies catalog, metadata and a header-protected stream.
Registration uses the account's normal `/api/v2/addons` route and verifies the
stored configuration is encrypted. Media uses the supported `X-API-Key` source
header; no source-header whitelist exception is added. The finite provider is
materialized from actual authenticated gateway output with original segment
bytes/durations and a separate fixture storage cap. The controller serves these
files over the existing trusted local certificate. Both host TLS listeners bind
loopback only; the emulator's host bridge reaches those listeners. It does not claim the
gateway's rolling VOD playlist itself has become a finite native contract.

Before readiness the fixture performs actual native password login, profile
selection, catalog/metadata/source discovery, saves progress at 20/120 seconds
with the returned exact source fingerprint, starts a native direct backend
lease at position20 with nonempty request headers, heartbeats and releases it.
The gateway viewer is a separate fixture-owned lease; the native app owns its
normal backend lease. Releasing one does not pretend to release the other.

The private `native.json` provides the native origin, host/control origin, CA,
synthetic credentials/profile and media ID. It must never be logged or committed.
`evidence.json` records sanitized preflight results and the actual binary hash.
`safe-requests.jsonl` contains only method/path/status, not headers, query values
or response bodies. Other diagnostic artifacts remain private.

## Transport controls

`GET /__control` returns safe `identityCalls`, `pairingStarts`, `refreshCalls` and
`successfulRefreshes` counters. Successful refresh increments only after the
actual server returns200, before any requested response hold.
`POST /__control` accepts:

- `delayIdentity`: milliseconds before forwarding actual identity reads.
- `delayRefreshResponse`: forward actual device refresh first, receive its real
  rotated-token response privately, then hold delivery for the requested
  milliseconds. This exposes cancellation after server-side token rotation.
- `offlineIdentity`: close the transport before returning an identity response.
- `failIdentityOnce401`: alter one forwarded bearer token so the actual server
  rejects it; subsequent identity reads retain the app's actual token.
- `rejectRefresh401`: alter forwarded refresh tokens so the actual server rejects
  refresh; reset it to false after the explicit revocation scenario.
- `approvePairing`: approve the latest actual UI device code through the real
  browser account grant and current CSRF token.

The proxy never fulfills successful backend API responses. These controls test
real native recovery/refresh paths without changing backend authentication.
Delay values must be integer milliseconds from0 through40000. All other control
values must be booleans; unknown fields and invalid scalars return400 before
changing any control state. The fixture also creates a completed alternate
account-owned profile through the normal API for response/profile replacement
checks; both profile IDs/names are available in private `native.json`.

Send SIGTERM to the recorded controller owner PID after device acceptance.
It stops only its owned backend/addon/TLS processes and retains the private
database/evidence. Stop the separate gateway fixture with its owner's cleanup
helper afterward; verify viewer release and restored input/output/viewer
reservations before claiming complete cleanup.

Each sudo/nsenter process tree runs in its own session, recorded in
`owned.json`; teardown signals that whole owned process group so an interrupted
wrapper cannot leave an orphaned addon relay behind. Grace and escalation wait
for the owned process group, independently of wrapper exit; a surviving group
is a teardown failure, not a successful cleanup report. Fresh-start qualification
uses only loopback TLS listeners and validates synthetic seed/control inputs.
Pairing approval uses a fresh real browser login for each control request. An
extended native run reproduced expiration of the initial approval session
(`/api/auth/me` returned401); an expired fixture login is setup failure, not a
native pairing or reconnect verdict. Approval failures return sanitized status
without fabricating authentication success.
API/addon/relay responses explicitly advertise `Connection: close`, matching
their HTTP/1.0 handler lifecycle. A dispatch-proven native profile POST immediately
after identity read returned transport I/O failure before a completed backend
request. Request shape/origin and Core phase/profile membership were valid;
cancellation was ruled out. One QA-only client-close-header discriminator made
the identical held-refresh profile switch and return pass without write replay.
The client workaround was removed; final ordinary-shell acceptance must use the
explicit-close fixture, not count the diagnostic build as final evidence.
The delayed-refresh control holds the actual successful rotation response after
the server has committed it, exposing native cancellation bugs without fake
authentication responses. Final process-group teardown is checked separately
after the native acceptance run finishes.

## Current preflight evidence

The actual candidate binary at backend source `305a8df` passed native login,
profile selection, encrypted addon registration, catalog/metadata/discovery,
exact-source progress20/120, direct playback position20 with required media
headers, heartbeat and release. Its SHA-256 is
`2f4899274ec3964c248aa42de81a9ae67215eb9167a6f2f620c37b8bfb8635a5`.
Host HTTPS health returned200 with certificate verification0; requesting the
media playlist without its required header returned403 with verification0.
The supplied 120-second real H264/AAC gateway remux produced91 retained
segments,11,806,212 bytes under a32MiB fixture collector cap. A real FFmpeg9
decode of fetched gateway media and finite-HLS seek20/decode2seconds passed.

This preflight is not physical-device, foreground UI, decoder-quality,
production deployment or universal gateway acceptance. Android owns subsequent
emulator evidence, including affected route/focus, timeout/retry, real refresh,
pairing, Resume and independent playback cleanup outcomes.

## Helper lifecycle qualification

Helper source `aeaff96b1ba126555d6b6aee07333d441532ee9f` passed fresh
preflight on separate loopback TLS18448/18449 and namespace18196/18095 ports.
Through actual authenticated HTTP, DELETE `/api/auth/sessions` revoked all
synthetic account browser sessions, including the fixture's initial approval
session. A subsequent actual device-code request, fixture approval and native
token exchange succeeded for the same account. This repeated twice with trusted
TLS; no stale browser session or fabricated native grant was used.

Normal owner SIGTERM exited0 and left no recorded owner/process-group member
or test listener. A second fresh run deliberately interrupted only the recorded
relay launcher with SIGKILL. Its child still served actual HTTPS health200;
subsequent owner SIGTERM exited0 and removed that child and both owned groups.
Namespace18196/18095 and TLS18448/18449 were absent afterward. Shared18180/8443
and the still-active native fixture18444/18445 remained listening.

These checks qualify fixture containment, not the Android product or production
rollback.

## Final ordinary-client qualification and cleanup

Explicit-close helper source `7ea0c60908919d4b8b1b3ba28f2327da7688e4a2`
passed fresh preflight and advertised `Connection: close` with trusted HTTPS.
The ordinary Android shell (source `ce0f33b`, no diagnostic client header/logs)
then passed the held successful-refresh profile-choice loop: actual profile
POST200, profile4 Home, explicit profile2 restoration POST200/Home, exactly one
successful rotation and no extra pairing. Android PR7 and its
`qualification/FOREGROUND_ACCEPTANCE.md` own product/normal-APK evidence.

Native fixture5 owner SIGTERM exited0. Its owner and both recorded process
groups were absent; namespace18195/18094 and TLS18444/18445 listeners were gone.
The independently owned gateway launcher/materializer/relay had already exited,
but its exact isolated container still ran. Authenticated live inspection found
the old viewer lookup404; idempotent DELETE returned204 and all two
input/output/viewer reservations were available. There was no FFmpeg child and
zero media-cache files before removing that exact test container. Do not call
the absent viewer an observed explicit released/expired status.

All dedicated emulators and fixture containers/listeners are stopped; shared
18180/8443 remain up. Private synthetic data, source media and sanitized evidence
were retained. No production, shared-stack, merge or deployment action occurred.
