#!/usr/bin/env python3
"""Isolated candidate-binary, encrypted synthetic catalog, trusted HTTPS acceptance.

No production data, containers, shared runtime, or TLS bypass. Private diagnostics
remain in the printed 0700 evidence directory. Only owned processes are stopped.
"""
import argparse
import base64
import http.cookiejar
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['server', 'provider-owners', 'vault-helper', 'dist', 'certificate', 'key']:
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--api-only', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    root = Path(__file__).resolve().parents[1]
    evidence = Path(tempfile.mkdtemp(prefix='viptv-bounded-vod-'))
    os.chmod(evidence, 0o700)
    session = evidence.name
    processes = []
    # Bind probes refuse occupied addresses; random ports avoid shared fixtures.
    def free_port():
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            return sock.getsockname()[1]
    backend_port, tls_port = free_port(), free_port()
    while tls_port == backend_port:
        tls_port = free_port()
    cfg = {'password': secrets.token_hex(24), 'keyring': {'active': 'synthetic', 'keys': {'synthetic': base64.b64encode(secrets.token_bytes(32)).decode()}},
           'origin': f'https://viptv.local.test:{tls_port}', 'gateway_key': secrets.token_hex(32), 'extra_providers': 205,
           'revision': subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()}
    (evidence / 'fixture.json').write_text(json.dumps(cfg))
    database = evidence / 'populated.sqlite'
    env = dict(os.environ, VIPTV_DATABASE=str(database), VIPTV_AUTH_ORIGIN=cfg['origin'], VIPTV_SECRETS_KEYRING=json.dumps(cfg['keyring']),
               VIPTV_BIND=f'127.0.0.1:{backend_port}', VIPTV_DASHBOARD_DIST=str(args.dist.resolve()), VIPTV_TV_DIST='')
    def command(argv, data=None, timeout=300):
        result = subprocess.run(argv, input=data, capture_output=True, timeout=timeout, env=env, cwd=evidence)
        if result.returncode:
            (evidence / 'operation-error.log').write_bytes(result.stdout + result.stderr)
            raise RuntimeError('Fixture operation failed; inspect private operation-error.log')
        return result.stdout.decode()
    try:
        # Reuse the exact existing seeder; replace only container-specific paths.
        seed = (root / 'scripts/populated-seed.py').read_text()
        seed = seed.replace("Path('/data/populated.sqlite')", 'Path(' + repr(str(database)) + ')')
        seed = seed.replace("'viptv-server'", repr(str(args.server.resolve())))
        seed = seed.replace("'provider-owners'", repr(str(args.provider_owners.resolve())))
        seed = seed.replace("'/qualification/vault-helper'", repr(str(args.vault_helper.resolve())))
        seed = seed.replace("f'/data/before-", "f'" + str(evidence) + '/before-')
        seeded = command(['python3', '-c', seed], json.dumps(cfg).encode())
        (evidence / 'seed.log').write_text(seeded)
        for executable, argv, logfile in [
            (args.server.resolve(), [], 'backend.log'),
        ]:
            log = open(evidence / logfile, 'wb')
            processes.append(subprocess.Popen([str(executable), *argv], env=env, stdout=log, stderr=subprocess.STDOUT))
        caddy = evidence / 'Caddyfile'
        caddy.write_text('{\n admin off\n auto_https disable_redirects\n}\n' + cfg['origin'] + ' {\n bind 127.0.0.1\n tls "' + str(args.certificate.resolve()) + '" "' + str(args.key.resolve()) + '"\n reverse_proxy 127.0.0.1:' + str(backend_port) + '\n}\n')
        processes.append(subprocess.Popen(['caddy', 'run', '--config', str(caddy)], stdout=open(evidence / 'proxy.log', 'wb'), stderr=subprocess.STDOUT,
                                          env=dict(env, XDG_DATA_HOME=str(evidence / 'caddy-data'), XDG_CONFIG_HOME=str(evidence / 'caddy-config'))))
        for _ in range(80):
            result = subprocess.run(['curl', '--fail', '--silent', '--max-time', '2', '-o', '/dev/null', '-w', '%{http_code} %{ssl_verify_result}', cfg['origin'] + '/api/health'], capture_output=True)
            if result.returncode == 0 and result.stdout == b'200 0':
                break
            if any(p.poll() is not None for p in processes):
                raise RuntimeError('Owned runtime exited; inspect private logs')
            time.sleep(.25)
        else:
            raise RuntimeError('Trusted HTTPS readiness failed; no -k fallback')
        jar = http.cookiejar.CookieJar()
        client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        def api(path, method='GET', body=None):
            headers = {'Origin': cfg['origin'], 'Content-Type': 'application/json'}
            if method != 'GET':
                csrf = api('/api/auth/status').get('csrf_token')
                if csrf:
                    headers['x-csrf-token'] = csrf
            request = urllib.request.Request(cfg['origin'] + path, method=method, headers=headers,
                                             data=None if body is None else json.dumps(body).encode())
            with client.open(request, timeout=30) as response:
                return json.load(response)
        api('/api/auth/login', 'POST', {'username': 'qa_member', 'password': cfg['password']})
        api('/api/auth/profile', 'POST', {'profile_id': 2})
        provider_ids, cursor = [], None
        while True:
            page = api('/api/v2/iptv/connections?limit=200' + (('&cursor=' + cursor) if cursor else ''))
            provider_ids.extend(int(item['id']) for item in page['items'])
            cursor = page['next_cursor']
            if not cursor:
                break
        assert len(provider_ids) == len(set(provider_ids)) == 208 and 605 in provider_ids and 201 not in provider_ids
        base = '/api/v2/iptv/matches?provider_id=101&limit=50'
        page, forward, max_cursor, pages = api(base), 0, 0, 0
        while True:
            assert 0 < len(page['items']) <= 50 and 'total' not in page
            for item in page['items']:
                forward += 1
                assert item['vod_id'] == f'vod:101:{forward:06}', 'Forward omission/duplicate/order'
            pages += 1
            for key in ['next_cursor', 'previous_cursor']:
                token = page[key]
                if token:
                    assert len(token) <= 4096 and re.fullmatch(r'[A-Za-z0-9_-]+', token)
                    max_cursor = max(max_cursor, len(token))
            if not page['next_cursor']:
                break
            page = api(base + '&cursor=' + urllib.parse.quote(page['next_cursor']))
        assert forward == 99999
        backward = forward
        reverse_pages = 0
        while True:
            for item in reversed(page['items']):
                assert item['vod_id'] == f'vod:101:{backward:06}', 'Reverse omission/duplicate/order'
                backward -= 1
            reverse_pages += 1
            if not page['previous_cursor']:
                break
            page = api(base + '&cursor=' + urllib.parse.quote(page['previous_cursor']))
        assert backward == 0
        with client.open(cfg['origin'] + '/') as response:
            served_index = response.read().decode()
        asset = re.search(r'assets/index-[A-Za-z0-9_-]+\.js', served_index)
        assert asset and (args.dist / asset[0]).is_file(), 'Candidate assets not served'
        report = {'candidate_revision': cfg['revision'], 'catalog_rows': 100000, 'initial_matched_rows': 1, 'unmatched_forward': forward, 'unmatched_reverse': forward - backward,
                  'forward_pages': pages, 'reverse_pages': reverse_pages, 'maximum_cursor_bytes': max_cursor, 'owned_providers': len(provider_ids),
                  'tls_verification': 'system trust, curl200 ssl_verify_result0; browser ignoreHTTPSErrors=false', 'candidate_sha256': command(['sha256sum', str(args.server.resolve())]).split()[0],
                  'served_candidate_asset': asset[0], 'asset_sha256': command(['sha256sum', str((args.dist / asset[0]).resolve())]).split()[0]}
        (evidence / 'api-summary.json').write_text(json.dumps(report, indent=2))
        print('PASS actual trusted-HTTPS API: encrypted100000 raw titles,99999 unmatched both directions,208 owned providers', flush=True)
        if not args.api_only:
            browser = evidence / 'browser.json'
            browser.write_text(json.dumps({'browser': {'browserName': 'chromium', 'isolated': True, 'contextOptions': {'ignoreHTTPSErrors': False}}}))
            command(['playwright-cli', '-s=' + session, 'open', cfg['origin'], '--browser=chrome', '--config=' + str(browser)], timeout=60)
            executable = evidence / 'browser-check.js'
            cfg['skipped_ids'] = []
            for width, height in [(1440, 900), (390, 844)]:
                source = (root / 'scripts/check-bounded-vod.js').read_text().replace('__FIXTURE_CONFIG__', json.dumps(cfg))
                executable.write_text(source)
                command(['playwright-cli', '-s=' + session, 'resize', str(width), str(height)])
                output = command(['playwright-cli', '-s=' + session, 'run-code', '--filename=' + str(executable)], timeout=1800)
                (evidence / f'browser-{width}.log').write_text(output)
                found = re.findall(r'^"BOUNDED_PASS .*"$', output, re.MULTILINE)
                assert len(found) == 1, 'Browser returned no actual acceptance result'
                summary = json.loads(json.loads(found[0]).removeprefix('BOUNDED_PASS '))
                (evidence / f'summary-{width}.json').write_text(json.dumps(summary, indent=2))
                cfg['skipped_ids'].append(int(summary['saved_id'].rsplit(':', 1)[1]))
                cfg['skipped_ids'].sort()
                print(f'PASS actual browser{width} full frontend traversal and affected-state checks', flush=True)
        print('Private evidence: ' + str(evidence), flush=True)
    finally:
        subprocess.run(['playwright-cli', '-s=' + session, 'close'], cwd=evidence, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        print('Private diagnostics retained; exact owned runtime processes stopped: ' + str(evidence), flush=True)


if __name__ == '__main__':
    main()
