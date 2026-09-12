"""Build the reviewed dashboard gitlink and refresh the deployable bundle."""
from pathlib import Path
import hashlib,json,subprocess,shutil
root=Path(__file__).resolve().parents[1];web=root/'dashboard'
assert (web/'package-lock.json').exists(), 'Initialize dashboard submodule first'
assert not subprocess.check_output(['git','status','--porcelain'],cwd=web,text=True).strip(), 'Web source must be clean'
sha=subprocess.check_output(['git','rev-parse','HEAD'],cwd=web,text=True).strip()
for args in [['npm','ci','--no-audit','--no-fund'],['npm','test','--','--run'],['npm','run','build']]: subprocess.run(args,cwd=web,check=True)
dst=root/'web-dist'
if dst.exists(): shutil.rmtree(dst)
shutil.copytree(web/'dist',dst)
manifest={'repository':'viptv-org/web','commit':sha,'files':{str(p.relative_to(dst)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(dst.rglob('*')) if p.is_file()}}
(root/'WEB_BUNDLE.json').write_text(json.dumps(manifest,indent=2)+'\n')
print('Review and commit dashboard, web-dist, and WEB_BUNDLE.json together:',sha)
