#!/usr/bin/env python3
"""Import/check the server's immutable viptv-engine Cargo.toml + src snapshot.

Usage: sync-engine.py sync ../core FULL_COMMIT [--repair-committed-manifest]
       sync-engine.py check [../core]
Only the managed production source tree is replaced. Existing fixture files
and Cargo.lock outside that tree are not runtime provenance inputs.
"""
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "server/shared/engine"
PACKAGE = "crates/viptv-engine"

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def inventory(path):
    files = [path / "Cargo.toml", *sorted((path / "src").rglob("*"))]
    return {str(file.relative_to(path)): digest(file) for file in files if file.is_file()}

def verify_local(repair_committed=False):
    manifest = json.loads((DEST / "SOURCE.json").read_text())
    if inventory(DEST) != manifest["files"]:
        changed = subprocess.check_output(["git","-C",str(ROOT),"status","--porcelain","--",
            "server/shared/engine/Cargo.toml","server/shared/engine/src","server/shared/engine/SOURCE.json"],text=True)
        if not repair_committed or changed:
            raise SystemExit("Engine snapshot differs from SOURCE.json; refusing to overwrite unreviewed edits")
        print("Repairing a reviewed, already-committed manifest mismatch; managed files match Git HEAD.")
    return manifest

def export(source, revision, stage):
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise SystemExit("An immutable full commit is required")
    archive = subprocess.check_output(["git", "-C", str(source), "archive", revision,
                                       f"{PACKAGE}/Cargo.toml", f"{PACKAGE}/src"])
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        # Copy only regular Git files/directories inside the staging tree.
        # This also supports Python versions predating tar's data filter.
        base = stage.resolve()
        for member in tar.getmembers():
            target = (stage / member.name).resolve()
            if not target.is_relative_to(base) or not (member.isdir() or member.isfile()):
                raise SystemExit("Unexpected engine archive member")
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with tar.extractfile(member) as source_file:
                    target.write_bytes(source_file.read())
    package = stage / PACKAGE
    cargo = package / "Cargo.toml"
    text = cargo.read_text()
    before = 'viptv-provider = { path = "../viptv-provider",'
    if text.count(before) != 1:
        raise SystemExit("Unexpected provider dependency; review the manifest transform")
    cargo.write_text(text.replace(before, 'viptv-provider = { path = "../../provider",'))
    return package, {"repository":"https://github.com/viptv-org/core", "revision":revision,
                     "path":PACKAGE, "manifest_transform":"viptv-provider path ../../provider",
                     "archive_sha256":hashlib.sha256(archive).hexdigest(), "files":inventory(package)}

def main():
    args = sys.argv[1:]
    repair_committed = "--repair-committed-manifest" in args
    args = [arg for arg in args if arg != "--repair-committed-manifest"]
    if not args or args[0] not in {"sync", "check"}:
        raise SystemExit(__doc__)
    if repair_committed and args[0] != "sync":
        raise SystemExit("Manifest repair is only available during sync; check never ignores drift")
    current = verify_local(repair_committed)
    if args[0] == "check" and len(args) == 1:
        print(f"Engine snapshot verified: {current['revision']}, {len(current['files'])} files")
        return
    if (args[0] == "sync" and len(args) != 3) or (args[0] == "check" and len(args) != 2):
        raise SystemExit(__doc__)
    source = Path(args[1]).resolve()
    revision = args[2] if args[0] == "sync" else current["revision"]
    with tempfile.TemporaryDirectory(prefix="viptv-engine-import-", dir=DEST.parent) as temp:
        stage = Path(temp)
        package, manifest = export(source, revision, stage)
        if args[0] == "check":
            if current["files"] != manifest["files"]:
                raise SystemExit("Engine snapshot does not match its declared canonical commit")
        else:
            # Validate all input before replacing the generated tree. The rename
            # also removes stale nested copies absent from the canonical source.
            backup = stage / "previous-src"
            (DEST / "src").rename(backup)
            try:
                (package / "src").rename(DEST / "src")
            except BaseException:
                backup.rename(DEST / "src")
                raise
            shutil.copyfile(package / "Cargo.toml", DEST / "Cargo.toml")
            (DEST / "SOURCE.json").write_text(json.dumps(manifest, indent=2) + "\n")
        print(f"Engine snapshot {args[0]}: {revision}, {len(manifest['files'])} canonical files")

if __name__ == "__main__":
    main()
