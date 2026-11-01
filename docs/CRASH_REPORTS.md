# Crash reports

`POST /api/crash-reports` stores one client crash report for the calling account and
returns `201 {id, received_at}`. Any signed-in session may upload, including a paired
TV; a restricted profile is refused like any other unlisted route. The body must be a
JSON object of at most 64 KB (larger is 413, anything else 400). It is stored as an
opaque blob after every string and key is scrubbed: URLs shrink to `scheme://host`
(userinfo, path, query and fragment dropped), `Bearer`/`Basic` values, values of
`token`, `key`, `password`, `auth`-style names and runs of 32 or more token characters
become `<redacted>`. Only `deviceModel`, `appVersion` and `buildCommit` are read, for
the list summary.

Each account keeps its 20 newest reports; older ones are evicted on upload. Reports are
deleted with their account.

`GET /api/crash-reports` lists the caller's reports newest first as
`{reports:[{id, received_at, summary:{device_model, app_version, build_commit}}]}`.
`GET /api/crash-reports/:id` returns `{id, received_at, report}` and
`DELETE /api/crash-reports/:id` removes one. Reading and deleting need an account
session (not a paired TV) outside a restricted profile; another account's id is 404.
