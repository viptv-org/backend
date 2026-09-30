# Isolated real-browser/runtime acceptance

This fixture runs the built account client against an actual backend image over
trusted HTTPS. It is not the mocked twelve-owner-route design harness and is not
production deployment approval. The backend image/source identity is checked
before a new synthetic tmpfs database is created. No developer environment,
operator secrets, host database or production volume is loaded.

Prerequisites: the workspace's existing local development certificate is trusted
by curl and Chromium, plus Docker, Caddy, `playwright-cli`, curl and Linux `ss`.
The runner does not install a CA or disable certificate verification. Certificate
and key paths must be readable absolute ASCII paths using letters, numbers,
slash, dot, underscore or hyphen. Port 18445 must be free; an occupied port
causes refusal, never replacement of a shared service.

```sh
bash scripts/check-browser-runtime.sh --sudo viptv:qualification-205a70a \
  sha256:9b2393c866d55321fabd21e61aba86035915e0e1ad1b28108bfb449cb9f94827 \
  205a70a06527a9b054e96b84e726e86a51b22fe1 \
  /mnt/ALPH/code/viptv-org/.local-https/certs/viptv.local.test.crt \
  /mnt/ALPH/code/viptv-org/.local-https/certs/viptv.local.test.key
```

The disposable nonroot/read-only backend uses an internal Docker bridge, no
published port and no keyring. Caddy connects to this exact container's verified
internal-network address, binds loopback only, and pins
the same HTTPS origin as the backend, and stores its own state in the private
fixture directory. Curl must report `200 0`; the unique isolated Chromium
session explicitly uses `ignoreHTTPSErrors: false` and loopback DNS mapping.
Browser contexts deny requests outside the fixture origin, including external
avatar images; fixture API responses are continued unchanged, not mocked.
An internal bridge is not a network-none sandbox: its host bridge interface is
reachable. No universal process/kernel egress-isolation claim is made here.

Each 1440×900 and 390×844 run creates a synthetic member account and checks:

- Registration/password mismatch, recovery-code presentation, profile creation
  and a browser-stored Secure/HttpOnly/SameSite=Strict session cookie.
- Real quality-free preference saving, reload/profile/session restoration.
- Account and eight member pages without horizontal overflow. The eight page
  transitions observe their real API responses and assert empty synthetic data;
  operator navigation is absent for this member.
- Actual logout/login and profile selection. Recovery invalidates the old
  password and used code; both fail with 401 and the new password signs in.
- Kids-profile restriction, protected navigation hidden, wrong PIN/focus and
  successful parent unlock.
- A second tab changes the PIN, genuinely revoking parent grants while a
  connection draft is open. A save gets real `parent_required`; protected
  portals are removed, wrong-PIN focus is retained, and the draft/masked field
  and dialog focus return after unlock. Saving is not automatically replayed.
  Explicit retry without a keyring returns `secret_store_not_configured` and a
  visible safe error. The separate image HTTP fixture verifies that missing-
  keyring refusals do not write connection records.
- Real device-code/deep-link approval, a token with no selected profile, exact
  account-owned profile IDs, and browser device revocation denying that bearer.

Passwords, the used original recovery code, all replacement recovery codes and current session-cookie values are checked
for absence from local/session storage. That is not an IndexedDB, OS-memory or
universal log audit. Credentials never appear in the returned public summary;
all CLI snapshots/logs run under a fresh mode-0700 evidence directory. Cleanup
targets only the owned browser session, proxy PID, container and network. Their
synthetic database is destroyed, not backed up for recovery. Private evidence
and diagnostic files remain local for inspection and are never committed.

The separate full-image HTTP harness additionally checks image packaging,
Docker health/security, tenant separation and retired-route refusal. This
browser fixture does not qualify populated IPTV/VOD data, owner/operator routes,
every authentication/expiry/cancellation permutation, gateway TLS/media,
physical devices, native stress/PiP, 4K/tracks, production ingress or deployment.

## Checked image — 2026-09-30

The command above passed eight groups at **both** viewports against exact image
`9b2393c866d55321fabd21e61aba86035915e0e1ad1b28108bfb449cb9f94827`,
source `205a70a06527a9b054e96b84e726e86a51b22fe1`, containing dashboard
`93c9316` and viewing bundle `db9c5ab`. System Chrome was used explicitly;
certificate errors were not ignored. Cleanup completed before PASS and verified
the owned browser, proxy, container, network and fixture listener absent.
Private evidence is retained locally at `/tmp/viptv-browser-image.wmLzVQ`.

Two earlier launcher attempts did not execute any browser groups: this host
does not support publishing the internal bridge through the proposed loopback
port, and its bundled Chromium cache was absent. Direct access to the verified
owned container address and explicit installed-Chrome selection corrected those
fixture dependencies. Both failed attempts independently verified cleanup; they
are not counted as browser acceptance or backend application failures.
