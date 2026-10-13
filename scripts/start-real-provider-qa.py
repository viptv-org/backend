#!/usr/bin/env python3
"""Start isolated real-provider QA on 18185/18445; print only the private run directory.

Build server/target/debug/viptv-server first. SIMKL application credentials are
read from WORKSPACE/.env, never printed. Uses fresh local owner/DB/keyring and
existing trusted local TLS certificates. This does not copy provider credentials,
link a Simkl user, start a gateway, or modify the original local environment.
Stop the PIDs recorded in fixture.json and caddy.pid when finished.
"""
import pathlib, os, json, base64, secrets, tempfile, subprocess, shlex, shutil
import argparse, socket
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--workspace',type=pathlib.Path,default=pathlib.Path('/mnt/ALPH/code/viptv-org'))
root=parser.parse_args().workspace.resolve()
for port in (18185,18445):
 with socket.socket() as listener:
  listener.bind(('127.0.0.1',port))
os.umask(0o077)
d=pathlib.Path(tempfile.mkdtemp(prefix='viptv-real-provider-')); d.chmod(0o700)
env=os.environ.copy()
for line in (root/'.env').read_text().splitlines():
 if '=' not in line: continue
 k,v=line.split('=',1)
 if k.strip() in ('SIMKL_CLIENT_ID','SIMKL_CLIENT_SECRET'):
  env[k.strip()]=shlex.split(v)[0]
key={'active':'qa','keys':{'qa':base64.b64encode(secrets.token_bytes(32)).decode()}}
env.update(VIPTV_SECRETS_KEYRING=json.dumps(key),VIPTV_DATABASE=str(d/'qa.sqlite'),VIPTV_AUTH_ORIGIN='https://viptv.local.test:18445',VIPTV_BIND='127.0.0.1:18185',OBSERVABILITY_ENABLED='false')
bin=root/'.worktrees/kmp-v2/backend/server/target/debug/viptv-server'
password=secrets.token_urlsafe(32)
created=subprocess.run([str(bin),'create-admin','qa-parent','Real Provider QA'],input=password,text=True,env=env,capture_output=True)
if created.returncode: raise RuntimeError('Owner creation failed; output withheld')
(d/'recovery.txt').write_text(created.stdout); (d/'recovery.txt').chmod(0o600)
(d/'keyring.json').write_text(json.dumps(key)); (d/'keyring.json').chmod(0o600)
log=open(d/'backend.log','w'); os.chmod(d/'backend.log',0o600)
p=subprocess.Popen([str(bin)],env=env,stdout=log,stderr=log,start_new_session=True)
config={'username':'qa-parent','password':password,'api_upstream':'http://127.0.0.1:18185','origin':'https://viptv.local.test:18445','database':str(d/'qa.sqlite'),'pid':p.pid}
(d/'fixture.json').write_text(json.dumps(config)); (d/'fixture.json').chmod(0o600)
shutil.copytree(pathlib.Path(__file__).parent/'real-provider-qa',d/'web')
caddy=f'''{{
 admin off
 auto_https off
}}
https://viptv.local.test:18445 {{
 bind 127.0.0.1
 tls {root}/.local-https/certs/viptv.local.test.crt {root}/.local-https/certs/viptv.local.test.key
 @landing path /
 redir @landing /qa/ 302
 handle_path /qa/* {{
  root * {d}/web
  file_server
 }}
 handle {{
  reverse_proxy 127.0.0.1:18185
 }}
}}
'''
(d/'Caddyfile').write_text(caddy)
clog=open(d/'caddy.log','w')
c=subprocess.Popen(['caddy','run','--config',str(d/'Caddyfile'),'--adapter','caddyfile'],stdout=clog,stderr=clog,start_new_session=True)
(d/'caddy.pid').write_text(str(c.pid))
print(d)
