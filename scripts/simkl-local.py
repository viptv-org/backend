#!/usr/bin/env python3
"""Run the SIMKL experiment against its own database and private local credentials."""
import base64
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys

workspace = Path(__file__).resolve().parents[2]
private = workspace / '.local-https' / 'simkl'
private.mkdir(parents=True, exist_ok=True)
os.chmod(private, 0o700)
env = os.environ.copy()
for line in (workspace / '.env').read_text().splitlines():
    key, separator, value = line.partition('=')
    if separator and key.startswith('SIMKL_'):
        env[key] = value.strip().strip('"').strip("'")
keyfile = private / 'keyring.json'
if not keyfile.exists():
    keyfile.write_text(json.dumps({'active': 'simkl-local', 'keys': {'simkl-local': base64.b64encode(secrets.token_bytes(32)).decode()}}))
    os.chmod(keyfile, 0o600)
env['VIPTV_SECRETS_KEYRING'] = keyfile.read_text()
env['VIPTV_DATABASE'] = str(private / 'viptv.sqlite')
env['VIPTV_BIND'] = '127.0.0.1:18191'
env['VIPTV_AUTH_ORIGIN'] = 'https://10.0.2.2:18443'
env['VIPTV_DASHBOARD_DIST'] = str(workspace / 'web' / 'dist')
# Register this additional callback in SIMKL before exercising real OAuth locally.
env['SIMKL_REDIRECT_URI'] = 'https://viptv.local.test:18443/api/integrations/simkl/callback'
binary = workspace / 'backend' / 'server' / 'target' / 'debug' / 'viptv-server'
sys.exit(subprocess.call([str(binary), *sys.argv[1:]], env=env, cwd=workspace / 'backend'))
