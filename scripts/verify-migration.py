from pathlib import Path
import hashlib,json
root=Path(__file__).resolve().parents[1]
m=json.loads((root/'MIGRATION.json').read_text())
# Destinations whose re-pin reason is carried by the packaging_changes record.
packaged=set(m.get('packaging_changes',[]))
repinned=0
removed=0
for entry in m['files']:
    p=root/entry['destination']
    if entry.get('removed'):
        # A recorded removal is only valid while the bytes stay gone.
        assert not p.exists(), f'Recorded removal no longer holds: {p}'
        assert entry.get('extracted_sha256') is None, f'Removed entry must not carry a content hash: {p}'
        assert entry.get('reason') or entry['destination'] in packaged, f'Removed entry needs a reason: {p}'
        removed+=1
        continue
    actual=hashlib.sha256(p.read_bytes()).hexdigest()
    assert actual==entry.get('extracted_sha256',entry['sha256']), f'Migration mismatch: {p}'
    if actual!=entry['sha256']:
        assert entry.get('reason') or entry['destination'] in packaged, f'Re-pinned entry needs a reason: {p}'
        repinned+=1
print('Migration checksums verified:',len(m['files']),'files')
print('Re-pinned against the extraction revision:',repinned,'entries')
print('Recorded removals confirmed absent:',removed,'entries')