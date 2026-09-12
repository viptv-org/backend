#!/usr/bin/env python3
"""Test read-only host preflight behavior using fake Docker commands, not a daemon."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts' / 'host-check.sh'
BASH = shutil.which('bash')
SECRET = 'sensitive-fixture-setting-not-real'


class HostCheckTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='viptv-host-check-')
        self.root = Path(self.directory.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'calls.jsonl'
        for file in ['compose.yaml', 'Dockerfile', 'server/Cargo.lock', 'dashboard/package-lock.json']:
            path = self.root / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixture\n')
        stat = shutil.which('stat')
        if stat:
            (self.bin / 'stat').symlink_to(stat)
        self.env = dict(os.environ, PATH=str(self.bin), VIPTV_PROJECT_DIR=str(self.root),
                        VIPTV_AUTH_ORIGIN='https://tv.fixture', CALL_LOG=str(self.log))
        for name in ['INFO_FAIL', 'NO_PLUGIN', 'CONFIG_FAIL']:
            self.env.pop(name, None)
        mock = f'''#!{sys.executable}
import json,os,pathlib,sys
args=sys.argv[1:]
with open(os.environ['CALL_LOG'],'a') as f: f.write(json.dumps(args)+'\\n')
if args[:1]==['info']:
    failed=os.environ.get('INFO_FAIL')
elif args==['compose','version']:
    failed=os.environ.get('NO_PLUGIN')
elif args==['version']:
    failed=False
elif args[-2:]==['config','--quiet']:
    assert '--project-directory' in args and '--env-file' in args
    failed=os.environ.get('CONFIG_FAIL')
else:
    raise AssertionError('Unexpected/non-read-only Docker command: '+repr(args))
if failed:
    print({SECRET!r})
    print({SECRET!r},file=sys.stderr)
    sys.exit(1)
print('fixture-version')
'''
        self.mock = mock
        self.install('docker')

    def tearDown(self):
        self.directory.cleanup()

    def install(self, name):
        path = self.bin / name
        path.write_text(self.mock)
        path.chmod(0o700)

    def run_check(self, **updates):
        result = subprocess.run([BASH, str(SCRIPT)], env=dict(self.env, **updates),
                                text=True, capture_output=True, timeout=5)
        self.assertNotIn(SECRET, result.stdout + result.stderr)
        return result

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_ready_host_only_uses_read_only_commands(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('Preflight passed', result.stdout)
        self.assertEqual(len(self.calls()), 3)

    def test_missing_or_non_https_origin_stops_before_docker(self):
        result = self.run_check(VIPTV_AUTH_ORIGIN='')
        self.assertEqual(result.returncode, 1)
        self.assertIn('exact HTTPS origin', result.stdout)
        self.assertEqual(self.calls(), [])
        result = self.run_check(VIPTV_AUTH_ORIGIN='http://tv.fixture')
        self.assertEqual(result.returncode, 1)
        self.assertIn('exact HTTPS origin', result.stdout)

    def test_missing_docker_is_actionable(self):
        (self.bin / 'docker').unlink()
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('Docker CLI is unavailable', result.stdout)

    def test_daemon_failure_redacts_diagnostics(self):
        result = self.run_check(INFO_FAIL='1')
        self.assertEqual(result.returncode, 1)
        self.assertIn('Docker daemon is unreachable', result.stdout)

    def test_invalid_config_redacts_rendered_secret(self):
        result = self.run_check(CONFIG_FAIL='1')
        self.assertEqual(result.returncode, 1)
        self.assertIn('Compose configuration is invalid', result.stdout)

    def test_standalone_compose_fallback(self):
        self.install('docker-compose')
        result = self.run_check(NO_PLUGIN='1')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('Using standalone Compose', result.stdout)

    def test_private_env_is_passed_explicitly(self):
        file = self.root / '.env'
        file.write_text('VIPTV_AUTH_ORIGIN=https://tv.fixture\n')
        file.chmod(0o600)
        self.assertEqual(self.run_check().returncode, 0)
        self.assertIn(str(file), self.calls()[-1])
        self.assertEqual(file.read_text(), 'VIPTV_AUTH_ORIGIN=https://tv.fixture\n')

    def test_insecure_env_stops_before_docker(self):
        file = self.root / '.env'
        file.write_text('VIPTV_AUTH_ORIGIN=https://tv.fixture\n')
        file.chmod(0o644)
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('restrict it to the owner', result.stdout)
        self.assertEqual(self.calls(), [])

    def test_symlink_env_stops_before_docker(self):
        target = self.root / 'secret'
        target.write_text(SECRET)
        (self.root / '.env').symlink_to(target)
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('symlink', result.stdout)
        self.assertEqual(self.calls(), [])


if __name__ == '__main__':
    unittest.main()
