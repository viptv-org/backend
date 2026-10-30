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
- `POST /auth/password {current_password,new_password}` verifies the current password and replaces the hash after the register length rule (at least 12 characters). The current session stays signed in, other sessions are not revoked, and the response carries a fresh CSRF value; a wrong current password returns `400 password_incorrect` and a short new password `400 password_too_short`. Like login and recovery it is rate limited with `429 rate_limited` and `Retry-After`. Neither the password nor its hash appears in a response, request log or telemetry field.
- `POST /auth/recovery-codes {}` regenerates the account's one-time recovery code and returns it once in `{"recovery_codes":[...]}`, invalidating the previous code. Any account session (owner or member) can call both credential routes for its own account; a paired TV gets `403 account_session_required`.
- `POST /auth/logout {}` revokes the current browser session.
- `GET /profiles` lists only profiles owned by the current account.
- `POST /profiles {name,avatar_style,avatar_choice?}` creates an owned profile atomically. Styles are the closed kid-friendly catalog worlds in `AVATAR_STYLES`; `avatar_choice` is the 1-based position within the world (at most 48, or the character world's size). The server owns the seed (`viptv-{style}-{choice}`, random without a choice) and emits the HTTPS `avatar_url`; profiles project `avatar_choice: null` when no catalog choice was made, and when a stored seed names a position its world does not have (data written before the per-world sizes, or by an import).
- `PATCH /profiles/:id {name,avatar_style,avatar_choice?,setup_complete:true}` completes or updates presentation without changing the stable profile ID or its history.
- `POST /auth/profile {profile_id}` selects a completed profile owned by the authenticated account.

Fresh accounts remain at zero profiles until an authenticated household browser or paired device creates one. Imported historical profiles retain their ID, favorites, and progress but require one presentation setup step. Catalog, discovery, favorites, progress, source, and playback access derive profile scope from the session; client profile headers cannot grant access.

## Device pairing

Pairing keeps the origin check, widened by the origin that served the request:
`device/code`, `device/login`, `device/token`, `device/refresh`, `device/revoke`,
`device/revoke-own`, `profile` and `logout` accept any HTTPS origin this server
answers on as well as the configured `VIPTV_AUTH_ORIGIN`, and refuse cross-site
requests. A TV loaded from a viewing hostname therefore pairs and claims its own
code without that hostname being configured, while the `verification_uri`,
`verification_uri_complete` and `qr_uri` it receives always name the canonical
(first configured) origin. Every other auth call stays pinned: a browser sign-in
or approval from the viewing hostname is still forbidden.

1. `POST /device/code {device_name, device_code?}` returns `device_code`, `user_code`, `verification_uri`, `verification_uri_complete`, `qr_uri`, `expires_in`, and `interval`. A TV asking for a fresh code may send the `device_code` it currently holds; the server then expires that device's earlier pending code so it can no longer be approved or redeemed (its token poll reads `expired_token`).
2. The QR contains only the complete dashboard activation URL. The server-generated PNG is bound to an unexpired user code and sent with `Cache-Control: no-store`.
3. A signed-in browser uses `POST /device/lookup {user_code}`, then confirms `POST /device/approve {user_code}` or denies with `POST /device/deny {user_code}`. Approval always binds to the current account; no target account or profile subset is accepted.
4. The TV polls `POST /device/token {device_code}`. Pending returns `authorization_pending`; approved one-time exchange returns a device access token and rotating refresh token. Exchange succeeds when the account has zero profiles.
5. The device can list, select, create, edit and delete profiles owned by its account, and change playback preferences for its selected profile. The primary profile cannot be deleted. Deleting a selected secondary profile clears that selection while retaining the device pairing; other viewers remain unaffected. A restricted kids session needs a current parent PIN grant before every profile mutation, including creation. PIN setup needs the owner; kids policy management stays with the household's account sessions. Leaving a selected kids profile or signing out requires parent PIN authority. It cannot manage accounts, providers, addons, matches, or other server settings—even when paired by the owner.
6. `POST /device/refresh {refresh_token}` rotates the device credential family. `GET /devices` and `DELETE /devices/:id` let the current account list and revoke its TVs. A paired device revokes the grant it is presenting with `POST /device/revoke-own {session_id?}` (its own grant when no ID is named; the legacy `POST /auth/device/revoke` accepts the same body); it can never revoke a sibling session of the account.

## Session caps

An account holds at most 32 sessions, browser and paired-device together, so a client that pairs repeatedly cannot fill the table. A sign-in at that limit evicts the account's oldest session of its own kind first — a browser login prunes browser history and never kicks a paired TV, and a new pairing prunes the oldest TV — falling back to the oldest session of any kind only when the account holds none of its own kind. Expired sessions are deleted before the count, so they never hold a slot: an account whose stored grants have all aged out can always sign in again, and an account at the cap always has room for the sign-in that lets its owner revoke a device. A server-wide bound separately prunes the oldest member sessions of any kind and never the owner's; its `429` names server capacity, not an account state. A viewer signed out everywhere therefore always recovers through `POST /auth/login`, `GET /devices`, and `DELETE /devices/:id`.

Store Roku access/refresh values only in private device storage and send access tokens in the Authorization header, never a query parameter. Pairing codes are short-lived, one-time, and not credentials by themselves. Revocation clears access immediately; there is no fallback credential.

## Deleting an account

`DELETE /auth/account {username,password,pin?}` deletes the signed-in account and
everything it owns in one transaction: profiles and their owners, progress and
history, favorites and library, approvals, per-profile and account preferences,
add-ons, IPTV connections and their sealed secrets, device pairings, sessions and
onboarding state. The typed `username` must equal the account's username and
`password` must verify; anything else is `400 account_delete_rejected` with no
row changed. When the account has a household PIN, `pin` must verify too:
`400 parent_pin_incorrect`. A paired device is refused with
`403 account_session_required` and the route shares the auth rate limit
(`429 rate_limited` with `Retry-After`). Success returns `204` and expires both
browser cookies.

An owner deletes its own account and its household like anyone else, including
when it is the server's only account. The one refusal is an owner whose server
still holds other accounts: they would be left without an owner, so it answers
`409 last_owner` until those accounts are deleted.

## Household settings and the owner

These writes belong to the server's owner and need an owner account session:

- `PUT /parent/pin` — set, rotate or remove the household PIN.
- `POST/PATCH/DELETE /addons` — the account's add-on inventory.
- `POST/PATCH/DELETE /iptv/connections`, `PUT /iptv/connections/:id/credentials`, `POST/DELETE /iptv/connections/:id/refresh`, `PUT /iptv/matches`, `PUT /iptv/live-default` — IPTV connections and match overrides.
- `POST/PUT/PATCH/DELETE /gateways`, `POST /gateways/:id/check`, `PUT /gateways/:id/grants` — playback gateways and their grants.
- `PUT/DELETE /profiles/:id/source-integrations` — the profile's source wiring, which the account owns.
- `DELETE /account/devices`, `DELETE /account/devices/:id`, `DELETE /auth/sessions`, `DELETE /devices/:id` for another session — signing other devices out.

A member account session gets `403 owner_required`; a paired device gets `403 account_session_required`. A paired device may still revoke the grant it is presenting with `POST /device/revoke-own`. Pairing, profile routes and parental policy stay open to any account session, so a member keeps its own profiles, limits and devices; only the household configuration is closed to it.

The credential routes stay self-service: `POST /auth/password`, `POST /auth/recovery-codes` and `DELETE /auth/account` act on the signed-in account for an owner or a member, and refuse a paired device with `403 account_session_required`. A member leaves the server by deleting its own account, and `409 last_owner` only refuses an owner while other accounts remain.

## Authorization and operations

Discovery jobs, registered source handles, playback controls, SSE, and bearerless media capabilities carry a server-authored account/profile/session lease. Every use revalidates the exact credential family, enabled account, selected completed profile, and ownership grant. Revoking any link invalidates the resource. Foreign handles cannot be replayed across sessions.

Status administration (`GET /auth/accounts`, `GET /auth/admin/devices`, `DELETE /admin/devices/:id`) requires the owner browser role and answers a closed forbidden response to anything else. Account inventory the account can read stays available to its own member sessions; a paired device reads only the add-on inventory.

Back up SQLite before upgrading. Migration tests must preserve existing profile IDs, favorites, progress values/timestamps, provider rows, and addon rows. Ambiguous historical multi-account profile grants must never be silently assigned to one account. Keep the prior image and a private pre-upgrade database backup for rollback; do not publish backups, session cookies, recovery codes, device storage, or deployment logs containing secrets.

Validate from `server/` with `cargo fmt --check`, `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings`. Real-media and physical-device validation remain separate gates.
