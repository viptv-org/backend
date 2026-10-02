# Real backend and gateway browser torrent qualification

The reusable `scripts/check-torrent-browser-backend.sh` runs the real backend
App router with ephemeral account/profile/session storage, encrypted addon and
gateway registration, actual gateway capability/scope checks and opaque Source
approval. Its only network exceptions are existing cfg(test) addon-loopback and
gateway-control fixture seams; they are unavailable in production. Device-token
provisioning is synthetic setup, not pairing/login acceptance. The catalog,
metadata and `.torrent` producer are generated external fixtures; account,
catalog/discovery/playback APIs and all media requests are unmocked.

The independently executed case uses backend server source `700ed351` plus the
committed test fixture, TV-web `29e1c3a3ec5b017dbe7ddca1a78c857dcee98d49`, Core
`f66c87e13a2c93b6dad3234da9694c57f8530b0a`, video
`0eb875b52755c4c8906e6b4b7525ac3cfa735a2a`, and gateway runtime image
`sha256:3ed14fbbf179d9d4a326b6e798118ad62c209f01a7fdf3d0524233a52e367825`.
The gateway driver's source is `02ef7dcea9e7fc52d78aa767a1d043f70a311f08`.
Later frontend package pins do not retroactively extend this media evidence.

The complete reusable runner passes in **51.08 seconds**. Trusted local HTTPS
reports `200 0` without `-k`; Chromium keeps `ignoreHTTPSErrors: false`. The real
TV-web app selects its actual backend-approved Source, decodes gateway HLS with
WebCodecs and requests a three-second replacement through the timeline. Decoded
RGB MSE is **24.42494** against the source at three seconds, compared with
**3814.37458** for the paused zero-origin browser frame. Normal renewal and both
old/final releases traverse actual backend and gateway APIs. Public API replies
are checked for absence of the private source URL and integration secret.

All two input/two output/four viewer slots are reclaimed. The UID10001 gateway
with zero effective capabilities/NoNewPrivs observes an active cache payload,
then its unchanged prompt service reaper removes that payload and stops the
peer while readiness stays healthy. The bounded deterministic peer delivers
**4,397,593 bytes over 21,785 milliseconds**; owned service shutdown succeeds.
Backend source checks pass 241 library tests, with three explicit ignored
fixtures, strict library/tests Clippy, formatting and shell/Node syntax.

```sh
bash scripts/check-torrent-browser-backend.sh IMAGE GATEWAY_ROOT TV_WEB_ROOT CERT_DIR
```

Use a checked, built TV-web checkout, Node24+, trusted system/NSS local
certificates, Caddy, Docker and the checked Rust toolchain. Coordinate exclusive
ports 8444/18444/18445/18446 first. Optional `GATEWAY_TARGET_DIR` and
`BACKEND_TARGET_DIR` reuse dependency caches without copying runtime data. All
media, temporary credentials/configuration and results stay in private ignored
`target/qualification/backend-torrent-browser-*` directories. Caddy output is
discarded; cleanup targets only the runner's own processes/container/socket.

The gateway driver uses privileged setup only in a guarded network-none
namespace; that driver is separate from the production all-capabilities-dropped
container envelope. The backend-to-gateway control hop intentionally uses its
existing local HTTP cfg(test) override. Production HTTPS/DNS egress behavior,
standalone backend startup configuration and real pairing are separate gates.
This case covers approved HTTP metainfo inputs; infoHash/magnet metadata
bootstrap, native desktop/Android Media3 torrent-output playback, managed
alternate tracks, physical hardware and larger/repeated peer workloads remain
separate acceptance. It authorizes no deployment or production data access.

`TORRENT_BROWSER_SERVE_ONLY=true` keeps the same real API/media stack available
for native acceptance, bounded to ten minutes. Write `native-complete` inside its
printed private artifact directory only after explicit native lease release;
the runner then verifies gateway admission/cache/peer cleanup. A gateway driver
with `PLAYBACK_TEST_DUAL_AUDIO=true` supplies two mapped silent AAC inputs tagged
eng/spa for managed track selection. That option is fixture generation, not
native playback acceptance. Emulator ingress may translate delivery origins
while retaining real API/lease/source/media state; never weaken the production
private-endpoint validator to accept an emulator literal address.
