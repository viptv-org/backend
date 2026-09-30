#!/usr/bin/env python3
"""Actual-image populated browser fixture; private synthetic volume only."""
import argparse
import base64
import json
import os
import re
from pathlib import Path
import secrets
import subprocess
import tempfile
import time

IMAGE = 'sha256:5b5900c8697b89e3519a31fb6685e21e75343b0e3caa3861de697b1aead8b6ef'
REVISION = '27a0296c69755e50412a6efeb9ad52564e029fd3'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sudo', action='store_true')
parser.add_argument('--certificate', required=True)
parser.add_argument('--key', required=True)
parser.add_argument('--seed-only', action='store_true')
parser.add_argument('--many-providers', action='store_true')
args = parser.parse_args()
os.umask(0o077)
docker = (['sudo'] if args.sudo else []) + ['docker']
root = Path(__file__).resolve().parents[1]
evidence = Path(tempfile.mkdtemp(prefix='viptv-populated-'))
os.chmod(evidence, 0o700)
config = {'password': secrets.token_hex(24), 'keyring': {'active': 'synthetic', 'keys': {'synthetic': base64.b64encode(secrets.token_bytes(32)).decode()}}, 'revision': REVISION,
          'origin': 'https://viptv.local.test:18446', 'gateway_key': secrets.token_hex(32), 'extra_providers': 205 if args.many_providers else 0}
private = evidence / 'fixture.json'
private.write_text(json.dumps(config)); os.chmod(private, 0o600)
volume = network = container = None
proxy = None
session = 'populated-' + evidence.name

def command(arguments, data=None, timeout=180):
    result = subprocess.run(arguments, input=data, capture_output=True, timeout=timeout,
                            cwd=evidence if arguments[0] == 'playwright-cli' else None)
    if result.returncode:
        failure = evidence / 'operation-error.log'
        failure.write_bytes(result.stdout + result.stderr); os.chmod(failure, 0o600)
        raise RuntimeError('Fixture operation failed; private diagnostics retained')
    return result.stdout.decode().strip()

try:
    inspection = json.loads(command(docker + ['image', 'inspect', IMAGE]))[0]
    assert inspection['Config']['Labels']['tech.syek.viptv.backend-revision'] == REVISION
    assert not command(['ss', '-ltnH', '( sport = :18446 )'])
    volume = command(docker + ['volume', 'create', session])
    command(docker + ['run', '--rm', '--pull=never', '--network', 'none', '--read-only', '--user', '0:0',
                      '--cap-drop', 'ALL', '--cap-add', 'CHOWN', '--cap-add', 'FOWNER', '--mount', f'type=volume,source={volume},target=/data',
                      '--entrypoint', 'sh', IMAGE, '-c', 'chown 10001:10001 /data && chmod 700 /data'])
    seeded = command(docker + ['run', '--rm', '-i', '--pull=never', '--network', 'none', '--read-only',
                              '--mount', f'type=volume,source={volume},target=/data', '--tmpfs', '/tmp:rw,nosuid,nodev,size=32m',
                              'viptv:populated-seeder-27a'], private.read_bytes())
    (evidence / 'seed.log').write_text(seeded + '\n')
    if args.seed_only:
        print(f'PASS synthetic sealed100k seed; private evidence {evidence}')
    else:
        network = command(docker + ['network', 'create', '--internal', session])
        environment = evidence / 'runtime.env'
        environment.write_text('VIPTV_DATABASE=/data/populated.sqlite\nVIPTV_AUTH_ORIGIN=' + config['origin'] + '\nVIPTV_SECRETS_KEYRING=' + json.dumps(config['keyring']) + '\n')
        os.chmod(environment, 0o600)
        container = command(docker + ['run', '--detach', '--pull=never', '--read-only', '--network', network,
                                     '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--memory', '768m', '--pids-limit', '128',
                                     '--mount', f'type=volume,source={volume},target=/data', '--tmpfs', '/tmp:rw,nosuid,nodev,size=32m',
                                     '--env-file', str(environment), IMAGE])
        details = json.loads(command(docker + ['inspect', container]))[0]
        networks = list(details['NetworkSettings']['Networks'].values())
        assert len(networks) == 1 and networks[0]['NetworkID'] == network
        assert details['Config']['User'] == '10001:10001' and details['HostConfig']['ReadonlyRootfs']
        address = networks[0]['IPAddress']
        caddy = evidence / 'Caddyfile'
        caddy.write_text('{\n admin off\n auto_https disable_redirects\n}\n' + config['origin'] + ' {\n bind 127.0.0.1\n tls "' + str(Path(args.certificate).resolve()) + '" "' + str(Path(args.key).resolve()) + '"\n reverse_proxy ' + address + ':8080\n}\n')
        proxy_log = open(evidence / 'proxy.log', 'wb')
        proxy = subprocess.Popen(['caddy', 'run', '--config', str(caddy)], stdout=proxy_log, stderr=subprocess.STDOUT,
                                 env=dict(os.environ, XDG_DATA_HOME=str(evidence / 'caddy-data'), XDG_CONFIG_HOME=str(evidence / 'caddy-config')))
        for _ in range(40):
            result = subprocess.run(['curl', '--fail', '--silent', '--max-time', '2', '--resolve', 'viptv.local.test:18446:127.0.0.1',
                                     '-o', '/dev/null', '-w', '%{http_code} %{ssl_verify_result}', config['origin'] + '/api/health'], capture_output=True)
            if result.returncode == 0 and result.stdout == b'200 0': break
            time.sleep(.25)
        else: raise RuntimeError('Trusted HTTPS readiness failed')
        browser = evidence / 'browser.json'
        browser.write_text(json.dumps({'browser': {'browserName': 'chromium', 'isolated': True,
                                      'launchOptions': {'args': ['--host-resolver-rules=MAP viptv.local.test 127.0.0.1']},
                                      'contextOptions': {'ignoreHTTPSErrors': False}}}))
        print('Browser critical window: actual local API, no response interception', flush=True)
        command(['playwright-cli', '-s=' + session, 'open', config['origin'], '--browser=chrome', '--config=' + str(browser)], timeout=60)
        source = (root / 'scripts/check-populated-runtime.js').read_text().replace('__FIXTURE_CONFIG__', json.dumps(config))
        executable = evidence / 'browser-check.js'; executable.write_text(source); os.chmod(executable, 0o600)
        for width, height in [(1440, 900), (390, 844)]:
            command(['playwright-cli', '-s=' + session, 'resize', str(width), str(height)])
            output = command(['playwright-cli', '-s=' + session, 'run-code', '--filename=' + str(executable)], timeout=240)
            (evidence / f'browser-{width}.log').write_text(output)
            results = re.findall(r'^"POPULATED_PASS .*"$', output, re.MULTILINE)
            assert len(results) == 1, 'Actual browser result missing (source echo is not acceptance)'
            report = json.loads(json.loads(results[0]).removeprefix('POPULATED_PASS '))
            assert report['viewport'] == {'width': width, 'height': height}
            assert len(report['checks']) >= (1 if args.many_providers else 4)
            (evidence / f'summary-{width}.json').write_text(json.dumps(report, indent=2) + '\n')
        print(f'PASS actual populated image/browser; private evidence {evidence}')
finally:
    try:
        subprocess.run(['playwright-cli', '-s=' + session, 'close'], cwd=evidence, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
    except subprocess.TimeoutExpired:
        (evidence / 'browser-cleanup-gap.txt').write_text('Owned named browser close timed out; inspect this session only.\n')
    if proxy:
        proxy.terminate()
        try: proxy.wait(timeout=10)
        except subprocess.TimeoutExpired: proxy.kill(); proxy.wait(timeout=5)
    if container:
        logs = subprocess.run(docker + ['logs', container], capture_output=True).stdout
        (evidence / 'backend.log').write_bytes(logs)
        subprocess.run(docker + ['container', 'rm', '--force', container], capture_output=True, check=True)
    if network: subprocess.run(docker + ['network', 'rm', network], capture_output=True, check=True)
    if volume: subprocess.run(docker + ['volume', 'rm', volume], capture_output=True, check=True)
    for file in evidence.iterdir():
        if file.is_file(): os.chmod(file, 0o600)
