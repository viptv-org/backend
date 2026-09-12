from pathlib import Path
import hashlib,json
root=Path(__file__).resolve().parents[1]
m=json.loads((root/'MIGRATION.json').read_text())
for entry in m['files']:
    p=root/entry['destination']
    actual=hashlib.sha256(p.read_bytes()).hexdigest()
    assert actual==entry.get('extracted_sha256',entry['sha256']), f'Migration mismatch: {p}'
print('Migration checksums verified:',len(m['files']),'files')
