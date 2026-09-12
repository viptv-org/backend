#!/usr/bin/env python3
"""Non-destructive live-host checks using an existing owner browser account.

Credentials come only from VIPTV_TEST_USERNAME and VIPTV_TEST_PASSWORD in the
process environment. The script never creates accounts, profiles, or library rows.
"""
import argparse
from http.cookies import SimpleCookie
import json
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request

p = argparse.ArgumentParser()
p.add_argument('--env-file', required=True, help='deployment settings only; not an account credential file')
p.add_argument('--container', default='viptv-viptv-1')
p.add_argument('--check-cinemeta', action='store_true')
a = p.parse_args()
env = {}
for line in Path(a.env_file).read_text().splitlines():
    if line.strip() and not line.lstrip().startswith('#') and '=' in line:
        key, value = line.split('=', 1)
        env[key.strip()] = value.strip().strip('"\'')
username = os.environ['VIPTV_TEST_USERNAME']
password = os.environ['VIPTV_TEST_PASSWORD']
assert username and len(username) <= 64 and 12 <= len(password) <= 256, 'Invalid test account credentials'
ip = env.get('VIPTV_PUBLISH_IP', '127.0.0.1')
if ip == '0.0.0.0':
    ip = '127.0.0.1'
base = os.environ.get('VIPTV_TEST_URL', 'http://' + ip + ':' + env.get('VIPTV_PORT', '8080')).rstrip('/')
origin = os.environ.get('VIPTV_TEST_ORIGIN', env.get('VIPTV_AUTH_ORIGIN') or base.replace('http://', 'https://', 1))

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

opener = urllib.request.build_opener(NoRedirect())
cookies = {}
csrf = ''

def api(path, method='GET', body=None, authenticated=True):
    global csrf
    headers = {'Accept': 'application/json', 'User-Agent': 'VIPTV-Live-Check/1.6'}
    if authenticated and cookies:
        headers['Cookie'] = '; '.join(f'{key}={value}' for key, value in cookies.items())
    if method not in ('GET', 'HEAD'):
        headers['Origin'] = origin
        if authenticated and csrf:
            headers['X-CSRF-Token'] = csrf
    data = None
    if body is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(body).encode()
    request = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    with opener.open(request, timeout=30) as response:
        payload = response.read()
        for value in response.headers.get_all('Set-Cookie', []):
            parsed = SimpleCookie(); parsed.load(value)
            for key, morsel in parsed.items():
                if morsel.value:
                    cookies[key] = morsel.value
                else:
                    cookies.pop(key, None)
        result = json.loads(payload) if payload else None
        if isinstance(result, dict) and isinstance(result.get('csrf_token'), str):
            csrf = result['csrf_token']
        return result

assert api('/api/health', authenticated=False)['status'] == 'ok'
print('PASS live container public health')
try:
    api('/api/profiles', authenticated=False)
    raise AssertionError('Unauthenticated profile access was accepted')
except urllib.error.HTTPError as error:
    assert error.code == 401, 'Unexpected unauthenticated status'
print('PASS protected API requires an account or paired-device session')
api('/api/auth/login', 'POST', {'username': username, 'password': password}, authenticated=False)
assert cookies and csrf, 'Login did not establish cookie/CSRF state'
me = api('/api/auth/me')
assert me['account']['role'] == 'owner', 'Live check requires an existing owner account'
print('PASS existing owner browser session authenticates without a shared bearer')
status = api('/api/status')
assert status['ffmpeg_available'] is True
profiles = api('/api/profiles')
print('PASS FFmpeg is available and account-owned profile listing is readable')

def docker(*args):
    return subprocess.check_output(['docker', '--host', 'unix:///var/run/docker.sock', *args], text=True).strip()

template = ('{"user":{{json .Config.User}},"readonly":{{json .HostConfig.ReadonlyRootfs}},'
            '"health":{{json .State.Health.Status}},"memory":{{json .HostConfig.Memory}},'
            '"swap":{{json .HostConfig.MemorySwap}},"cpus":{{json .HostConfig.NanoCpus}},'
            '"pids":{{json .HostConfig.PidsLimit}},"cap_drop":{{json .HostConfig.CapDrop}},'
            '"image":{{json .Image}},"ports":{{json .NetworkSettings.Ports}}}')
info = json.loads(docker('inspect', '--format', template, a.container))
assert info['user'] == '10001:10001'
assert info['readonly'] is True and info['health'] == 'healthy'
assert info['memory'] == 0 and info['swap'] == 0
assert info['cpus'] == 0 and info['pids'] in (0, None)
assert 'ALL' in info['cap_drop']
assert info['image'] == docker('image', 'inspect', 'viptv:local', '--format', '{{.Id}}')
assert info['ports']['8080/tcp'] == [{'HostIp': ip, 'HostPort': env.get('VIPTV_PORT', '8080')}]
print('PASS actual image identity, nonroot/read-only/capability guards, unthrottled CPU/memory/PID and scoped binding')
docker('exec', a.container, 'sh', '-ec',
       'test "$(id -u)" = 10001; test "$(stat -c %a /data/viptv.sqlite)" = 600; '
       'test -w /data; f=$(mktemp /cache/viptv-check.XXXXXX); rm "$f"; test ! -w /app')
print('PASS SQLite permissions, persistent data access and actual ephemeral-cache write/cleanup')
if a.check_cinemeta:
    complete = next((profile for profile in profiles if profile.get('setup_complete', profile.get('presentation_complete', True))), None)
    assert complete is not None, 'Metadata check needs an existing completed profile; this script will not create one'
    api('/api/auth/profile', 'POST', {'profile_id': complete['id']})
    metas = api('/api/discover?type=movie&search=The%20Matrix')['metas']
    assert len(metas) > 0, 'No metadata returned by configured search catalogs'
    print('PASS actual configured metadata search:', len(metas), 'results')
print('Live deployment checks passed; no accounts, profiles, providers, addons, favorites, or progress rows were created.')
