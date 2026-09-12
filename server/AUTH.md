# Account and device authentication

All routes are relative to `/api`. VIPTV has no shared administrator bearer, bootstrap HTTP claim, or anonymous application API. Browser requests use revocable account cookies; Roku uses a revocable paired-device bearer. Public health, registration/login/recovery, pairing initiation/polling, dashboard assets, and capability-scoped media URLs are the only unauthenticated entry points.

## First owner and public registration

A fresh installation has no account and no profile. Create its sole administrator offline while the service is stopped:

```sh
# Password is read from standard input, never a process argument.
read -rsp 'New owner password: ' OWNER_PASSWORD; printf '\n'
printf '%s\n' "$OWNER_PASSWORD" | viptv-server create-admin OWNER_USERNAME "Owner name"
unset OWNER_PASSWORD
```

Use a private prompt or secret-file descriptor in production rather than exporting a long-lived password. The command refuses to create a second owner and returns one one-time recovery code; store it privately. Existing deployments retain their owner and profile ownership through the additive migration.

`POST /auth/register {username,name,password}` is public and always creates an ordinary member with zero profiles. It can never create or become the administrator. Registration and other authentication work are bounded by persisted global and subject rate limits. `POST /auth/login {username,password}` starts a browser session. `POST /auth/recover {username,recovery_code,password}` revokes prior sessions and rotates recovery material.

## Browser sessions and profiles

Browser authentication sets Secure, HttpOnly, SameSite=Strict access and refresh cookies. Serve the application through HTTPS. Cookie-authenticated writes require both an allowed `Origin` and the current `X-CSRF-Token`; no permissive CORS or forwarded-origin override is enabled. Keep passwords, recovery material, refresh secrets, and CSRF values out of URLs and browser storage.

- `GET /auth/status` reports public registration and current-session status without exposing account names.
- `GET /auth/me` returns the authenticated account, active profile ID, capabilities, and current CSRF value.
- `POST /auth/refresh {}` rotates browser cookies; replay revokes that credential family.
- `POST /auth/logout {}` revokes the current browser session.
- `GET /profiles` lists only profiles owned by the current account.
- `POST /profiles {name,avatar_style}` creates an owned profile atomically. Styles are a closed kid-friendly DiceBear allowlist; the server owns the opaque seed and emits the HTTPS `avatar_url`.
- `PATCH /profiles/:id {name,avatar_style,setup_complete:true}` completes or updates presentation without changing the stable profile ID or its history.
- `POST /auth/profile {profile_id}` selects a completed profile owned by the authenticated account.

Fresh accounts remain at zero profiles until an authenticated household browser or paired device creates one. Imported historical profiles retain their ID, favorites, and progress but require one presentation setup step. Catalog, discovery, favorites, progress, source, and playback access derive profile scope from the session; client profile headers cannot grant access.

## Device pairing

1. `POST /device/code {device_name}` returns `device_code`, `user_code`, `verification_uri`, `verification_uri_complete`, `qr_uri`, `expires_in`, and `interval`.
2. The QR contains only the complete dashboard activation URL. The server-generated PNG is bound to an unexpired user code and sent with `Cache-Control: no-store`.
3. A signed-in browser uses `POST /device/lookup {user_code}`, then confirms `POST /device/approve {user_code}` or denies with `POST /device/deny {user_code}`. Approval always binds to the current account; no target account or profile subset is accepted.
4. The TV polls `POST /device/token {device_code}`. Pending returns `authorization_pending`; approved one-time exchange returns a device access token and rotating refresh token. Exchange succeeds when the account has zero profiles.
5. The device can list, select, create, edit and delete profiles owned by its account, and change playback preferences for its selected profile. The primary profile cannot be deleted. Deleting a selected secondary profile clears that selection while retaining the device pairing; other viewers remain unaffected. A restricted kids session needs a current parent PIN grant before every profile mutation, including creation. PIN setup and kids policy management retain their separate household-browser authorization. Leaving a selected kids profile or signing out requires parent PIN authority. It cannot manage accounts, providers, addons, matches, or other server settings—even when paired by the owner.
6. `POST /device/refresh {refresh_token}` rotates the device credential family. `GET /devices` and `DELETE /devices/:id` let the current account list and revoke its TVs.

Store Roku access/refresh values only in private device storage and send access tokens in the Authorization header, never a query parameter. Pairing codes are short-lived, one-time, and not credentials by themselves. Revocation clears access immediately; there is no fallback credential.

## Authorization and operations

Discovery jobs, registered source handles, playback controls, SSE, and bearerless media capabilities carry a server-authored account/profile/session lease. Every use revalidates the exact credential family, enabled account, selected completed profile, and ownership grant. Revoking any link invalidates the resource. Foreign handles cannot be replayed across sessions.

Provider, addon, match, account inventory, and status administration require the owner browser role. Ordinary accounts and paired devices receive a closed forbidden response.

Back up SQLite before upgrading. Migration tests must preserve existing profile IDs, favorites, progress values/timestamps, provider rows, and addon rows. Ambiguous historical multi-account profile grants must never be silently assigned to one account. Keep the prior image and a private pre-upgrade database backup for rollback; do not publish backups, session cookies, recovery codes, device storage, or deployment logs containing secrets.

Validate from `server/` with `cargo fmt --check`, `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings`. Real-media and physical-device validation remain separate gates.
