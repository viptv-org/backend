#!/usr/bin/env bash
# Isolated generated media only; no production configuration/data/deployment.
set -euo pipefail
image="${1:?Usage: bash scripts/check-torrent-browser-backend.sh IMAGE GATEWAY_ROOT TV_WEB_ROOT CERT_DIR}"
gateway_root=$(realpath "${2:?gateway source checkout required}")
tv_root=$(realpath "${3:?built TV-web checkout required}")
cert_dir=$(realpath "${4:?trusted local certificate directory required}")
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$root/target/qualification"
q=$(mktemp -d "$root/target/qualification/backend-torrent-browser-XXXXXX")
chmod 700 "$q"
export QUALIFICATION_DIR="$q" TV_WEB_ROOT="$tv_root" VIPTV_TEST_BROWSER_ORIGIN=https://viptv.local.test:8444
[[ -f "$tv_root/dist/index.html" && -f "$cert_dir/viptv.local.test.crt" ]]
[[ $(node -p 'Number(process.versions.node.split(".")[0])') -ge 24 ]]
python3 - <<'PYPORT'
import socket
for port in [8444,18444,18445,18446]:
 with socket.socket() as check:
  check.bind(('127.0.0.1',port))
PYPORT
id='' bridge_pid='' backend_pid='' caddy_pid='' 
cleanup() {
 for pid in "$caddy_pid" "$backend_pid" "$bridge_pid"; do [[ -z "$pid" ]] || kill "$pid" 2>/dev/null || true; done
 [[ -z "$id" ]] || docker logs "$id" > "$q/gateway.log" 2>&1 || true
 [[ -z "$id" ]] || docker rm --force --volumes "$id" >/dev/null 2>&1 || true
 if [[ -s "$q/socket-alias" ]]; then python3 -c 'from pathlib import Path;import sys;p=Path(sys.argv[1]);(p/"gateway.sock").unlink(missing_ok=True);p.rmdir()' "$(cat "$q/socket-alias")"; fi
}
trap cleanup EXIT
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="${GATEWAY_TARGET_DIR:-$gateway_root/target}" cargo test --manifest-path "$gateway_root/Cargo.toml" --locked -p playback-gateway --test torrent_browser --no-run --message-format=json > "$q/gateway-compile.json" 2> "$q/gateway-compile.log"
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="${BACKEND_TARGET_DIR:-$root/server/target}" cargo test --manifest-path "$root/server/Cargo.toml" --locked --lib --no-run --message-format=json > "$q/backend-compile.json" 2> "$q/backend-compile.log"
artifact() { python3 -c 'import json,sys;r=[json.loads(l) for l in open(sys.argv[1])];a=[x["executable"] for x in r if x.get("reason")=="compiler-artifact" and x.get("target",{}).get("name")==sys.argv[2] and x.get("executable")];assert len(a)==1;print(a[0])' "$1" "$2"; }
gateway_binary=$(artifact "$q/gateway-compile.json" torrent_browser)
backend_binary=$(artifact "$q/backend-compile.json" viptv_server)
id=$(docker run --detach --pull=never --init --network none --read-only --no-healthcheck --cap-drop ALL --cap-add NET_ADMIN --cap-add CHOWN --cap-add SETUID --cap-add SETGID --cap-add DAC_OVERRIDE --cap-add KILL --security-opt no-new-privileges --memory 1g --memory-swap 1g --pids-limit 256 --cpus 2 --tmpfs /tmp:rw,nosuid,nodev,size=512m,mode=1777 --user 0:0 --env PLAYBACK_TEST_ISOLATED_NETWORK=container --env PLAYBACK_TEST_SERVICE_BINARY=/usr/local/bin/playback-gateway --env PLAYBACK_TEST_FFMPEG=/opt/ffmpeg/bin/ffmpeg --env PLAYBACK_TEST_FFPROBE=/opt/ffmpeg/bin/ffprobe --mount "type=bind,source=$gateway_binary,target=/fixtures/browser-fixture,readonly" --mount "type=bind,source=$q,target=/qualification" --entrypoint /fixtures/browser-fixture "$image" standalone_torrent_browser_uid10001 --ignored --exact --nocapture --test-threads=1)
for _ in {1..200}; do [[ ! -f "$q/fixture.json" ]] || break; sleep .1; done
[[ -f "$q/fixture.json" ]]
setsid node "$root/scripts/torrent-browser-bridge.mjs" > "$q/bridge.log" 2>&1 </dev/null & bridge_pid=$!
for _ in {1..50}; do [[ ! -f "$q/backend.json" ]] || break; sleep .1; done
VIPTV_TORRENT_BROWSER_CONFIG="$q/backend.json" VIPTV_AUTH_ORIGIN="$VIPTV_TEST_BROWSER_ORIGIN" setsid "$backend_binary" gateway::torrent_browser_acceptance::actual_backend_torrent_browser_server --ignored --exact --nocapture --test-threads=1 > "$q/backend.log" 2>&1 </dev/null & backend_pid=$!
for _ in {1..100}; do if curl -sS -o /dev/null http://127.0.0.1:18444/api/health 2>/dev/null; then break; fi; sleep .1; done
socket=$(cat "$q/socket-alias")
cat > "$q/Caddyfile" <<CADDY
{
 admin off
 auto_https disable_redirects
 log default {
  output discard
 }
}
$VIPTV_TEST_BROWSER_ORIGIN {
 tls $cert_dir/viptv.local.test.crt $cert_dir/viptv.local.test.key
 handle /api/* {
  reverse_proxy 127.0.0.1:18444
 }
 handle_path /gateway/* {
  reverse_proxy unix/$socket/gateway.sock
 }
 handle /tv/* {
  uri strip_prefix /tv
  root * $tv_root/dist
  try_files {path} /index.html
  file_server
 }
}
CADDY
setsid caddy run --config "$q/Caddyfile" > "$q/caddy.log" 2>&1 </dev/null & caddy_pid=$!
for _ in {1..50}; do if curl -sS -o /dev/null "$VIPTV_TEST_BROWSER_ORIGIN/tv/" 2>/dev/null; then break; fi; sleep .1; done
[[ $(curl -sS -o /dev/null -w '%{http_code} %{ssl_verify_result}' "$VIPTV_TEST_BROWSER_ORIGIN/tv/") == '200 0' ]]
timeout --signal=TERM --kill-after=5s 180s node "$root/scripts/torrent-browser-check.mjs" > "$q/browser.log" 2>&1
node --input-type=module - <<'JS'
import http from 'node:http';import{readFileSync,writeFileSync}from'node:fs';
const q=process.env.QUALIFICATION_DIR+'/';const c=JSON.parse(readFileSync(q+'fixture.json'));
const call=()=>new Promise((resolve,reject)=>{const r=http.request({host:'127.0.0.1',port:18445,path:'/v1/capabilities',headers:{Authorization:'Bearer '+c.key}},s=>{let b='';s.on('data',x=>b+=x);s.on('end',()=>resolve(JSON.parse(b)));});r.on('error',reject);r.end();});
for(let i=0;i<400;i++){const c=await call();if(c.available.inputs===2&&c.available.outputs===2&&c.available.viewers===4){writeFileSync(q+'capacities.json',JSON.stringify(c.available));writeFileSync(q+'stop','actual backend browser passed');break;}if(i===399)throw Error('gateway admission reclamation deadline');await new Promise(r=>setTimeout(r,100));}
JS
result=$(timeout 15s docker wait "$id")
docker logs "$id" > "$q/gateway.log" 2>&1
[[ "$result" == 0 ]]
cat "$q/browser.log" "$q/gateway.log"
printf 'Actual backend/gateway browser proof passed. Private artifacts: %s\n' "$q"
