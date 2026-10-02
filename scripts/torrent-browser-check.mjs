// Real App/auth/addon/gateway/browser; no account/catalog/playback route mocking.
const {chromium}=await import(process.env.TV_WEB_ROOT+'/node_modules/playwright/index.mjs');
import {readFileSync,writeFileSync} from 'node:fs';
import assert from 'node:assert/strict';
const q=process.env.QUALIFICATION_DIR+'/',origin=process.env.VIPTV_TEST_BROWSER_ORIGIN;
const gw=JSON.parse(readFileSync(q+'fixture.json'));
const browser=await chromium.launch({args:['--autoplay-policy=no-user-gesture-required']});
const context=await browser.newContext({viewport:{width:1440,height:900},ignoreHTTPSErrors:false});
const page=await context.newPage();
const requests=[],responses=[],failures=[];
let duration;
page.on('request',request=>{
 const p=new URL(request.url()).pathname;
 if(p==='/api/v2/playback'&&request.method()==='POST')requests.push({op:'create',position:JSON.parse(request.postData()).position});
 else if(p.startsWith('/api/v2/playback/')&&request.method()==='DELETE')requests.push({op:'release'});
 else if(p.endsWith('/heartbeat')&&p.startsWith('/api/v2/playback/'))requests.push({op:'renew'});
});
page.on('response',async response=>{
 const p=new URL(response.url()).pathname;
 if(p.startsWith('/api/')){
  responses.push({kind:p.startsWith('/api/v2/playback')?'playback':p.startsWith('/api/v2/streams')?'discovery':'account/catalog',status:response.status()});
  const text=await response.text().catch(()=>'');
  if(text.includes(gw.source)||text.includes(gw.key))failures.push('public API exposed private source or integration material');
  try{const v=JSON.parse(text);if(v.delivery?.kind==='gateway'&&Number.isFinite(v.delivery.duration))duration=v.delivery.duration;}catch{}
 }
 if(p.startsWith('/gateway/media/'))responses.push({kind:'gateway media',status:response.status()});
});
page.on('pageerror',()=>failures.push('page error'));
await page.addInitScript(({origin})=>localStorage.setItem('viptv-device:'+origin,JSON.stringify({sessionId:'s1',accountId:'1',profileId:'1',accessToken:'member-token-1',refreshToken:'fixture-refresh',expiresIn:3600})),{origin});
try {
 await page.goto(origin+'/tv/');
 await page.locator('.media-card').filter({hasText:'Generated torrent clip'}).first().click({timeout:30000});
 await page.locator('[data-focus-id="detail-source"]').click();
 await page.locator('[data-focus-id="source-0"]').click();
 await page.waitForFunction(()=>{const c=document.querySelector('.player-canvas');return c&&c.width===320&&c.getContext('2d').getImageData(0,0,c.width,c.height).data.some((v,i)=>i%4!==3&&v>20);},{},{timeout:60000});
 await page.locator('.vx-player__buffering').waitFor({state:'hidden',timeout:45000});
 if(await page.locator('[data-focus-id="pause"]').getAttribute('aria-label')==='Pause')await page.locator('[data-focus-id="pause"]').click();
 await page.waitForFunction(()=>document.querySelector('[data-focus-id="pause"]')?.getAttribute('aria-label')==='Play');
 await page.locator('[data-focus-id="rewind"]').click();await page.waitForTimeout(1000);
 await page.waitForFunction(()=>Number(document.querySelector('[data-focus-id="timeline"]')?.getAttribute('aria-valuenow'))===0);
 const capture=()=>page.evaluate(()=>{const c=document.querySelector('.player-canvas');return Array.from(c.getContext('2d').getImageData(0,0,c.width,c.height).data).filter((_,i)=>i%4!==3);});
 const first=await capture();
 assert(Number.isFinite(duration));
 const timeline=page.locator('[data-focus-id="timeline"]'),box=await timeline.boundingBox();
 for(const type of ['pointerdown','pointerup'])await timeline.dispatchEvent(type,{clientX:box.x+box.width*3/duration,clientY:box.y+box.height/2,pointerId:1,pointerType:'mouse',buttons:type==='pointerdown'?1:0});
 for(let i=0;i<900&&!requests.some(r=>r.op==='create'&&Math.abs(r.position-3)<0.01);i++)await page.waitForTimeout(50);
 assert(requests.some(r=>r.op==='create'&&Math.abs(r.position-3)<0.01),'real backend must receive three-second approved-source replacement');
 const reference=Array.from(readFileSync(q+'expected3.rgb'));
 const mse=(a,b)=>a.reduce((s,v,i)=>s+(v-b[i])**2,0)/a.length;
 let soughtMse;
 for(let i=0;i<900;i++){soughtMse=mse(await capture(),reference);if(soughtMse<150)break;await page.waitForTimeout(50);}
 const firstMse=mse(first,reference);assert(soughtMse<150&&soughtMse<firstMse*.1,'real-backend gateway browser seek must decode source at three seconds');
 await page.waitForTimeout(20500);
 await page.locator('[data-focus-id="player-back"]').click();
 await page.locator('.player-overlay').waitFor({state:'hidden'});
 assert(requests.some(r=>r.op==='renew')&&requests.filter(r=>r.op==='release').length>=2);
 assert.equal(failures.length,0);
 writeFileSync(q+'actual-browser-result.json',JSON.stringify({soughtMse,firstMse,requests,responses,failures},null,2));
 console.log('Actual backend account/catalog/approved-source to gateway HTTPS browser decoded seek and lease lifecycle passed.',{soughtMse,firstMse});
} catch(error){
 await page.screenshot({path:q+'browser-failure.png'}).catch(()=>{});
 console.log('Fixture browser state',await page.locator('body').innerText().catch(()=>''));throw error;
}finally{await browser.close();}
