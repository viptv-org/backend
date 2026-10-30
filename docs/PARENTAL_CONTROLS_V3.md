# Parental controls

A household parent PIN gates parental administration. `GET /api/parent/status` returns `{pin_configured,unlocked,restricted,leave_pin_configured}`; `unlocked` is the session grant and `leave_pin_configured` reports the selected profile. `POST /api/parent/unlock {pin}` records a session-bound grant for 120 seconds. A wrong PIN is 403 `parent_pin_invalid`; five failures lock the account's PIN for five minutes (429). `PUT /api/parent/pin {pin?,current_pin?}` sets, rotates or removes the PIN: rotating needs `current_pin`, an empty `pin` removes it only while no kids profile is enabled, and both clear every session grant. PINs are 4–8 ASCII digits.

Enabling a kids profile needs the household PIN configured. Ownership, the parent grant and the profile's policy revision apply as they do to the other `/api/profiles/:id` routes, and every policy change bumps `revision` so a restricted client re-reads content.

## Per-profile policy

`GET|PUT /api/profiles/:id/kids` reads and changes `{enabled,max_age,movie_rating,series_rating,allow_unrated}`.

`max_age` is 0–17 and is the fallback age ceiling. `movie_rating` accepts `G`, `PG`, `PG-13`, `R` or `NC-17`; `series_rating` accepts `TV-Y`, `TV-Y7`, `TV-G`, `TV-PG`, `TV-14` or `TV-MA`. Each label stores the age `kids::metadata` derives for it, so the two kinds carry separate limits: a title is served when its derived age is within the limit for its kind, or within `max_age` when that kind has no label. Labels are matched case-insensitively and stored uppercase; `null` or an omitted field keeps the current label and an empty string clears it. `NC-17` and `TV-MA` derive 18, and titles rated 18+ are never served to a kids profile, so those labels mean "no ceiling below adult".

The response also carries `ratings`, the labels each kind accepts with the ages the server derives, so a client never invents that mapping:

```json
{"ratings":{"movie":[{"label":"G","age":0},{"label":"PG","age":10},{"label":"PG-13","age":13},{"label":"R","age":17},{"label":"NC-17","age":18}],
"series":[{"label":"TV-Y","age":0},{"label":"TV-Y7","age":7},{"label":"TV-G","age":0},{"label":"TV-PG","age":10},{"label":"TV-14","age":14},{"label":"TV-MA","age":18}]}}
```

`allow_unrated` (default false) serves titles this account observed without a usable rating and titles whose rating sources disagree. Without it both stay hidden even when an age ceiling would allow them, which is the default kids behaviour. An episode id whose parent edge is ambiguous — observed under more than one series — stays hidden either way: that is an identity question, not a rating one. The 18+ exclusion and the age ceilings apply to everything `allow_unrated` admits.

## Approvals and the blocked list

`GET|POST /api/profiles/:id/approvals` allows single titles beyond the age ceiling; [typed parent title selection](PARENT_TITLE_APPROVAL_V3.md) defines its contract.

`GET|POST /api/profiles/:id/blocked` is the deny list. `POST {type,id,blocked,name?,poster?}` blocks (`true`) or unblocks (`false`) one movie or series id, keeping the display name and poster the client showed for at most 500 entries per profile; more is 400. `GET` returns `{items:[{id,type,name,poster}]}`, oldest first, with `offset` paging. A blocked id hides that title and every episode of it from catalog, detail, saved library entries, continuation and source starts. A block outranks an approval: approving a blocked title does not serve it.

## Per-profile leave PIN

`GET /api/profiles/:id/pin` returns `{availability:"Available",message:null,configured}`, and `GET /api/profiles` advertises `capabilities.profile_pin.availability`. `PUT /api/profiles/:id/pin {pin?,current_pin?}` sets, rotates or removes the PIN that gates leaving this profile: rotating or removing needs `current_pin`, an empty `pin` removes it. Both need a browser account session, ownership of the profile, and a parent grant when the selected profile is restricted.

`POST /api/auth/profile {profile_id,pin?}` selects a profile. When the selected profile has a leave PIN, leaving it requires it: missing is 403 `leave_pin_required`, wrong is 403 `leave_pin_invalid`, and five wrong attempts lock leaving that profile for five minutes (429). A restricted profile also needs the household parent unlock first, so both gates can apply; a successful switch consumes the session's parent grant. Failed attempts are counted outside the switch transaction so the lockout survives the rejected request.