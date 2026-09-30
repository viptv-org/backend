#!/usr/bin/env python3
"""Offline synthetic data only; shipped CLI owns schema, password hash and sealing."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time

config = json.load(sys.stdin)
database = Path('/data/populated.sqlite')
assert not database.exists(), 'Fixture requires a fresh owned volume'
os.environ['VIPTV_DATABASE'] = str(database)
os.environ['VIPTV_AUTH_ORIGIN'] = config['origin']
os.environ['VIPTV_SECRETS_KEYRING'] = json.dumps(config['keyring'])
result = subprocess.run(['viptv-server', 'create-admin', 'qa_operator', 'Synthetic operator'],
                        input=config['password'].encode(), capture_output=True)
if result.returncode:
    diagnostic = result.stderr.decode(errors='replace').replace(config['password'], '[private]')
    for key in config['keyring']['keys'].values(): diagnostic = diagnostic.replace(key, '[private]')
    sys.stderr.write(diagnostic)
assert result.returncode == 0, 'Synthetic schema/owner initialization failed'
db = sqlite3.connect(database)
db.execute('PRAGMA foreign_keys=ON')
owner = db.execute("SELECT id,password_hash FROM auth_accounts WHERE username='qa_operator'").fetchone()
assert owner[0] == 1
now = int(time.time())
for account, username in [(2, 'qa_member'), (3, 'qa_foreign')]:
    db.execute('INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(?,?,?,?,?,?,?)',
               (account, username, username, owner[1], 'member', 'synthetic-unused-recovery', now))
for account in [1, 2, 3]:
    db.execute('INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(?,?,?,1)',
               (account, f'Synthetic profile {account}', f'fixture-{account}'))
    db.execute('INSERT INTO profile_owners VALUES(?,?,?)', (account, account, now))
    db.execute('INSERT INTO auth_profiles VALUES(?,?)', (account, account))
db.execute('DELETE FROM addons WHERE account_id=0')
for provider, account in [(101, 2), (102, 2), (103, 2), (201, 3), (301, 1)]:
    db.execute('INSERT INTO providers(id,name,url,username,password) VALUES(?,?,?,?,?)',
               (provider, f'Synthetic provider {provider}', f'https://provider-{provider}.fixture.invalid', 'synthetic-user', 'synthetic-password'))
    db.execute('INSERT INTO provider_ownership VALUES(?,?)', (provider, account))
    db.execute("INSERT INTO provider_refresh_v2(provider_id,account_id,token,state,requested_at,finished_at,counts) VALUES(?,?,?,'cancelled',?,?,?)",
               (provider, account, f'fixture-{provider}', now, now, '{}'))
    for index in range(3):
        db.execute('INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?,?,?,?)',
                   (f'iptv:{provider}:{index}', provider, str(index), f'Channel {provider}-{index}'))
    count = 100000 if provider == 101 else 150
    db.executemany('INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES(?,?,?,?,?,?,?,?)',
                   ((f'vod:{provider}:{index:06}', provider, str(index), 'movie', f'Synthetic title {index:06}', f'synthetic title {index:06}', 2020, 'mp4') for index in range(count)))
    db.execute('INSERT INTO provider_matches VALUES(?,?,?)', (f'vod:{provider}:000000', 'tt1234567', 'movie'))
for account, provider in [(1, 301), (2, 101), (3, 201)]:
    db.execute('INSERT INTO account_media_settings(account_id,default_live_provider_id) VALUES(?,?)', (account, provider))
    manifest = {'id': f'fixture.{account}', 'name': f'Synthetic addon {account}', 'resources': ['stream'], 'types': ['movie'], 'catalogs': []}
    db.execute('INSERT INTO addons(id,account_id,name,manifest_url,manifest) VALUES(?,?,?,?,?)',
               (account, account, manifest['name'], f'https://addon-{account}.fixture.invalid/private-token/manifest.json', json.dumps(manifest)))
extra = config.get('extra_providers', 0)
assert extra in (0, 205), 'Fixture provider bound required'
for provider in range(401, 401 + extra):
    db.execute('INSERT INTO providers(id,name,url,username,password,enable_live,enable_series) VALUES(?,?,?,?,?,0,0)',
               (provider, f'Synthetic provider {provider}', f'https://provider-{provider}.fixture.invalid', 'synthetic-user', 'synthetic-password'))
    db.execute('INSERT INTO provider_ownership VALUES(?,2)', (provider,))
    db.execute("INSERT INTO provider_refresh_v2(provider_id,account_id,token,state,requested_at) VALUES(?,2,?,'cancelled',?)", (provider, f'fixture-{provider}', now))
if extra:
    db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES('vod:605:tail',605,'tail','movie','Tail provider title','tail provider title',2020,'mp4')")
gateway_id = 'synthetic-operator-gateway'
secret_input = {'keyring': config['keyring'], 'account': 1, 'id': gateway_id, 'value': 'pgk_' + config['gateway_key']}
sealed = subprocess.run(['/qualification/vault-helper'], input=json.dumps(secret_input).encode(), capture_output=True, check=True).stdout.decode().strip()
db.execute('INSERT INTO playback_gateways(id,owner_account_id,name,endpoint,namespace,priority,secret) VALUES(?,?,?,?,?,?,?)',
           (gateway_id, 1, 'Synthetic operator gateway', 'https://gateway.fixture.invalid/', 'synthetic', 10, sealed))
db.execute('INSERT INTO playback_gateway_grants VALUES(?,?)', (gateway_id, 2))
db.commit()
db.close()
for mode in ['encrypt', 'encrypt-addons']:
    result = subprocess.run(['provider-owners', mode, str(database), f'/data/before-{mode}.sqlite',
                             f'/data/before-{mode}.json', config['revision'], '--confirm-encryption'], capture_output=True)
    assert result.returncode == 0, f'Shipped CLI {mode} refused synthetic fixture'
db = sqlite3.connect(database)
assert db.execute('SELECT count(*) FROM providers WHERE credentials_version=1').fetchone()[0] == 5 + extra
assert db.execute('SELECT count(*) FROM addons WHERE credentials_version=1').fetchone()[0] == 3
assert db.execute('SELECT count(*) FROM provider_vod WHERE provider_id=101').fetchone()[0] == 100000
assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
db.close()
print('Synthetic populated fixture sealed: five providers, three addons, offline unverified gateway, member100k VOD; no runtime fetch')
