#!/usr/bin/env python3
"""Own an isolated real backend/addon/TLS fixture for native lifecycle acceptance.

All input configuration is synthetic and private. No production imports, trust
modification, fulfilled API mocks, shared runtime or automatic deployment.
"""
import argparse, hashlib, http.client, http.server, json, os, pathlib, re, signal, socket, socketserver, sqlite3, ssl, subprocess, sys, threading, time, urllib.error, urllib.parse, urllib.request

P=argparse.ArgumentParser(description=__doc__)
for name in ['previous-fixture','gateway-source','gateway-private','server','certificate','key','ca','artifacts']:
    P.add_argument('--'+name,type=pathlib.Path,required=True)
P.add_argument('--backend-port',type=int,default=18195)
P.add_argument('--addon-port',type=int,default=18094)
P.add_argument('--tls-port',type=int,default=18445)
P.add_argument('--media-port',type=int,default=18444)
P.add_argument('--isolated-address',required=True)
P.add_argument('--emulator-host',required=True)
P.add_argument('--inner',action='store_true')
A=P.parse_args(); R=A.artifacts.resolve()
class Quiet(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def respond(self,status,data,headers=None):
        self.send_response(status)
        for k,v in (headers or {}).items(): self.send_header(k,v)
        # HTTP/1.0 handlers close after every response. State that explicitly:
        # otherwise an immediate native write can race a silently closed socket.
        self.send_header('Connection','close'); self.close_connection=True
        self.send_header('Content-Length',str(len(data))); self.end_headers()
        if self.command!='HEAD': self.wfile.write(data)
    def value(self,value,status=200): self.respond(status,json.dumps(value).encode(),{'Content-Type':'application/json'})
class UnixHTTP(http.client.HTTPConnection):
    def connect(self):
        self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); self.sock.settimeout(40); self.sock.connect(str(R/'backend.sock'))
def upstream(method,path,body=b'',headers=None):
    c=UnixHTTP('localhost',timeout=40); c.request(method,path,body,headers or {}); response=c.getresponse(); data=response.read(); status=response.status; items=response.getheaders(); result=dict(items)
    cookies=[v for k,v in items if k.lower()=='set-cookie']
    if cookies: result['set-cookie']=','.join(cookies)
    c.close(); return status,data,result
def save(name,value):
    p=R/name; p.write_text(json.dumps(value,indent=2)); p.chmod(0o600)
def api(path,method='GET',body=None,token=None):
    headers={'Content-Type':'application/json','Origin':origin}
    if token: headers['Authorization']='Bearer '+token
    status,data,_=upstream(method,path,b'' if body is None else json.dumps(body).encode(),headers)
    if status>=400:
        save('operation-failure.json',{'path':path,'status':status,'response':json.loads(data)})
        raise RuntimeError('Actual API rejected '+path+' status '+str(status)+'; private operation-failure.json')
    return json.loads(data) if data else None
class Addon(Quiet):
    def do_GET(self):
        source=json.loads((R/'source.json').read_text())
        meta={'id':'tt1234567','type':'movie','name':'Native gateway fixture','description':'Controlled120 second real gateway output','runtime':'2','year':2020}
        if self.path=='/manifest.json': self.value({'id':'native.fixture','version':'1.0.0','name':'Native gateway fixture','resources':['catalog','meta','stream'],'types':['movie'],'idPrefixes':['tt'],'catalogs':[{'id':'native','type':'movie','name':'Native qualification'}]})
        elif self.path.startswith('/catalog/movie/native'): self.value({'metas':[meta]})
        elif self.path=='/meta/movie/tt1234567.json': self.value({'meta':meta})
        elif self.path=='/stream/movie/tt1234567.json': self.value({'streams':[{'name':'Native gateway fixture','title':'H264 AAC120 seconds','url':source['url'],'behaviorHints':{'filename':'native-gateway-fixture.ts','proxyHeaders':{'request':source['headers']}}}]})
        else: self.value({'error':'unknown_fixture_resource'},404)
class Relay(Quiet):
    def do_GET(self): self.forward()
    def do_POST(self): self.forward()
    def do_PUT(self): self.forward()
    def do_PATCH(self): self.forward()
    def do_DELETE(self): self.forward()
    def forward(self):
        body=self.rfile.read(int(self.headers.get('Content-Length',0)))
        c=http.client.HTTPConnection('127.0.0.1',A.backend_port,timeout=40); c.request(self.command,self.path,body,{k:v for k,v in self.headers.items() if k.lower() not in ('host','connection')})
        response=c.getresponse(); data=response.read(); items=response.getheaders(); headers={k:v for k,v in items if k.lower() not in ('transfer-encoding','connection','content-length')}; cookies=[v for k,v in items if k.lower()=='set-cookie']
        if cookies: headers['set-cookie']=','.join(cookies)
        self.respond(response.status,data,headers); c.close()
class UnixServer(socketserver.ThreadingMixIn,socketserver.UnixStreamServer): daemon_threads=True
if A.inner:
    assert pathlib.Path('/proc/self/ns/net').readlink()!=pathlib.Path('/proc/1/ns/net').readlink()
    assert [name for _,name in socket.if_nameindex()]==['lo']
    addon=http.server.ThreadingHTTPServer((A.isolated_address,A.addon_port),Addon); threading.Thread(target=addon.serve_forever,daemon=True).start()
    relay=UnixServer(str(R/'backend.sock'),Relay); os.chmod(R/'backend.sock',0o666); relay.serve_forever(); sys.exit()

os.umask(0o077); R.mkdir(mode=0o700,parents=True,exist_ok=False)
old=json.loads((A.previous_fixture/'fixture.json').read_text()); source=json.loads(A.gateway_source.read_text()); private=json.loads(A.gateway_private.read_text())
assert old['keyring']['active']=='synthetic' and set(old['keyring']['keys'])=={'synthetic'}, 'requires synthetic seed keyring marker'
assert old.get('extra_providers') in (0,205) and re.fullmatch(r'[0-9a-f]{40}',old.get('revision','')), 'requires generated synthetic seed configuration'
assert urllib.parse.urlsplit(old['origin']).hostname=='viptv.local.test' and isinstance(old['password'],str), 'requires local synthetic seed configuration'
# The backend's real source-header whitelist accepts this explicit API header.
source['headers']={'X-API-Key':next(iter(source['headers'].values()))}; save('source.json',source)
origin=f'https://{A.emulator_host}:{A.tls_port}'; host_origin=f'https://127.0.0.1:{A.tls_port}'
database=R/'native.sqlite'
with sqlite3.connect('file:'+str(A.previous_fixture/'populated.sqlite')+'?mode=ro',uri=True) as original, sqlite3.connect(database) as clone:
    assert original.execute('SELECT id,username FROM auth_accounts ORDER BY id').fetchall()==[(1,'qa_operator'),(2,'qa_member'),(3,'qa_foreign')], 'refusing non-synthetic accounts before backup'
    assert original.execute('SELECT id,name,avatar_seed FROM profiles ORDER BY id').fetchall()==[(n,f'Synthetic profile {n}',f'fixture-{n}') for n in (1,2,3)], 'requires untouched synthetic profile seed'
    assert original.execute("SELECT count(*) FROM providers WHERE name NOT LIKE 'Synthetic provider %'").fetchone()[0]==0, 'refusing non-synthetic providers before backup'
    assert original.execute("SELECT count(*) FROM playback_gateways WHERE id='synthetic-operator-gateway'").fetchone()[0]==1, 'requires synthetic gateway schema marker'
    original.backup(clone)
    clone.execute('UPDATE addons SET enabled=0'); clone.execute('UPDATE providers SET enabled=0,enable_live=0,enable_movies=0,enable_series=0'); clone.execute('DELETE FROM auth_sessions'); clone.execute('DELETE FROM auth_pairings'); clone.commit()
namespace_pid=subprocess.check_output(['sudo','docker','inspect',private['container_id'],'--format','{{.State.Pid}}'],text=True).strip()
assert namespace_pid.isdigit() and int(namespace_pid)>1
env=dict(os.environ,VIPTV_DATABASE=str(database),VIPTV_AUTH_ORIGIN=origin,VIPTV_SECRETS_KEYRING=json.dumps(old['keyring']),VIPTV_BIND=f'127.0.0.1:{A.backend_port}',VIPTV_DASHBOARD_DIST='',VIPTV_TV_DIST='')
processes=[]; stopping=threading.Event(); servers=[]
signal.signal(signal.SIGTERM,lambda *_:stopping.set()); signal.signal(signal.SIGINT,lambda *_:stopping.set())
controls={'identityCalls':0,'pairingStarts':0,'refreshCalls':0,'successfulRefreshes':0,'delayIdentity':0,'delayRefreshResponse':0,'offlineIdentity':False,'failIdentityOnce401':False,'rejectRefresh401':False}; lock=threading.Lock(); approval=None; latest_code=None
class Proxy(Quiet):
    def do_GET(self): self.forward()
    def do_POST(self): self.forward()
    def do_PUT(self): self.forward()
    def do_PATCH(self): self.forward()
    def do_DELETE(self): self.forward()
    def forward(self):
        global latest_code
        body=self.rfile.read(int(self.headers.get('Content-Length',0)))
        if self.path=='/__control':
            if self.command=='POST':
                value=json.loads(body or b'{}')
                valid=isinstance(value,dict) and set(value)<= {'delayIdentity','delayRefreshResponse','offlineIdentity','failIdentityOnce401','rejectRefresh401','approvePairing'}
                if valid:
                    for k,v in value.items():
                        valid=valid and (type(v) is int and 0<=v<=40000 if k in ('delayIdentity','delayRefreshResponse') else type(v) is bool)
                if not valid:
                    self.value({'error':'invalid_fixture_control'},400); return
                with lock:
                    for k in ['delayIdentity','delayRefreshResponse','offlineIdentity','failIdentityOnce401','rejectRefresh401']:
                        if k in value: controls[k]=value[k]
                if value.get('approvePairing'):
                    if latest_code is None:
                        self.value({'error':'no_pending_actual_pairing'},409); return
                    # Long native qualification can outlive the initial browser
                    # session. Approve with a fresh actual login, not stale auth.
                    status,data,login_headers=upstream('POST','/api/auth/login',json.dumps({'username':'qa_member','password':old['password']}).encode(),{'Content-Type':'application/json','Origin':origin})
                    if status!=200:
                        self.value({'error':'actual_approval_login_failed','status':status},502); return
                    fresh_approval=login_headers['set-cookie'].split('viptv_session=',1)[1].split(';',1)[0]
                    csrf=api('/api/auth/me',token=fresh_approval)['csrf_token']; status,data,_=upstream('POST','/api/auth/device/approve',json.dumps({'user_code':latest_code}).encode(),{'Content-Type':'application/json','Origin':origin,'Authorization':'Bearer '+fresh_approval,'x-csrf-token':csrf})
                    if status!=200:
                        self.value({'error':'actual_pairing_approval_failed','status':status},502); return
            with lock: self.value(dict(controls))
            return
        headers={k:v for k,v in self.headers.items() if k.lower() not in ('host','connection','content-length')}
        identity=self.path=='/api/auth/me'; pairing=self.path=='/api/auth/device/code'; refresh_call=self.path=='/api/auth/device/refresh'
        with lock:
            if identity: controls['identityCalls']+=1
            if pairing: controls['pairingStarts']+=1
            if refresh_call: controls['refreshCalls']+=1
            delay=controls['delayIdentity'] if identity else 0; offline=identity and controls['offlineIdentity']; reject=identity and controls['failIdentityOnce401']; refresh=self.path=='/api/auth/device/refresh' and controls['rejectRefresh401']
            refresh_delay=controls['delayRefreshResponse'] if refresh_call else 0
            if reject: controls['failIdentityOnce401']=False
        if delay: time.sleep(min(float(delay)/1000,40))
        if offline:
            self.close_connection=True; self.connection.shutdown(socket.SHUT_RDWR); self.connection.close(); return
        if reject: headers['Authorization']='Bearer native-fixture-invalid-access'
        if refresh:
            value=json.loads(body); value['refresh_token']='native-fixture-invalid-refresh'; body=json.dumps(value).encode()
        status,data,response_headers=upstream(self.command,self.path,body,headers)
        if refresh_call and status==200:
            with lock: controls['successfulRefreshes']+=1
        # The real server has already rotated its tokens. Hold its actual bytes
        # privately to exercise native cancellation after server-side commit.
        if refresh_delay: time.sleep(refresh_delay/1000)
        if pairing and status==200: latest_code=json.loads(data)['user_code']
        with (R/'safe-requests.jsonl').open('a') as log: log.write(json.dumps({'path':urllib.parse.urlsplit(self.path).path,'method':self.command,'status':status})+'\n')
        try: self.respond(status,data,{k:v for k,v in response_headers.items() if k.lower() not in ('content-length','transfer-encoding','connection')})
        except (BrokenPipeError,ConnectionResetError,ssl.SSLError): pass
class Media(http.server.SimpleHTTPRequestHandler):
    def __init__(self,*args,**kw): super().__init__(*args,directory=str(A.gateway_source.parent/'materialized'),**kw)
    def log_message(self,*args): pass
    def do_GET(self):
        if self.headers.get('X-API-Key')!=next(iter(source['headers'].values())): self.send_error(403); return
        super().do_GET()
    def do_HEAD(self):
        if self.headers.get('X-API-Key')!=next(iter(source['headers'].values())): self.send_error(403); return
        super().do_HEAD()
def tls_server(port,handler):
    server=http.server.ThreadingHTTPServer(('127.0.0.1',port),handler); ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain(A.certificate,A.key); server.socket=ctx.wrap_socket(server.socket,server_side=True); threading.Thread(target=server.serve_forever,daemon=True).start(); servers.append(server)
try:
    for argv,name in [(['sudo','--preserve-env=VIPTV_DATABASE,VIPTV_AUTH_ORIGIN,VIPTV_SECRETS_KEYRING,VIPTV_BIND,VIPTV_DASHBOARD_DIST,VIPTV_TV_DIST','nsenter','--target',namespace_pid,'--net','setpriv',f'--reuid={os.getuid()}',f'--regid={os.getgid()}','--clear-groups',str(A.server.resolve())],'backend.log'),(['sudo','nsenter','--target',namespace_pid,'--net','python3',str(pathlib.Path(__file__).resolve()),*sys.argv[1:],'--inner'],'addon-relay.log')]:
        processes.append(subprocess.Popen(argv,env=env,stdin=subprocess.DEVNULL,stdout=(R/name).open('wb'),stderr=subprocess.STDOUT,start_new_session=True))
    save('owned.json',{'owner_pid':os.getpid(),'process_pids':[p.pid for p in processes],'process_groups':[p.pid for p in processes],'gateway_container_id':private['container_id']})
    for _ in range(100):
        try:
            if api('/api/health'): break
        except Exception: time.sleep(.1)
    else: raise RuntimeError('actual backend readiness deadline')
    tls_server(A.tls_port,Proxy); tls_server(A.media_port,Media)
    trust=ssl.create_default_context(cafile=str(A.ca))
    with urllib.request.urlopen(host_origin+'/api/health',context=trust,timeout=10) as ready:
        assert ready.status==200
    try:
        urllib.request.urlopen(f'https://127.0.0.1:{A.media_port}/index.m3u8',context=trust,timeout=10)
        raise AssertionError('required media header was not enforced')
    except urllib.error.HTTPError as denied:
        assert denied.code==403
    # Browser session tokens arrive in cookies, never a synthetic DB token.
    # Login is repeated to capture the real Set-Cookie value at this boundary.
    status,data,headers=upstream('POST','/api/auth/login',json.dumps({'username':'qa_member','password':old['password']}).encode(),{'Content-Type':'application/json','Origin':origin})
    approval=headers['set-cookie'].split('viptv_session=',1)[1].split(';',1)[0]
    csrf=api('/api/auth/me',token=approval)['csrf_token']
    status,data,_=upstream('POST','/api/v2/addons',json.dumps({'manifest_url':f'http://{A.isolated_address}:{A.addon_port}/manifest.json'}).encode(),{'Content-Type':'application/json','Authorization':'Bearer '+approval,'x-csrf-token':csrf})
    assert status==200,'actual encrypted addon registration rejected; inspect private addon log'
    addon=json.loads(data); assert addon['credentials_encrypted']
    status,data,_=upstream('POST','/api/profiles',json.dumps({'name':'Foreground alternate','avatar_style':'critters'}).encode(),{'Content-Type':'application/json','Authorization':'Bearer '+approval,'x-csrf-token':csrf})
    assert status==200, 'actual alternate profile creation failed'
    alternate=json.loads(data); assert alternate['setup_complete']
    login=api('/api/auth/device/login','POST',{'username':'qa_member','password':old['password'],'device_name':'Native fixture preflight'}); token=login['access_token']; api('/api/auth/profile','POST',{'profile_id':2},token)
    catalog=api('/api/catalogs',token=token); metadata=api('/api/meta/movie/tt1234567',token=token)
    job=api('/api/v2/streams','POST',{'type':'movie','id':'tt1234567','only_addons':True},token)
    source_card=None
    for _ in range(100):
        poll=api('/api/v2/streams/'+job['id'],token=token)
        for event in poll['events']:
            if event.get('streams'): source_card=event['streams'][0]
        if source_card: break
        time.sleep(.1)
    assert source_card and len(source_card['source_fingerprint'])==64
    progress={'id':'tt1234567','type':'movie','name':'Native gateway fixture','position':20,'duration':120,'source_addon_id':source_card['source_addon_id'],'source_fingerprint':source_card['source_fingerprint'],'source_name':source_card['source_name']}; api('/api/profiles/2/progress','PUT',progress,token)
    playback=api('/api/v2/playback','POST',{'request_id':'native-fixture-preflight','stream_id':source_card['id'],'position':20,'client':{'platform':'android','can_play_direct':True,'max_width':1920,'max_height':1080,'video_codecs':['h264'],'audio_codecs':['aac']}},token)
    for _ in range(100):
        if playback['status']!='starting': break
        time.sleep(.1); playback=api('/api/v2/playback/'+playback['id'],token=token)
    save('preflight-playback-private.json',playback)
    assert playback['delivery']['kind']=='direct' and playback['delivery']['position']==20 and playback['delivery']['headers']
    api('/api/v2/playback/'+playback['id']+'/heartbeat','POST',{},token); api('/api/v2/playback/'+playback['id'],'DELETE',token=token)
    save('native.json',{'origin':origin,'host_origin':host_origin,'control_origin':host_origin,'ca':str(A.ca.resolve()),'username':'qa_member','password':old['password'],'profile_id':2,'profile_name':'Synthetic profile 2','media_id':'tt1234567','alternate_profile_id':alternate['id'],'alternate_profile_name':alternate['name']})
    save('evidence.json',{'backend_sha256':hashlib.sha256(A.server.read_bytes()).hexdigest(),'native_login':True,'profile':2,'encrypted_addon':True,'catalog':True,'metadata':metadata['meta']['id'],'source_fingerprint':source_card['source_fingerprint'],'direct_headers_present':True,'preflight_position':20,'preflight_heartbeat_release':True,'separate_gateway_lease':True})
    print('READY actual backend native fixture; private native.json and sanitized evidence.json in '+str(R),flush=True)
    stopping.wait()
finally:
    for server in servers: server.shutdown(); server.server_close()
    for process in reversed(processes):
        # Each owned sudo/nsenter tree has its own session; stop the actual
        # service/relay too, rather than leaving orphaned grandchildren.
        subprocess.run(['sudo','-n','kill','-TERM','--',str(-process.pid)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            process.poll()  # Reap the wrapper independently of its descendants.
            remaining=subprocess.run(['sudo','-n','kill','-0','--',str(-process.pid)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            if remaining.returncode!=0: break
            time.sleep(.1)
        else:
            subprocess.run(['sudo','-n','kill','-KILL','--',str(-process.pid)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        process.wait(timeout=5)
        # Do not report successful cleanup merely because sudo has exited.
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            remaining=subprocess.run(['sudo','-n','kill','-0','--',str(-process.pid)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            if remaining.returncode!=0: break
            time.sleep(.1)
        else:
            raise RuntimeError('Owned process group survived teardown: '+str(process.pid))
    print('Exact owned backend/addon/TLS processes stopped; separate gateway lease remains fixture-owned.',flush=True)
