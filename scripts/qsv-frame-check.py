#!/usr/bin/env python3
"""GPU regression: a cropped 804px picture must not expose green padded rows.
Run on the GPU host with VIPTV_BASE_IMAGE. --filter scale_qsv reproduces the defect.
"""
import argparse, os, subprocess, tempfile
from pathlib import Path
parser=argparse.ArgumentParser();parser.add_argument('--filter',choices=['scale_qsv','vpp_qsv'],default='vpp_qsv');args=parser.parse_args()
image=os.environ.get('VIPTV_BASE_IMAGE','viptv:local')
device=os.environ.get('VIPTV_TEST_QSV_DEVICE','/dev/dri/renderD128')
with tempfile.TemporaryDirectory(prefix='viptv-qsv-pixels-') as folder:
 root=Path(folder);root.chmod(0o777)
 def ff(command):
  p=subprocess.run(['docker','run','--rm','--device',device,'--group-add',str(Path(device).stat().st_gid),'-v',str(root)+':/work','--entrypoint','ffmpeg',image,'-hide_banner','-loglevel','error',*command],capture_output=True,timeout=60)
  if p.returncode:raise RuntimeError(p.stderr.decode()[:1500])
  return p.stdout
 ff(['-f','lavfi','-i','color=c=gray:s=1920x804:r=24','-t','1','-c:v','libx264','-preset','ultrafast','-pix_fmt','yuv420p','-y','/work/in.mp4'])
 ff(['-init_hw_device',f'qsv=viptv,child_device={device}','-filter_hw_device','viptv','-hwaccel','qsv','-hwaccel_output_format','qsv','-c:v','h264_qsv','-i','/work/in.mp4','-vf',f'{args.filter}=w=1920:h=804:format=nv12','-c:v','h264_qsv','-look_ahead','0','-y','/work/out.mp4'])
 raw=ff(['-i','/work/out.mp4','-vf','crop=iw:8:0:ih-8,format=rgb24','-frames:v','1','-f','rawvideo','-'])
 assert len(raw)==1920*8*3,'Missing decoded pixels'
 green=sum(1 for i in range(0,len(raw),3) if raw[i+1]>raw[i]+30 and raw[i+1]>raw[i+2]+30)
 assert green==0,f'{green} green pixels in a neutral gray bottom edge'
 print('PASS: cropped QSV output has no green bottom pixels')
