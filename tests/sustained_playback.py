#!/usr/bin/env python3
"""Real-time rolling-HLS soak. Starts one opaque source and decodes every segment."""
import argparse,json,re,subprocess,threading,time,urllib.error,urllib.parse,urllib.request
from pathlib import Path


def request(url, device_token=None, method='GET', body=None, timeout=75):
    data=None if body is None else json.dumps(body).encode()
    headers={'User-Agent':'VIPTV-Sustained-Test/1.6'}
    if device_token is not None: headers['Authorization']='Bearer '+device_token
    if data is not None: headers['Content-Type']='application/json'
    with urllib.request.urlopen(urllib.request.Request(url,data=data,headers=headers,method=method),timeout=timeout) as response:
        return response.read()

def json_request(url, device_token, method='GET', body=None, timeout=75):
    return json.loads(request(url,device_token,method,body,timeout))

def media_playlist(url):
    text=request(url,timeout=10).decode('utf-8')
    if '#EXT-X-STREAM-INF' in text:
        choices=[line.strip() for line in text.splitlines() if line.strip() and not line.startswith('#') and line.strip().endswith('.m3u8')]
        assert choices,'Master playlist has no media rendition'
        url=urllib.parse.urljoin(url,choices[0]); text=request(url,timeout=10).decode('utf-8')
    return url,text

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--base',required=True,help='VIPTV API origin')
    parser.add_argument('--device-token-file',required=True,type=Path,
                        help='owner-only file containing a paired-device access token')
    stream=parser.add_mutually_exclusive_group(required=True)
    stream.add_argument('--stream-id')
    stream.add_argument('--stream-id-file',type=Path)
    parser.add_argument('--expected-seconds',required=True,type=float)
    parser.add_argument('--force-transcode',action='store_true')
    parser.add_argument('--expect-mode',choices=['remux','transcode'])
    parser.add_argument('--ffmpeg',default='ffmpeg')
    args=parser.parse_args()
    token=args.device_token_file.read_text().strip(); assert token and len(token)<=4096
    stream_id=args.stream_id_file.read_text().strip() if args.stream_id_file else args.stream_id
    assert stream_id and len(stream_id)<=512
    base=args.base.rstrip('/'); assert urllib.parse.urlsplit(base).scheme in ('http','https')
    started=time.monotonic(); session=None; decoder=None; stop=threading.Event(); audit_error=[]; report={}
    try:
        session=json_request(base+'/api/playback',token,'POST',{'stream_id':stream_id,'position':0,'force_transcode':args.force_transcode,'capabilities':{'max_width':1280,'max_height':720,'h264':True,'hevc':False,'aac':True}},90)
        if args.expect_mode: assert session['mode']==args.expect_mode,session['mode']
        playlist_url,initial=media_playlist(urllib.parse.urljoin(base+'/',session['url']))
        first_ready=time.monotonic()-started
        sequences=set(); target_durations=set(); fetched_bytes=0; last_new=time.monotonic(); last_heartbeat=0.; endlist=False
        def audit():
            nonlocal fetched_bytes,last_new,last_heartbeat,endlist
            try:
                while not stop.is_set():
                    now=time.monotonic()
                    if now-last_heartbeat>=15:
                        request(base+'/api/playback/'+urllib.parse.quote(session['id'])+'/heartbeat',token,'POST',timeout=10); last_heartbeat=now
                    _,text=media_playlist(playlist_url)
                    target=re.search(r'^#EXT-X-TARGETDURATION:(\d+)$',text,re.M)
                    if target: target_durations.add(int(target.group(1)))
                    media_sequence=re.search(r'^#EXT-X-MEDIA-SEQUENCE:(\d+)$',text,re.M)
                    sequence=int(media_sequence.group(1)) if media_sequence else 0
                    names=[line.strip() for line in text.splitlines() if line.strip() and not line.startswith('#') and line.strip().endswith('.ts')]
                    for index,name in enumerate(names):
                        match=re.search(r'(\d+)(?=\.ts$)',name)
                        number=int(match.group(1)) if match else sequence+index
                        if number not in sequences:
                            data=request(urllib.parse.urljoin(playlist_url,name),timeout=10)
                            assert len(data)>=188 and data[0]==0x47,(name,len(data))
                            sequences.add(number); fetched_bytes+=len(data); last_new=now
                    endlist='#EXT-X-ENDLIST' in text
                    elapsed=now-started
                    if not endlist and elapsed>20 and args.expected_seconds-elapsed>15 and now-last_new>8:
                        raise AssertionError(f'Playlist made no segment progress for {now-last_new:.1f}s')
                    if endlist: return
                    stop.wait(.25)
            except Exception as error:
                audit_error.append(repr(error)); stop.set()
        thread=threading.Thread(target=audit,daemon=True); thread.start()
        decoder=subprocess.Popen([args.ffmpeg,'-hide_banner','-loglevel','error','-xerror','-nostdin','-i',playlist_url,'-map','0:v:0','-map','0:a:0?','-progress','pipe:1','-nostats','-f','null','-'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        decoded_us=0
        assert decoder.stdout is not None
        for line in decoder.stdout:
            if line.startswith('out_time_us='):
                try: decoded_us=max(decoded_us,int(line.partition('=')[2]))
                except ValueError: pass
            if stop.is_set() and audit_error: break
        if audit_error:
            decoder.kill()
        stdout,stderr=decoder.communicate(timeout=30)
        stop.set(); thread.join(timeout=15)
        assert not thread.is_alive(),'Playlist auditor did not stop'
        assert not audit_error,audit_error
        assert decoder.returncode==0,stderr[-2000:]
        elapsed=time.monotonic()-started
        decoded=decoded_us/1_000_000
        assert elapsed>=args.expected_seconds*.95,(elapsed,args.expected_seconds)
        assert decoded>=args.expected_seconds-10,(decoded,args.expected_seconds)
        deadline=time.monotonic()+30
        while not endlist and time.monotonic()<deadline:
            _,text=media_playlist(playlist_url); endlist='#EXT-X-ENDLIST' in text; time.sleep(.5)
        assert endlist,'Natural EOF never published ENDLIST'
        ordered=sorted(sequences); assert ordered and ordered==list(range(ordered[0],ordered[-1]+1)),(ordered[:3],ordered[-3:],len(ordered))
        assert len(target_durations)==1,target_durations
        report={'ok':True,'mode':session['mode'],'first_ready_seconds':round(first_ready,3),'wall_seconds':round(elapsed,3),'decoded_seconds':round(decoded,3),'segments_fetched':len(sequences),'segment_bytes':fetched_bytes,'target_duration':next(iter(target_durations)),'natural_endlist':True}
    finally:
        stop.set()
        if decoder is not None and decoder.poll() is None:
            decoder.kill(); decoder.wait(timeout=15)
        if session is not None:
            session_path=base+'/api/playback/'+urllib.parse.quote(session['id'])
            try:
                request(session_path,token,'DELETE',timeout=20)
            except urllib.error.HTTPError as error:
                if error.code != 404 or report.get('ok'):
                    raise
            if report.get('ok'):
                try:
                    request(session_path+'/heartbeat',token,'POST',timeout=10)
                except urllib.error.HTTPError as error:
                    assert error.code in (401,404),error.code
                else:
                    raise AssertionError('Deleted playback session still accepted a heartbeat')
    print(json.dumps(report,sort_keys=True))

if __name__=='__main__': main()
