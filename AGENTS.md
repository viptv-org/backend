# viptv development

Delivery policy (owner approved 2026-09-27): only Android, desktop, Roku and TV-web
build workflows remain, triggered by main pushes and manual dispatch. No PR
gates, automatic releases, image publishing or deployment. Retain local checks.
This supersedes older automation/release-gate instructions below.

Before changing product behavior, read the pinned design revision in DESIGN_REF and https://github.com/viptv-org/design/blob/main/DESIGN.md, then its relevant visual and behavior specifications. Record proposed UX changes in design first; link the approved design commit and acceptance scenarios in the implementation issue. Match tap, hold, Back, focus restoration, and error behavior across platforms.

Specs and tickets live in this repository's GitHub Issues. Search existing issues first; use needs-triage, needs-info, ready-for-agent, ready-for-human, and wontfix. Work from SPEC.md and the issue; validate the affected interface using repository CI commands. Report actual results separately from hardware or deployment checks that were not run.

Keep credentials and user data private. Preserve profile/account IDs, configuration and history. Production migration retains the existing environment and named volume viptv_viptv_data; never use docker compose down -v. Use a new artifact path for each build. App assets are versioned in design/assets; local packaged copies remain pinned and updates require an explicit design change.
