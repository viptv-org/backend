# Account device management

`GET /api/account/devices` returns the authenticated account’s identity and all
its unexpired browser and paired-app session families in one response. Each entry
contains a stable family ID, supplied display name, session kind, creation time,
current-browser flag and per-device sign-out availability. No credential, hash,
IP address or hardware identifier is returned. The current browser is listed last.
Account session creation limits each account to 32 active families; no manual
pagination is needed. A sign-in at that limit evicts the account's oldest family
of the same kind first (a browser sign-in prunes a browser, a new pairing prunes
an older paired app), falling back to the oldest family of either kind only when
the account holds none of its own kind, so a sign-in is never refused for the
account's own cap. Expired families never count.

`DELETE /api/account/devices/:id` revokes one owned family and its used-refresh
tombstones. Foreign or missing identities return 404. Per-device removal of the
current browser returns 409; ordinary Sign out remains its single-session action.

`DELETE /api/account/devices` explicitly signs out every owned browser and
paired family, including the caller, and cancels its pending approved pairings.
Both browser cookies expire. Profiles, saved titles, progress and other accounts
are preserved. Revocation and its audit event commit in one transaction.

All routes revalidate the captured session/role/profile lease under the database
lock through the account-management boundary. A TV pairing credential cannot
manage accounts; a session from `POST /auth/device/login` acts as its account's
session and follows the same owner and member rules as a browser, so it reads
this list and, when the account is the owner, signs devices out. Parent
restrictions remain enforced. Cookie mutations require the existing exact-origin
and CSRF protections. Responses are no-store.

Creation timestamps are not last-activity timestamps. The client labels them
“Signed in …” and does not invent operating-system or model information absent
from the host. The supplied `W3Account` board informs the KMP view; new confirmation,
failure, empty and narrow-screen variants remain provisional. No existing running
QA service or real account was changed by implementation/fixture checks.

The combined Live/SDK server gate passed 438 checks with 10 ignored. Four focused
HTTP/SQLite cases cover family visibility/ownership, per-device/all-device
revocation, retained saved titles/progress, cookie CSRF/clearing and parent policy.
Three additional legacy-auth regressions reject approval requests whose captured
browser family was revoked before final dispatch; a live approval still succeeds.
Focused existing auth login/refresh/recovery coverage also passed. These are
synthetic isolated fixtures; neither real account revocation nor QA activation
was performed.
