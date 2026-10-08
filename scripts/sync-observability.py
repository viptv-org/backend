#!/usr/bin/env python3
"""Vendor the pinned service telemetry crate without repinning native contracts."""
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
revision = (ROOT / 'OBSERVABILITY_REF').read_text().strip()
source = ROOT.parent / 'playback-gateway'
data = subprocess.check_output(['git', '-C', str(source), 'archive', revision,
                                'telemetry', 'LICENSE'])
destination = ROOT / 'server' / 'shared' / 'service-telemetry'
if destination.exists():
    shutil.rmtree(destination)
destination.mkdir(parents=True)
with tarfile.open(fileobj=io.BytesIO(data)) as archive:
    for member in archive.getmembers():
        if member.name.startswith('telemetry/'):
            member.name = member.name.removeprefix('telemetry/')
        if member.name and member.name != 'telemetry':
            archive.extract(member, destination, filter='data')
(destination / 'PROVENANCE.json').write_text(json.dumps({
    'repository': 'https://github.com/viptv-org/playback-gateway',
    'revision': revision,
    'license': 'GPL-2.0-only',
    'archive_sha256': hashlib.sha256(data).hexdigest(),
}, indent=2) + '\n')
