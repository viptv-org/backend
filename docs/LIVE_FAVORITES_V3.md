# Live channel favorites

Channel stars are profile-owned live preferences, separate from VOD favorites, Simkl lists and watch history.

- `GET /api/v3/profiles/:profile/live/favorites` returns `{ "channel_ids": ["host-issued-opaque-id"] }`.
- `PUT /api/v3/profiles/:profile/live/:channel/favorite` saves the current host-issued channel.
- `DELETE` on that path removes the selected profile's saved preference, including when that channel/connection is temporarily absent.

Mutations return the same current snapshot. Repeated save/remove requests for an available channel are idempotent. Unknown/foreign/disabled channels cannot be saved. Reads exclude removed, disabled or unassigned connections and channels missing from the current host registry; stored preferences are retained so they can be explicitly removed. Native M3U/Xtream rows also bind the connection revision, so an old preference does not authorize a reconfigured input.

The selected profile, account ownership, Live capability assignment, parent restrictions, configuration revision and browser-cookie CSRF rules remain authoritative. No new provider capability or upstream favorite API is required. Responses contain IDs only, with a 10,000-per-profile limit and deterministic ordering; reaching capacity does not delete prior rows.

Favorites reads do not contact providers. For M3U/Xtream, load channels before reading favorites after a cold server start or configuration change. Registry TTL alone does not delete preferences. Actual playback still performs its existing current-source authorization.

Five isolated server tests cover ownership/profile boundaries, idempotence and VOD separation, disabled/unassigned/absent channels, current native registry/revisions, cookie CSRF and kids restrictions, and capacity preservation. These do not qualify physical-TV input or real provider availability.

Full backend library verification after integrating this API and the actual profile-age projection: **462 passed, zero failures, ten pre-existing ignores** (`live-runtime,vsources-runtime`, Cargo jobs1, test threads2; 66.63 seconds). No running TV/production services or provider accounts were changed.
