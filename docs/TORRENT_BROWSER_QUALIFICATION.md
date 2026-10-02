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
discarded; raw gateway/browser stderr stays private and only finite safe metrics
are printed. HTTPS binds host loopback explicitly; cleanup targets only the runner's own processes/container/socket.

The gateway driver uses privileged setup only in a guarded network-none
namespace; that driver is separate from the production all-capabilities-dropped
container envelope. The backend-to-gateway control hop intentionally uses its
existing local HTTP cfg(test) override. Production HTTPS/DNS egress behavior,
standalone backend startup configuration and real pairing are separate gates.
The original case covers approved HTTP metainfo inputs. Controlled magnet and
bounded repeated peer cases and Android managed input selection are recorded
below. Native desktop torrent-output playback, audible alternate-language
content, physical hardware and larger or hostile public peer workloads remain
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

## Native selected-input observation

`scripts/observe-native-torrent-map.py PRIVATE_CASE_DIRECTORY 2` reads only the
container whose `/qualification` mount matches that private directory. Within
a bounded observation (120 seconds by default, at most 180), it records real
FFmpeg mappings before playback. Once a native delivery is ready selecting
input index 2, its actual served MPEGTS segment SHA256 must match the same
segment in a recorded FFmpeg output directory mapping `0:2`. The opaque public
viewer id is never treated as an engine output id. Start the observer before
native playback because encoding can finish before the app reports ready. Old output jobs and non-FFmpeg
processes cannot provide this proof. The script changes no service, lease or
media state. Docker access is required.

Raw process arguments may contain source/capability values; they are saved only
as mode0600 `native-ffmpeg-argv.bin` inside the already-private directory and
never printed. `native-audio-map.json` and stdout contain only the numeric
selector, matching observed output process count and a ready-output-match boolean.
The private argv archive is limited to 4MiB and sixteen observed outputs.

The qualified runtime labels tagged English HLS audio `eng` and other selected
input languages `und`; native input index2 may therefore describe Spanish while
the actual encoded HLS reports AAC/und. Identical silent source samples cannot
prove audible language. This observer proves the actual selected input map;
native decode, track-menu selection, Back, seek and release must still be
verified through the app. The helper alone does not claim that acceptance.

The independent fresh Android case executes this observer at source
`31b2dc8551bc682cb9b60a0984b9b979308c31e4`: selector2, one matching observed
output process and served-output digest match pass. The native lane separately
reports immediate Audio selection/Back returning Spanish Current, a managed
seek with decoded burned-in82.920, four lease DELETE200 responses, reclaimed
inputs2/outputs2/viewers4 and full runner exit0 with cache/peer retirement.
Actual delivered audio tags are eng then und. This is selected-input and
managed playback proof, not audible Spanish sample identification. Native
helper/source pins and screenshots remain in the Android qualification record.

## Approved infoHash bootstrap and bounded multi-peer cases

`PLAYBACK_TEST_MAGNET=true` makes the generated addon return infoHash/fileIdx
rather than an HTTP metainfo URL. The real backend approves that opaque Source
and sends magnet input to the unchanged enabled service. Controlled BEP5 DHT
responders discover outbound BEP10/BEP9 metadata peers inside Docker network
none. Container-only bootstrap DNS aliases never change host/production policy.
`PLAYBACK_TEST_SWARM_PEERS=1..4` bounds peers and requires at least two actual
payload contributors when more than one peer is advertised. Optional
`TORRENT_BROWSER_TLS_PORT` and `TORRENT_BROWSER_BASE_PORT` reserve an independent
lane (base, base+1, base+2); default native fixture ports remain unchanged.
The runner copies each compiler executable into its private case before
launching, so later builds cannot replace an active binary.

Real backend/browser results using the same runtime image:

| Case | Advertised/contributing peers | Metadata bytes | DHT queries | Delivered payload bytes / elapsed ms |
| --- | --- | --- | --- | --- |
| Single magnet | 1/1 | 4,990 | 16 | 4,014,795 / 19,996 |
| Multi-peer first case | 3/2 | 9,980 | 18 | 4,014,631 / 25,863 |
| Separate fresh cycle | 3/3 | 9,980 | 14 | 6,832,456 / 34,664 |
| Copied executable fresh cycle | 3/3 | 16,350 | 20 | 5,601,160 / 29,731 |

The final fresh cycle passes in 70.09 seconds with TV-web
`ea1733497e0b8e0911fdc842aa597d2bfda32288`, Coref66 and video
`550ab3503a670080a5c6f5e3ff619446c8fcd9e5`; its driver source is gateway
`fbaac48c4628c918e4788250055d4562fd3b1270`. Earlier magnet cases used the
previous TV29e/Coref66/video0eb pins. All cases exercise decoded three-second
seek, actual renewals/releases, all2/2/4 admission reclamation, prompt idle cache
eviction, peer transport retirement and owned shutdown. Final RGB MSE24.42494
versus zero-origin3814.37458; two renewals and two releases. Byte counts include
repeated block requests. These are bounded fresh-process repetitions; they do
not qualify long-duration public swarms, arbitrary peer implementations or
audible alternate-language content.

Protocol references: [BEP5](https://www.bittorrent.org/beps/bep_0005.html),
[BEP9](https://www.bittorrent.org/beps/bep_0009.html),
[BEP10](https://www.bittorrent.org/beps/bep_0010.html).
