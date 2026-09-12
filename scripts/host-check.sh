#!/usr/bin/env bash
# Read-only Linux Docker host preflight. Never starts/stops containers or prints secrets.
set -u
ROOT="${VIPTV_PROJECT_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}"
failed=0
pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failed=1; }
warn() { printf 'WARN %s\n' "$1"; }

if [[ ! -f "$ROOT/compose.yaml" || ! -f "$ROOT/Dockerfile" ]]; then
  fail "Project directory must contain compose.yaml and Dockerfile."
  exit 1
fi
pass "Deployment files exist."

# Do not accidentally load a different project's .env from the caller's directory.
ENV_FILE=/dev/null
if [[ -f "$ROOT/.env" ]]; then
  ENV_FILE="$ROOT/.env"
  if [[ -L "$ENV_FILE" ]]; then
    fail "The .env file is a symlink; review the secret target before deploying."
  elif mode="$(stat -c '%a' -- "$ENV_FILE" 2>/dev/null)" && [[ "$mode" =~ ^[0-7]{3,4}$ ]]; then
    if (( (8#$mode & 077) != 0 )); then
      fail ".env is readable or writable by group/others; restrict it to the owner (for example chmod 600)."
    else
      pass ".env has owner-only permissions."
    fi
  else
    fail "Could not verify .env permissions on this Linux host."
  fi
else
  warn "No project .env exists; Compose will require deployment settings from the process environment."
fi

# Read only the non-secret origin setting; never source arbitrary .env shell content.
auth_origin="${VIPTV_AUTH_ORIGIN:-}"
if [[ -z "$auth_origin" && "$ENV_FILE" != /dev/null ]]; then
  while IFS='=' read -r name value; do
    if [[ "${name//[[:space:]]/}" == "VIPTV_AUTH_ORIGIN" ]]; then
      auth_origin="${value%$'\r'}"
      auth_origin="${auth_origin#\"}"; auth_origin="${auth_origin%\"}"
      auth_origin="${auth_origin#\'}"; auth_origin="${auth_origin%\'}"
      break
    fi
  done < "$ENV_FILE"
fi
if [[ ! "$auth_origin" =~ ^https://[^/?#]+$ || "$auth_origin" == *"@"* ]]; then
  fail "VIPTV_AUTH_ORIGIN must be one exact HTTPS origin without credentials, path, query, fragment, or trailing slash."
else
  pass "Exact HTTPS authentication origin is configured."
fi

if (( failed )); then
  printf '\nPreflight stopped before reading unsafe secret configuration. No files were changed.\n'
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  fail "Docker CLI is unavailable. Run this check on the actual Docker Engine host."
  exit 1
fi
if docker info --format '{{.ServerVersion}}' >/dev/null 2>&1; then
  pass "Docker daemon is reachable using the current Docker context."
else
  fail "Docker daemon is unreachable or this account lacks access; review the host/context privately."
fi

compose_command=()
if docker compose version >/dev/null 2>&1; then
  compose_command=(docker compose)
elif command -v docker-compose >/dev/null 2>&1 && docker-compose version >/dev/null 2>&1; then
  compose_command=(docker-compose)
  warn "Using standalone Compose; Docker recommends the Compose plugin for normal deployments."
else
  fail "Docker Compose is unavailable. Install the Compose plugin on the host."
fi
if (( ${#compose_command[@]} )); then
  # --quiet validates interpolation without printing rendered deployment settings.
  if "${compose_command[@]}" --project-directory "$ROOT" --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" config --quiet >/dev/null 2>&1; then
    pass "Compose configuration and required environment values validate."
  else
    fail "Compose configuration is invalid or required environment values are missing; inspect config --quiet privately."
  fi
fi

for directory in server dashboard; do
  if [[ -d "$ROOT/$directory" ]]; then pass "$directory build context exists."; else fail "$directory build context is missing."; fi
done
for lockfile in server/Cargo.lock dashboard/package-lock.json; do
  if [[ -f "$ROOT/$lockfile" ]]; then pass "$lockfile exists."; else fail "$lockfile is missing."; fi
done
if (( failed )); then
  printf '\nPreflight failed. No containers or configuration files were changed.\n'
  exit 1
fi
printf '\nPreflight passed. This does NOT prove image build, GPU support, throughput, provider access, or Roku playback.\n'
printf 'Next: build/start using README.md, verify /api/health from the Roku LAN, then configure real sources privately.\n'
