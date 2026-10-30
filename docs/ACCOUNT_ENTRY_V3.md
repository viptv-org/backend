# Browser account entry contracts

These are the existing local account/password and recovery-code contracts. They do not verify an email address or send email/OTP messages. A username may contain `@`, but remains a username. Public registration creates a member; initial owner creation is an existing offline operator operation.

## Browser transport

Use the browser's Secure/HttpOnly/SameSite session cookies. Public register/login/recover calls require the pinned HTTPS Origin. `GET /api/auth/status` returns `registration_enabled`, `authenticated` and `csrf_token`; authenticated cookie mutations use that CSRF token. Status can report authenticated when the refresh cookie remains valid after access expiry. In that case refresh before relying on `/api/auth/me`.

`GET /api/auth/me` returns the account `{id,username,name,role}`, selected `profile_id`, owned profiles, profile-management flags and CSRF token. Account-only entry and TV approval do not require Home/catalog/profile metadata requests.

## Registration and recovery

| Operation | Request | Result |
| --- | --- | --- |
| `POST /api/auth/register` | `{username,name?,password}` | New member browser session, `profile_id:null`, `profiles:[]`, one-time `recovery_code` and single-element `recovery_codes` |
| `POST /api/auth/login` | `{username,password}` | Browser session; no recovery code is disclosed |
| `POST /api/auth/recover` | `{username,recovery_code,password}` | Replaces the password and recovery code, revokes old account sessions and approved pairings, creates a new browser session |
| `POST /api/auth/password` | `{current_password,new_password}` | Verifies the current password, replaces the hash after the register length rule, keeps this session and the other sessions signed in, and returns a fresh `csrf_token`; a wrong current password is 400 `password_incorrect`, a short new password 400 `password_too_short` |
| `POST /api/auth/recovery-codes` | `{}` | Replaces the account's one-time recovery code and returns it once in `recovery_codes`; the previous code stops working |

A normalized response example uses synthetic values only:

```json
{
  "account_id": "42",
  "profile_id": null,
  "profiles": [],
  "csrf_token": "synthetic-csrf-example",
  "recovery_code": "synthetic-example-not-a-valid-recovery-code",
  "recovery_codes": ["synthetic-example-not-a-valid-recovery-code"]
}
```

The exact response also includes session metadata. Access/refresh cookies belong to the browser; do not persist them in JavaScript storage.

Usernames are trimmed/lowercased ASCII alphanumeric strings with `.`, `_`, `-`, or `@`, bounded to 80 bytes. Display names are nonblank and bounded to 80 bytes. Current password validation bounds the UTF-8 value to 12–256 bytes; the existing short-password response says “12 characters.” Do not silently introduce a different client policy. Unknown usernames, incorrect passwords and invalid/used recovery codes are refused; duplicate registration returns 409.

Capture and present the replacement recovery kit immediately after a successful register/recover response, before optional bootstrap requests. If bootstrap fails, retain the one-time kit in memory and retry bootstrap, rather than replaying the account mutation. Passwords and recovery codes must not enter preference/session storage, logs, analytics or diagnostic `toString()` output. Acknowledge the recovery kit before moving on.

Recovery retains profiles, saved titles and watch progress. The old password, used recovery code and old session families must fail afterward. A paired app cannot administer account entry through its restricted device role.

## First profile and parent administration

A new member has no profile. `GET /api/v3/profiles` and `POST /api/v3/profiles {name,avatar_style?,avatar_choice?}` (legacy `avatar_id?` still accepted) work with an account-only session. Create/select a real profile before media requests. Profile selection is `POST /api/auth/profile {profile_id}`. Existing onboarding state is available through `/api/auth/setup`; its accepted fields are household_name, timezone, language, completed, step and profile_id.

Household configuration is owner-only: the household PIN (`PUT /api/v3/parent/pin`), add-ons, IPTV connections and matches, gateways, source integrations and signing other devices out answer `403 owner_required` to a member session and `403 account_session_required` to a paired device. Profile and parental-policy routes stay with any browser account session. Parent operations are:

- `GET /api/v3/parent/status` → `{pin_configured,unlocked,restricted}`.
- `POST /api/v3/parent/unlock {pin}` → a session-bound, one-shot grant lasting 120 seconds. Wrong PIN is 403; five failures trigger a five-minute lockout (429).
- `PUT /api/v3/parent/pin {pin,current_pin?}` sets/rotates the PIN; an empty new PIN removes it only when no kids profiles remain. PINs contain 4–8 ASCII digits. Rotation/removal requires the current PIN and clears grants.
- `GET/PUT /api/profiles/:id/kids` reads/changes `{enabled,max_age}`. Age ceilings are integers 0–17. Enabling requires a configured account PIN. Ownership and parent restrictions apply, policy revision changes invalidate old media leases, and the client must refresh selected-profile content.

Selecting another profile consumes an unlock grant. An authenticated account session can change its own password and regenerate its recovery code, but there is no account-name editing API, email-delivery API, or forgotten-parent-PIN reset contract in this slice; do not present those operations as completed.

The v3 profile-management projection preserves `kids` as a JSON boolean and returns the actual integer `max_age` alongside it. This is not a nested policy object. Older servers may omit `max_age`; clients should represent an unknown ceiling instead of fabricating one. A regression checks ordinary-profile `false/12` and an actual configured kids policy `true/7`.

The browser TV-approval page uses **GET** `/api/auth/me` after status. The real router deliberately returns 405 for POST; its account-only restore test now asserts HTTP methods so a path-only mock cannot hide this mismatch. A real Chromium run against the disposable trusted-HTTPS normal backend verified fresh browser sign-in and explicit approval with the correct account and no selected device profile. A second run expired only that synthetic browser session's access timestamp during confirmation: deny performed one refresh and one successful mutation, removed its pairing, and made no Bootstrap, profile-list, or catalog requests. The corrected isolated frontend then passed the full real Chromium flow: expired restore used GET me401 → status/refresh200 → GET me200; explicit approval and expired-access denial passed with the expected account and profile-null device scope, 14 API requests, zero page errors, and no Bootstrap/catalog/profile requests. Completion screenshots contain no pairing code or credentials.

The broader isolated integration gate also passed all **16** existing tests across account HTTP flows/surface, additive migration, dashboard mounts, provider/add-on ownership CLI, and retirement CLI. The full library gate is **462 passed, 10 existing ignored**. These fixtures do not validate production deployment or physical-TV behavior.
