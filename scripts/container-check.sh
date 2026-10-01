#!/usr/bin/env bash
# Compatibility entry for disposable, network-none backend/gateway acceptance.
# Pass an existing LOCAL_GATEWAY_IMAGE; no deployment env/data is loaded.
set -euo pipefail
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
exec bash "$ROOT/scripts/check-gateway-container.sh" "$@"
