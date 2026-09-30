# Transcoder decision — 2026-09-27

Historical pre-BE-002 backend evidence only. The backend no longer runs FFmpeg,
GPU probes or these benchmark hooks; their source is recoverable from Git history.
The generic `scripts/benchmark-source.py` HTTPS byte-range fixture is retained
after coordination with the gateway owner for possible later diagnostic reuse.
It is not a backend packaging, runtime or deployment entry point. Media settings
and current decoder qualification belong to the independent gateway.

The historical decision kept FFmpeg 5.1.9 as the default. The candidate was Jellyfin-FFmpeg
8.1.2-5, portable archive SHA-256
`1fd859927053c44a4f2dbf67ae8b9ba8d29fb3b8930df0dd57d91aa60589363d`.

The existing Intel UHD 630 and exposed render device were tested in isolated
containers with identical production drivers, HTTPS synthetic input, settings
and output validation. Five runs per workload; both binaries decoded the output
successfully. The temporary HTTPS source was stopped afterwards.

| Workload | Baseline p95 first segment | Candidate p95 | Baseline minimum speed | Candidate minimum speed |
|---|---:|---:|---:|---:|
| Stream copy | 378.2 ms | 214.3 ms | 20.378x | 35.022x |
| Audio conversion | 522.9 ms | 336.6 ms | 12.710x | 16.393x |
| Software video | 481.9 ms | 679.6 ms | 7.059x | 8.006x |
| QSV video | 685.1 ms | 531.0 ms | 4.195x | 8.377x |

Every workload exceeded the 1.25x realtime gate. Software startup regressed
41%, exceeding the allowed 10%; therefore the candidate was not promoted.
QSV improved, but that does not remove the software fallback requirement.
These synthetic startup measurements are not provider/network startup promises.

`scripts/benchmark-source.py` retains the controlled HTTPS fixture. The removed
workload runner and its backend-only `VIPTV_FFMPEG` / `VIPTV_FFPROBE` settings are
recoverable from the pre-retirement revision `d6b8d570326fa98bdb8077e8078f660617071b02`;
they are not current deployment instructions or supported backend settings.

References: [pinned release](https://github.com/jellyfin/jellyfin-ffmpeg/releases/tag/v8.1.2-5),
[Jellyfin acceleration](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/),
[FFmpeg HLS](https://ffmpeg.org/ffmpeg-all.html#hls).
