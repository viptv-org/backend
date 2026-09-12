from pathlib import Path
import hashlib,json,subprocess
root=Path(__file__).resolve().parents[1]
m=json.loads((root/'WEB_BUNDLE.json').read_text())
assert m['repository']=='viptv-org/web'
pin=subprocess.check_output(['git','ls-files','--stage','dashboard'],cwd=root,text=True).split()[1]
assert pin==m['commit'], 'Web bundle does not match the pinned source revision'
actual={str(p.relative_to(root/'web-dist')):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted((root/'web-dist').rglob('*')) if p.is_file()}
assert actual==m['files'], 'Web bundle files/checksums differ'
print('Pinned web bundle verified:',m['commit'])
