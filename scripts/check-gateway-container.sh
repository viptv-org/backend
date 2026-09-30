#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-only
# Real media without public network, published ports, host data or credentials.
set -euo pipefail

gateway_image="${1:?Usage: bash scripts/check-gateway-container.sh LOCAL_GATEWAY_IMAGE}"
backend_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
backend_revision=$(git -C "$backend_root" rev-parse HEAD)
gateway_image_id=$(docker image inspect "$gateway_image" --format '{{.Id}}')
if [[ ! "$gateway_image_id" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo 'The gateway must be an existing local image with an exact content ID.' >&2
  exit 1
fi

# Archive tracked source only: never send ignored developer env/data to Docker.
git -C "$backend_root" cat-file -e "$backend_revision:scripts/gateway-fixture.Dockerfile"
fixture_parent="viptv-qualification-parent:${gateway_image_id#sha256:}"
docker image tag "$gateway_image_id" "$fixture_parent"
fixture_image="viptv-backend-gateway-fixture:${backend_revision:0:12}-${gateway_image_id:7:12}"
printf 'Backend revision: %s\nGateway image: %s\n' "$backend_revision" "$gateway_image_id"
git -C "$backend_root" archive --format=tar "$backend_revision" server scripts/gateway-fixture.Dockerfile \
  | docker build --pull=false --progress=plain \
      --build-arg "GATEWAY_IMAGE=$fixture_parent" \
      --build-arg "GATEWAY_REVISION_IMAGE=$gateway_image_id" \
      --build-arg "BACKEND_REVISION=$backend_revision" \
      --file scripts/gateway-fixture.Dockerfile --tag "$fixture_image" -

container_id=''
cleanup() {
  if [[ -n "$container_id" ]]; then
    # Exact ID returned by this run only; no existing container or volume targets.
    docker container rm --force --volumes "$container_id" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Root/NET_ADMIN are test-only, required to add the fixture address to this
# private loopback. The test independently asserts that no other NIC exists.
# This does not qualify the production gateway's non-root execution envelope.
container_id=$(docker run --detach --pull=never --init --user 0:0 \
  --network none --read-only --no-healthcheck \
  --cap-drop ALL --cap-add NET_ADMIN --security-opt no-new-privileges \
  --tmpfs /tmp:rw,nosuid,nodev,size=512m \
  --memory 1g --memory-swap 1g --pids-limit 256 --cpus 2 \
  "$fixture_image")
if [[ ! "$container_id" =~ ^[0-9a-f]{64}$ ]]; then
  echo 'Docker did not return a valid new container ID.' >&2
  exit 1
fi
if ! result=$(timeout --signal=TERM --kill-after=5s 120s docker wait "$container_id"); then
  docker logs "$container_id" >&2 || true
  echo 'Isolated gateway fixture did not finish within its bound.' >&2
  exit 1
fi
docker logs "$container_id"
if [[ "$result" != 0 ]]; then
  printf 'Isolated gateway fixture failed with status %s.\n' "$result" >&2
  exit 1
fi
echo 'Isolated real-media lifecycle passed; disposable container/data removed on exit.'
