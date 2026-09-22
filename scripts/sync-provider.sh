#!/usr/bin/env bash
# Vendored shared provider crate.
#
# The canonical source of `viptv-provider` lives in viptv-org/core
# (core/crates/viptv-provider) because both the fat shells (core) and this
# server consume it, and core must build standalone. The backend Docker image
# builds from this repository alone, so the crate is vendored into
# server/provider and kept in sync by this script.
#
# Usage:
#   scripts/sync-provider.sh sync [core-checkout]   # default ../core
#   scripts/sync-provider.sh verify
#
# `sync` requires a clean core checkout; it copies Cargo.toml and src/ over
# server/provider and records the source commit plus file hashes in
# server/provider-sync.json. `verify` recomputes the hashes and fails on any
# drift, so CI blocks edits made directly to the vendored copy.
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-verify}"
source_crate="crates/viptv-provider"
vendored="server/provider"
manifest="server/provider-sync.json"

hash_tree() {
  # Stable file hash listing for the vendored tree (source files only).
  ( cd "$vendored" && find . -type f -not -path './target/*' | LC_ALL=C sort | \
    while read -r file; do
      printf '%s  %s\n' "$(sha256sum "$file" | cut -d' ' -f1)" "${file#./}"
    done )
}

case "$mode" in
  sync)
    core="${2:-../core}"
    revision="$(git -C "$core" rev-parse HEAD)"
    dirty="$(git -C "$core" status --porcelain -- "$source_crate")"
    if [[ -n "$dirty" ]]; then
      echo "Commit the core provider crate before adoption" >&2
      exit 1
    fi
    rm -rf "$vendored"
    mkdir -p "$vendored"
    cp -r "$core/$source_crate/Cargo.toml" "$core/$source_crate/src" "$vendored/"
    {
      printf '{\n  "source": "viptv-org/core",\n  "revision": "%s",\n  "files": {\n' "$revision"
      hash_tree | awk 'NR>1{printf ",\n"} NR{printf "    \"%s\": \"%s\"", $2, $1} END{printf "\n  }\n}\n"}'
    } > "$manifest"
    echo "Vendored viptv-provider from core $revision"
    ;;
  verify)
    [[ -f "$manifest" ]] || { echo "Missing $manifest; run sync" >&2; exit 1; }
    python3 - "$manifest" <(hash_tree) << 'EOF'
import json, sys
manifest = json.load(open(sys.argv[1]))
actual = {}
for line in open(sys.argv[2]).read().splitlines():
    digest, _, name = line.partition("  ")
    actual[name] = digest
expected = manifest["files"]
missing = sorted(set(expected) - set(actual))
extra = sorted(set(actual) - set(expected))
changed = sorted(name for name in set(expected) & set(actual) if expected[name] != actual[name])
if missing or extra or changed:
    for name in missing:
        print(f"missing: {name}")
    for name in extra:
        print(f"unexpected: {name}")
    for name in changed:
        print(f"changed: {name}")
    print("Vendored provider crate out of sync; edit core/crates/viptv-provider and re-run sync")
    sys.exit(1)
print(f"Vendored provider crate matches core {manifest['revision']}")
EOF
    ;;
  *)
    echo "Use sync [core-checkout] or verify" >&2
    exit 2
    ;;
esac
