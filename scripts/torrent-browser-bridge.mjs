// Generated-media producer and transport bridge only; product APIs remain real.
import http from 'node:http';
import {readFileSync,writeFileSync,symlinkSync,mkdtempSync} from 'node:fs';
const q=process.env.QUALIFICATION_DIR+'/';
if(!process.env.QUALIFICATION_DIR)throw Error('QUALIFICATION_DIR required');
const gw=JSON.parse(readFileSync(q+'fixture.json'));
if(process.env.PLAYBACK_TEST_MAGNET==='true'&&!/^[0-9a-f]{40}$/.test(gw.info_hash??''))throw Error('Compiled magnet fixture must provide generated infoHash');
const backendPort=Number(process.env.BACKEND_PORT),controlPort=Number(process.env.CONTROL_PORT),addonPort=Number(process.env.ADDON_PORT),origin=process.env.VIPTV_TEST_BROWSER_ORIGIN;
const short=mkdtempSync('/tmp/pg-full-');
symlinkSync(q+'gateway.sock',short+'/gateway.sock');
writeFileSync(q+'socket-alias',short);
http.createServer((req,res)=>{
 const upstream=http.request({socketPath:short+'/gateway.sock',path:req.url,method:req.method,headers:req.headers},r=>{res.writeHead(r.statusCode,r.headers);r.pipe(res);});
 upstream.on('error',()=>{res.writeHead(502);res.end();}); req.pipe(upstream);
}).listen(controlPort,'127.0.0.1');
const movie={id:'torrent-clip',type:'movie',name:'Generated torrent clip',description:'Locally generated media for isolated integration qualification.'};
http.createServer((req,res)=>{
 const path=new URL(req.url,'http://fixture').pathname;
 let value;
 if(path==='/manifest.json')value={id:'fixture.gateway.torrent',version:'1.0.0',name:'Generated torrent addon',description:'Synthetic qualification only',types:['movie'],resources:['catalog','meta','stream'],catalogs:[{type:'movie',id:'fixture',name:'Generated media'}]};
 else if(path.startsWith('/catalog/'))value={metas:[movie]};
 else if(path.startsWith('/meta/'))value={meta:movie};
 else if(path.startsWith('/stream/'))value={streams:[{name:'Generated torrent source',title:'H264 AAC fixture',...(process.env.PLAYBACK_TEST_MAGNET==='true'?{infoHash:gw.info_hash}:{url:gw.source}),fileIdx:0,behaviorHints:{filename:'clip.mkv',videoSize:gw.movie_size}}]};
 else{res.writeHead(404);res.end();return;}
 res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify(value));
}).listen(addonPort,'127.0.0.1');
writeFileSync(q+'backend.json',JSON.stringify({gateway_control:`http://127.0.0.1:${controlPort}/`,gateway_endpoint:origin+'/gateway/',gateway_key:gw.key,addon_manifest:`http://127.0.0.1:${addonPort}/manifest.json`,backend_bind:`127.0.0.1:${backendPort}`}),{mode:0o600});
console.log('Actual gateway control bridge and generated addon producer ready.');
