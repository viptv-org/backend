#!/usr/bin/env bash
# Real browser/API acceptance on synthetic tmpfs data, never a production origin.
set -euo pipefail
umask 077
docker_command=(docker)
if [[ "${1:-}" == --sudo ]]; then docker_command=(sudo docker); shift; fi
if [[ "$#" != 5 ]]; then
  echo 'Usage: check-browser-runtime.sh [--sudo] IMAGE IMAGE_ID REVISION CERT KEY' >&2
  exit 2
fi
image="$1"; expected_image="$2"; revision="$3"
certificate=$(realpath -- "$4"); private_key=$(realpath -- "$5")
[[ "$expected_image" =~ ^sha256:[0-9a-f]{64}$ && "$revision" =~ ^[0-9a-f]{40}$ ]]
[[ "$certificate" =~ ^/[A-Za-z0-9._/-]+$ && "$private_key" =~ ^/[A-Za-z0-9._/-]+$ ]]
[[ -r "$certificate" && -r "$private_key" ]]
for executable in caddy playwright-cli curl timeout ss node; do command -v "$executable" >/dev/null; done
if [[ -n "$(ss -ltnH '( sport = :18445 )')" ]]; then
  echo 'Fixture port 18445 is occupied; do not touch an existing service.' >&2
  exit 1
fi
actual_image=$("${docker_command[@]}" image inspect "$image" --format '{{.Id}}')
actual_revision=$("${docker_command[@]}" image inspect "$image" --format '{{index .Config.Labels "tech.syek.viptv.backend-revision"}}')
[[ "$actual_image" == "$expected_image" && "$actual_revision" == "$revision" ]]
backend_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
evidence=$(mktemp -d /tmp/viptv-browser-image.XXXXXX)
session="viptv-browser-$(basename -- "$evidence")"
container_id=''; caddy_pid=''; network_id=''
browser_attempted=0; cleanup_done=0; cleanup_status=0
# CLI-generated snapshots belong to this mode-0700 evidence directory too.
cd -- "$evidence"
export NO_UPDATE_NOTIFIER=1
cleanup() {
  if (( cleanup_done )); then return "$cleanup_status"; fi
  cleanup_done=1
  local remaining probe_status
  : > "$evidence/cleanup-summary.txt"
  if (( browser_attempted )); then
    if ! timeout --signal=TERM --kill-after=2s 10s playwright-cli -s="$session" close > "$evidence/browser-close.log" 2>&1; then
      cleanup_status=1
      echo 'browser close failed' >> "$evidence/cleanup-summary.txt"
    fi
    # Scope this probe to our unique session; never inspect/close global browsers.
    if timeout --signal=TERM --kill-after=2s 10s playwright-cli -s="$session" tab-list > "$evidence/browser-absence.log" 2>&1; then
      cleanup_status=1
      echo 'browser still open' >> "$evidence/cleanup-summary.txt"
    else
      probe_status=$?
      if [[ "$probe_status" == 1 ]] && grep -Fq "The browser '$session' is not open, please run open first" "$evidence/browser-absence.log"; then
        echo 'browser absent' >> "$evidence/cleanup-summary.txt"
      else
        cleanup_status=1
        echo 'browser absence not verified' >> "$evidence/cleanup-summary.txt"
      fi
    fi
  fi
  if [[ -n "$caddy_pid" ]]; then
    kill -TERM "$caddy_pid" 2>/dev/null || true
    for attempt in {1..20}; do kill -0 "$caddy_pid" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$caddy_pid" 2>/dev/null; then
      kill -KILL "$caddy_pid" 2>/dev/null || true
      for attempt in {1..20}; do kill -0 "$caddy_pid" 2>/dev/null || break; sleep 0.1; done
    fi
    if kill -0 "$caddy_pid" 2>/dev/null; then
      cleanup_status=1
      echo 'proxy still alive' >> "$evidence/cleanup-summary.txt"
    else
      wait "$caddy_pid" 2>/dev/null || true
      echo 'proxy absent' >> "$evidence/cleanup-summary.txt"
    fi
  fi
  if [[ "$container_id" =~ ^[0-9a-f]{64}$ ]]; then
    timeout --signal=TERM --kill-after=2s 10s "${docker_command[@]}" logs "$container_id" > "$evidence/backend.log" 2>&1 || true
    if ! timeout --signal=TERM --kill-after=2s 20s "${docker_command[@]}" container rm --force "$container_id" > "$evidence/container-cleanup.log" 2>&1; then
      cleanup_status=1
      echo 'container removal failed' >> "$evidence/cleanup-summary.txt"
    fi
    if remaining=$(timeout --signal=TERM --kill-after=2s 10s "${docker_command[@]}" container ls --all --no-trunc --filter "id=$container_id" --format '{{.ID}}' 2> "$evidence/container-absence-error.log") && [[ -z "$remaining" ]]; then
      echo 'container absent' >> "$evidence/cleanup-summary.txt"
    else
      cleanup_status=1
      echo 'container absence not verified' >> "$evidence/cleanup-summary.txt"
    fi
  fi
  if [[ "$network_id" =~ ^[0-9a-f]{64}$ ]]; then
    if ! timeout --signal=TERM --kill-after=2s 20s "${docker_command[@]}" network rm "$network_id" > "$evidence/network-cleanup.log" 2>&1; then
      cleanup_status=1
      echo 'network removal failed' >> "$evidence/cleanup-summary.txt"
    fi
    if remaining=$(timeout --signal=TERM --kill-after=2s 10s "${docker_command[@]}" network ls --no-trunc --filter "id=$network_id" --format '{{.ID}}' 2> "$evidence/network-absence-error.log") && [[ -z "$remaining" ]]; then
      echo 'network absent' >> "$evidence/cleanup-summary.txt"
    else
      cleanup_status=1
      echo 'network absence not verified' >> "$evidence/cleanup-summary.txt"
    fi
  fi
  if remaining=$(ss -ltnH '( sport = :18445 )') && [[ -z "$remaining" ]]; then
    echo 'fixture listeners absent' >> "$evidence/cleanup-summary.txt"
  else
    cleanup_status=1
    echo 'fixture listener absence not verified' >> "$evidence/cleanup-summary.txt"
  fi
  if (( cleanup_status )); then echo "Fixture cleanup failed; inspect private evidence $evidence" >&2; fi
  return "$cleanup_status"
}
on_exit() {
  local original_status=$?
  trap - EXIT
  if ! cleanup; then original_status=1; fi
  exit "$original_status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'image=%s\nrevision=%s\n' "$expected_image" "$revision" > "$evidence/source.txt"
# An owned internal bridge is accessible from this host, without publishing any
# container port. Only our loopback-bound TLS proxy exposes the fixture. No
# private env, host data or volume is loaded. Keyring is intentionally absent.
network_id=$("${docker_command[@]}" network create --driver bridge --internal "viptv-$(basename -- "$evidence")")
[[ "$network_id" =~ ^[0-9a-f]{64}$ ]]
[[ "$("${docker_command[@]}" network inspect "$network_id" --format '{{.Internal}} {{.Driver}} {{.Id}}')" == "true bridge $network_id" ]]
container_id=$("${docker_command[@]}" run --detach --pull=never --read-only \
  --network "$network_id" \
  --cap-drop ALL --security-opt no-new-privileges --pids-limit 128 \
  --memory 512m --memory-swap 512m --cpus 2 \
  --tmpfs /data:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700,size=32m \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=16m \
  --env VIPTV_AUTH_ORIGIN=https://viptv.local.test:18445 \
  --env VIPTV_DATABASE=/data/synthetic.sqlite "$expected_image")
[[ "$container_id" =~ ^[0-9a-f]{64}$ ]]
"${docker_command[@]}" container inspect "$container_id" --format '{{json .NetworkSettings.Networks}}' > "$evidence/network-address.json"
backend_address=$(node -e '
  const fs=require("node:fs"),assert=require("node:assert/strict");
  const entries=Object.values(JSON.parse(fs.readFileSync(process.argv[1],"utf8")));
  assert.equal(entries.length,1,"Fixture container must have only its owned network");
  assert.equal(entries[0].NetworkID,process.argv[2],"Unexpected container network");
  const ip=entries[0].IPAddress;
  assert.equal(typeof ip,"string");
  assert(/^(0|[1-9][0-9]{0,2})(\.(0|[1-9][0-9]{0,2})){3}$/.test(ip),"Invalid fixture IPv4");
  const octets=ip.split(".").map(Number);
  assert(octets.every(n=>n<=255)&&octets[0]>0&&octets[0]<224&&octets[0]!==127,"Invalid fixture IPv4");
  process.stdout.write(ip);
' "$evidence/network-address.json" "$network_id")
printf '{\n admin off\n auto_https disable_redirects\n}\nhttps://viptv.local.test:18445 {\n bind 127.0.0.1\n tls "%s" "%s"\n reverse_proxy %s:8080\n}\n' \
  "$certificate" "$private_key" "$backend_address" > "$evidence/Caddyfile"
mkdir "$evidence/caddy-data" "$evidence/caddy-config"
XDG_DATA_HOME="$evidence/caddy-data" XDG_CONFIG_HOME="$evidence/caddy-config" \
  caddy run --config "$evidence/Caddyfile" --adapter caddyfile > "$evidence/caddy.log" 2>&1 &
caddy_pid=$!
for attempt in {1..40}; do
  kill -0 "$caddy_pid"
  if curl --fail --silent --show-error --max-time 2 \
    --resolve viptv.local.test:18445:127.0.0.1 https://viptv.local.test:18445/api/health > "$evidence/health.json" 2> "$evidence/health-error.log"; then break; fi
  sleep 0.25
done
curl --fail --silent --show-error --max-time 5 -o /dev/null -w '%{http_code} %{ssl_verify_result}\n' \
  --resolve viptv.local.test:18445:127.0.0.1 https://viptv.local.test:18445/ > "$evidence/tls.txt"
[[ "$(< "$evidence/tls.txt")" == '200 0' ]]
kill -0 "$caddy_pid"
printf '%s\n' '{"browser":{"browserName":"chromium","isolated":true,"launchOptions":{"args":["--host-resolver-rules=MAP viptv.local.test 127.0.0.1"]},"contextOptions":{"ignoreHTTPSErrors":false}}}' > "$evidence/browser.json"
browser_attempted=1
timeout --signal=TERM --kill-after=5s 60s playwright-cli -s="$session" open \
  https://viptv.local.test:18445/ --browser=chrome --config="$evidence/browser.json" > "$evidence/browser-open.log" 2>&1
for viewport in '1440 900' '390 844'; do
  read -r width height <<< "$viewport"
  timeout --signal=TERM --kill-after=2s 15s playwright-cli -s="$session" resize "$width" "$height" > "$evidence/resize-$width.log" 2>&1
  timeout --signal=TERM --kill-after=5s 180s playwright-cli -s="$session" run-code \
    --filename="$backend_root/scripts/check-browser-runtime.js" > "$evidence/check-$width.log" 2>&1
  # A zero-test or failed CLI invocation cannot qualify this fixture.
  grep -Eq '^\{"passed":8,"checks":' "$evidence/check-$width.log"
done
cleanup
trap - EXIT
echo "PASS: eight real-browser groups at both viewports; private evidence $evidence"
echo 'Verified owned browser/proxy/container/network/listener removal; no production/media qualification.'
