#!/bin/sh
set -eu
FFMPEG=${VIPTV_TEST_FFMPEG:-ffmpeg}
OUTPUT=${1:-artifacts/long-fixture-7200s.mp4}
DURATION=${VIPTV_LONG_FIXTURE_SECONDS:-7200}
[ ! -e "$OUTPUT" ] || { echo "Refusing to overwrite $OUTPUT" >&2; exit 1; }
mkdir -p "$(dirname "$OUTPUT")"
"$FFMPEG" -hide_banner -loglevel error -nostdin -y \
  -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -t "$DURATION" -map 0:v:0 -map 1:a:0 \
  -c:v libx264 -preset ultrafast -crf 30 -pix_fmt yuv420p \
  -r 30 -g 60 -keyint_min 60 -sc_threshold 0 -flags +cgop \
  -c:a aac -b:a 96k -ac 2 -ar 48000 -metadata:s:a:0 language=eng \
  -movflags +faststart "$OUTPUT"
"$FFMPEG" -v error -xerror -i "$OUTPUT" -map 0:v:0 -map 0:a:0 -f null -
printf 'Generated %s seconds: %s\n' "$DURATION" "$OUTPUT"
