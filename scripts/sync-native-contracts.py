#!/usr/bin/env python3
"""Export immutable shared Rust sources; never edit imported source by hand."""
import hashlib
import io
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCES = {
    'core': ((ROOT / 'CORE_REF').read_text().strip(), ['Cargo.toml', 'crates/viptv-core', 'LICENSE']),
    'playback-gateway': ('f318d6df9bb3d4d03a4c81c3ea2e524e6f5405d1', ['torrent-policy', 'LICENSE']),
}
def prepare(name, revision, paths, stage):
    data = subprocess.check_output(['git', '-C', str(ROOT.parent / name), 'archive', revision, *paths])
    destination = stage / name
    destination.mkdir()
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive.getmembers():
            target = (destination / member.name).resolve()
            if not target.is_relative_to(destination.resolve()) or not (member.isdir() or member.isfile()):
                raise SystemExit('Unexpected native contract archive member')
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as source:
                    target.write_bytes(source.read())
    if name == 'core':
        workspace = destination / 'Cargo.toml'
        text, count = re.subn(r'^members = \[.*\]$', 'members = ["crates/viptv-core"]', workspace.read_text(), flags=re.M)
        if count != 1:
            raise SystemExit('Unexpected core workspace members; review the manifest transform')
        workspace.write_text(text)
        library = destination / 'crates' / 'viptv-core' / 'Cargo.toml'
        before = 'viptv-engine = { path = "../viptv-engine" }'
        text = library.read_text()
        if text.count(before) != 1:
            raise SystemExit('Unexpected engine dependency; review the manifest transform')
        library.write_text(text.replace(before, 'viptv-engine = { path = "../../../engine" }'))
    return {'revision': revision, 'license': ['GPL-2.0-only'] if name == 'core' else ['GPL-2.0-only', 'Apache-2.0'],
            'archive_sha256': hashlib.sha256(data).hexdigest()}

shared = ROOT / 'server' / 'shared'
manifest_path = shared / 'NATIVE_CONTRACTS.json'
with tempfile.TemporaryDirectory(prefix='native-contract-import-', dir=shared) as directory:
    stage = Path(directory)
    manifest = {name: prepare(name, revision, paths, stage) for name, (revision, paths) in SOURCES.items()}
    manifest['design_revision'] = json.loads(manifest_path.read_text())['design_revision']
    staged_manifest = stage / 'NATIVE_CONTRACTS.json'
    staged_manifest.write_text(json.dumps(manifest, indent=2) + '\n')
    replaced = []
    try:
        for name in SOURCES:
            destination, backup = shared / name, stage / (name + '-previous')
            if destination.exists():
                destination.rename(backup)
            replaced.append((destination, backup))
            (stage / name).rename(destination)
        staged_manifest.replace(manifest_path)
    except BaseException:
        for destination, backup in reversed(replaced):
            if destination.exists():
                shutil.rmtree(destination)
            if backup.exists():
                backup.rename(destination)
        raise
