"""Package the hosted locked release with source and ELF compatibility evidence."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ALLOWED_LIBRARIES = {'libgcc_s.so.1', 'libm.so.6', 'libc.so.6', 'ld-linux-x86-64.so.2'}


def inspect_abi(dynamic, versions):
    libraries = sorted(set(re.findall(r'Shared library: \[([^\]]+)\]', dynamic)))
    if not libraries or set(libraries) - ALLOWED_LIBRARIES:
        raise ValueError('unexpected_runtime_libraries')
    names = set(re.findall(r'Name: (GLIBC_[^\s]+)', versions))
    if not names or any(not re.fullmatch(r'GLIBC_\d+\.\d+(?:\.\d+)?', name) for name in names):
        raise ValueError('unexpected_glibc_version_requirement')
    maximum = max(tuple(map(int, name.removeprefix('GLIBC_').split('.'))) for name in names)
    if maximum > (2, 35):
        raise ValueError('glibc_requirement_exceeds_2_35')
    return {'needed_libraries': libraries, 'maximum_glibc_required': '.'.join(map(str, maximum)),
            'glibc_ceiling': '2.35', 'target': 'x86_64-unknown-linux-gnu'}


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for part in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(part)
    return value.hexdigest()


def main(messages, output):
    root = Path.cwd().resolve()
    git = lambda *args: subprocess.check_output(['git', *args], text=True).strip()
    revision = git('rev-parse', 'HEAD')
    if revision != os.environ['SOURCE_REVISION'] or git('status', '--porcelain', '--untracked-files=no'):
        raise ValueError('source_revision_or_cleanliness_mismatch')
    records = [json.loads(line) for line in Path(messages).read_text().splitlines() if line.startswith('{')]
    artifacts = [row for row in records if row.get('reason') == 'compiler-artifact'
                 and row.get('manifest_path') == str(root / 'server/Cargo.toml')
                 and row.get('target', {}).get('name') == 'viptv-server' and row.get('executable')]
    if len(artifacts) != 1 or artifacts[0]['fresh']:
        raise ValueError('fresh_exact_server_artifact_required')
    artifact = artifacts[0]
    if not {'default', 'live-runtime', 'vsources-runtime'}.issubset(artifact['features']):
        raise ValueError('required_runtime_features_missing')
    binary = Path(artifact['executable'])
    expected = root / 'server/target/x86_64-unknown-linux-gnu/release/viptv-server'
    if binary.resolve() != expected.resolve():
        raise ValueError('unexpected_binary_location')
    header = binary.read_bytes()[:20]
    if header[:6] != b'\x7fELF\x02\x01' or int.from_bytes(header[18:20], 'little') != 62:
        raise ValueError('linux_x86_64_elf_required')
    readelf = lambda flag: subprocess.check_output(['readelf', flag, str(binary)], text=True)
    abi = inspect_abi(readelf('--dynamic'), readelf('--version-info'))
    engine = root / 'server/shared/engine'
    core_revision = json.loads((engine / 'SOURCE.json').read_text())['revision']
    sdk = re.search(r'^vsources = .*rev = "([a-f0-9]{40})"', (engine / 'Cargo.toml').read_text(), re.MULTILINE)
    if sdk is None:
        raise ValueError('immutable_sdk_revision_required')
    manifest = {'source_revision': revision, 'source_tree': git('rev-parse', 'HEAD^{tree}'),
                'server_tree': git('rev-parse', 'HEAD:server'), 'binary_sha256': digest(binary),
                'binary_file': 'viptv-server', 'core_revision': core_revision, 'sdk_revision': sdk.group(1),
                'binary_bytes': binary.stat().st_size, 'features': sorted(artifact['features']),
                'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
                'build': 'cargo build --locked --release --target x86_64-unknown-linux-gnu --features live-runtime --bin viptv-server',
                'workflow_run_id': os.environ['GITHUB_RUN_ID'], 'workflow_run_attempt': os.environ['GITHUB_RUN_ATTEMPT'],
                'source_branch': os.environ['SOURCE_BRANCH'], 'runner': os.environ.get('ImageOS', os.environ['RUNNER_OS']),
                'abi': abi, 'release_passed': True}
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    with tempfile.TemporaryDirectory() as temporary:
        stage = Path(temporary)
        shutil.copy2(binary, stage / 'viptv-server')
        (stage / 'viptv-server').chmod(0o755)
        shutil.copy2(output / 'manifest.json', stage / 'manifest.json')
        shutil.copy2(root / 'LICENSE', stage / 'LICENSE')
        (stage / 'SHA256SUMS').write_text(manifest['binary_sha256'] + '  viptv-server\n')
        with tarfile.open(output / 'backend-linux-x86_64.tar.gz', 'w:gz') as archive:
            for path in sorted(stage.iterdir()):
                archive.add(path, arcname=path.name)
    (output / 'SHA256SUMS').write_text(''.join(digest(output / name) + '  ' + name + '\n'
                                            for name in ['backend-linux-x86_64.tar.gz', 'manifest.json']))
    print(json.dumps({'source_revision': revision, 'binary_sha256': manifest['binary_sha256'], 'artifact_packaged': True}))


if __name__ == '__main__':
    main(*sys.argv[1:])
