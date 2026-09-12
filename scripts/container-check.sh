#!/usr/bin/env bash
# Actual Docker acceptance; creates only uniquely named disposable test resources.
# Run on the Docker host after building viptv:local, or select a candidate with
# VIPTV_BASE_IMAGE to validate it without retagging/restarting production.
set -euo pipefail
ROOT="${VIPTV_PROJECT_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}"
project="viptv-check-$(date +%s)-$$"
export VIPTV_INTEGRATION_IMAGE="viptv:validation-${project}"
# Host is explicit: never accidentally test against another saved Docker context.
docker_command=(docker --host "${VIPTV_TEST_DOCKER_HOST:-unix:///var/run/docker.sock}")
compose=("${docker_command[@]}" compose --ansi never --progress plain
  --env-file /dev/null --project-directory "$ROOT"
  -f "$ROOT/compose.validation.yaml" -p "$project")
if [[ -n "${VIPTV_TEST_QSV_DEVICE:-}" ]]; then
  export VIPTV_TEST_RENDER_GID="${VIPTV_TEST_RENDER_GID:-$(stat -c %g "$VIPTV_TEST_QSV_DEVICE")}"
  compose+=(-f "$ROOT/compose.validation.qsv.yaml")
fi
cleanup() {
  status=$?
  trap - EXIT
  cleanup_failed=0
  "${compose[@]}" down --volumes --remove-orphans || cleanup_failed=1
  if "${docker_command[@]}" image inspect "$VIPTV_INTEGRATION_IMAGE" >/dev/null 2>&1; then
    "${docker_command[@]}" image rm "$VIPTV_INTEGRATION_IMAGE" >/dev/null || cleanup_failed=1
  fi
  if (( cleanup_failed )); then
    printf 'FAIL cleanup for %s; inspect this test project only.\n' "$project" >&2
    if (( status == 0 )); then status=1; fi
  else
    printf 'PASS disposable project cleanup: %s\n' "$project"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
printf 'Testing selected image using disposable project: %s\n' "$project"
"${docker_command[@]}" image inspect "${VIPTV_BASE_IMAGE:-viptv:local}" --format 'Selected image: {{.Id}}'
"${compose[@]}" config --quiet
# Source image and deployment data are never removed. No real .env is loaded.
timeout --signal=TERM --kill-after=15s 600s "${compose[@]}" build checks
timeout --signal=TERM --kill-after=15s 360s "${compose[@]}" up --no-build --abort-on-container-exit --exit-code-from checks
if [[ -n "${VIPTV_TEST_QSV_DEVICE:-}" ]]; then
  gpu_log=$("${compose[@]}" logs --no-color server)
  if [[ "$gpu_log" != *h264_qsv* ]]; then
    printf 'FAIL: no successful Quick Sync playback in acceptance logs.\n' >&2
    exit 1
  fi
  printf 'PASS: actual Quick Sync playback recorded in acceptance server.\n'
  python3 scripts/qsv-frame-check.py
fi
printf 'PASS container acceptance command completed. Verify smoke PASS logs above.\n' 
