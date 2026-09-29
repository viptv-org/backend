"""Reproducible in-memory VOD listing baseline; never opens a real database."""
import argparse
import json
import sqlite3
import statistics
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--sizes', type=int, nargs='+', default=[10_000, 100_000])
parser.add_argument('--iterations', type=int, default=5)
args = parser.parse_args()
assert all(1 <= size <= 1_000_000 for size in args.sizes)
assert 1 <= args.iterations <= 20

for size in args.sizes:
    db = sqlite3.connect(':memory:')
    db.executescript('''
      CREATE TABLE providers(id INTEGER PRIMARY KEY, enabled INTEGER, enable_movies INTEGER, enable_series INTEGER);
      CREATE TABLE provider_vod(id TEXT PRIMARY KEY,provider_id INTEGER,stream_id TEXT,kind TEXT,name TEXT,normalized TEXT,year INTEGER,imdb_id TEXT,tmdb_id TEXT,extension TEXT,poster TEXT);
      CREATE INDEX provider_vod_provider ON provider_vod(provider_id);
      CREATE TABLE provider_matches(vod_id TEXT PRIMARY KEY,metadata_id TEXT,kind TEXT);
      INSERT INTO providers VALUES(1,1,1,1),(2,1,1,1),(3,1,1,1);
    ''')
    db.executemany('INSERT INTO provider_vod VALUES(?,?,?,?,?,?,?,?,?,?,?)', (
        (f'iptv:{i%3+1}:movie:{i:08}',i%3+1,str(i),'movie',f'Fixture title {i}',f'fixture title {i}',2000+i%25,None if i%3==0 else f'tt{i:08}',None,'mp4',None)
        for i in range(size)))
    db.commit()
    query = '''SELECT v.id,v.provider_id,v.stream_id,v.kind,v.name,v.normalized,v.year,v.imdb_id,v.tmdb_id,v.extension,v.poster,m.metadata_id
      FROM provider_vod v JOIN providers p ON p.id=v.provider_id
      LEFT JOIN provider_matches m ON m.vod_id=v.id AND m.kind=v.kind
      WHERE p.enabled=1 AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1))
      ORDER BY v.provider_id,v.id'''
    samples=[]
    for _ in range(args.iterations):
        start=time.perf_counter()
        candidates=db.execute(query).fetchall()
        rows=[{'vod_id':r[0],'provider_id':r[1],'type':r[3],'name':r[4],'year':r[6],'poster':r[10]} for r in candidates if r[7] is None and r[8] is None and r[11] is None]
        data=json.dumps(rows,separators=(',',':')).encode()
        samples.append((time.perf_counter()-start)*1000)
    print(json.dumps({'implementation':'legacy-full-materialization','fixture_rows':size,'materialized_rows':len(candidates),'returned_rows':len(rows),'response_bytes':len(data),'median_ms':round(statistics.median(samples),2),'query_plan':[r[3] for r in db.execute('EXPLAIN QUERY PLAN '+query)]}))
    db.executescript('''
      CREATE TABLE provider_ownership(provider_id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL);
      CREATE INDEX provider_ownership_account ON provider_ownership(account_id,provider_id);
      INSERT INTO provider_ownership VALUES(1,11),(2,11),(3,11);
      CREATE INDEX provider_vod_unmatched_page ON provider_vod(provider_id,id) WHERE imdb_id IS NULL AND tmdb_id IS NULL;
      CREATE VIRTUAL TABLE provider_vod_search_v2 USING fts5(name,content='provider_vod',content_rowid='rowid');
      INSERT INTO provider_vod_search_v2(provider_vod_search_v2) VALUES('rebuild');
    ''')
    # The exact production v2 SQL, not a benchmark-only simplified query.
    query=(Path(__file__).resolve().parents[1]/'server/src/provider/matches_page_v2.sql').read_text()
    parameters=(11,None,None,0,'','',51)
    samples=[]
    for _ in range(args.iterations):
        start=time.perf_counter()
        candidates=db.execute(query,parameters).fetchall()
        rows=[dict(zip(['vod_id','provider_id','type','name','year','poster'],row)) for row in candidates[:50]]
        data=json.dumps(rows,separators=(',',':')).encode()
        samples.append((time.perf_counter()-start)*1000)
    print(json.dumps({'implementation':'v2-bounded-first-page','fixture_rows':size,'materialized_rows':len(candidates),'returned_rows':len(rows),'response_items_bytes':len(data),'median_ms':round(statistics.median(samples),2),'query_plan':[r[3] for r in db.execute('EXPLAIN QUERY PLAN '+query,parameters)]}))
    db.close()
