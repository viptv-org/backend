#!/usr/bin/env python3
"""Export immutable shared Rust sources; never edit imported source by hand."""
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
SOURCES = {
    'core': ((ROOT / 'CORE_REF').read_text().strip(), ['Cargo.toml', 'crates/viptv-core', 'crates/viptv-simkl', 'LICENSE']),
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
        # The exported workspace intentionally contains only the library used here.
        cargo = destination / 'Cargo.toml'
        cargo.write_text(cargo.read_text().replace(
            'members = ["crates/viptv-core", "crates/viptv-provider", "crates/typegen", "crates/viptv-simkl"]',
            'members = ["crates/viptv-core", "crates/viptv-simkl"]'))
    manifest[name] = {'revision': revision, 'license': ['GPL-2.0-only'] if name == 'core' else ['GPL-2.0-only', 'Apache-2.0'],
                      'archive_sha256': hashlib.sha256(data).hexdigest()}
manifest['design_revision'] = (ROOT / 'DESIGN_REF').read_text().strip()
(ROOT / 'server' / 'shared' / 'NATIVE_CONTRACTS.json').write_text(json.dumps(manifest, indent=2) + '\n')
