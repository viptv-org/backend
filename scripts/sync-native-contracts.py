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

ROOT = Path(__file__).resolve().parents[1]
SOURCES = {
    'core': ((ROOT / 'CORE_REF').read_text().strip(), ['Cargo.toml', 'crates/viptv-core', 'LICENSE']),
    'playback-gateway': ('f318d6df9bb3d4d03a4c81c3ea2e524e6f5405d1', ['torrent-policy', 'LICENSE']),
}
manifest = {}
for name, (revision, paths) in SOURCES.items():
    source = ROOT.parent / name
    data = subprocess.check_output(['git', '-C', str(source), 'archive', revision, *paths])
    destination = ROOT / 'server' / 'shared' / name
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(destination, filter='data')
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
    manifest[name] = {'revision': revision, 'license': ['GPL-2.0-only'] if name == 'core' else ['GPL-2.0-only', 'Apache-2.0'],
                      'archive_sha256': hashlib.sha256(data).hexdigest()}
MANIFEST = ROOT / 'server' / 'shared' / 'NATIVE_CONTRACTS.json'
manifest['design_revision'] = json.loads(MANIFEST.read_text())['design_revision']
MANIFEST.write_text(json.dumps(manifest, indent=2) + '\n')
