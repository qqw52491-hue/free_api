import { createServer } from 'vite';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import vue from '@vitejs/plugin-vue';
import { mkdtemp, readFile, writeFile, readdir } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
const root=fileURLToPath(new URL('../',import.meta.url));
// Run with Node >= 22 and Chrome; CHROME_BIN overrides the Puppeteer cache.
let executable=process.env.CHROME_BIN;
if(!executable){const cache=join(homedir(),'.cache/puppeteer/chrome');const versions=await readdir(cache);executable=join(cache,versions.sort().at(-1),'chrome-linux64/chrome');}
const profile=await mkdtemp(join(tmpdir(),'free-api-chat-ui-'));
const entry=`import {createApp,h,reactive} from 'vue';import Chat from '/src/components/agent/AgentChatStream.vue';
Object.defineProperty(navigator,'clipboard',{value:{writeText:async text=>{if(window.denyClipboard) throw Error('denied');window.copied=text}},configurable:true});
window.forks=[];window.copied=null;
const props=reactive({currentGoal:'',legacyMessages:[{id:'u1',role:'user',content:'长用户问题\\n'.repeat(100),created_at:'2026-11-05T05:38:32Z'},{id:'a1',role:'assistant',content:'正常正文\\n\\n---\\\\n\\n\\n## 后续章节\\n正文',created_at:'2026-11-05T05:39:00Z'}],historyTurns:[],steps:[],selectedStepId:null,selectedSubpart:null,selectedNarrative:null,thought:'',isThinking:false,humanInterventions:[],stateDescription:'',isRunning:false,completionMessage:'',finalSummary:'',finalArtifacts:[],hasError:false,onFork:p=>window.forks.push(p)});window.fixtureProps=props;createApp({render:()=>h(Chat,props)}).mount('#app');`;
const server=await createServer({root,configFile:false,plugins:[vue(),{name:'chat-test',resolveId(id){if(id==='virtual:chat-test')return '\0chat-test';},load(id){if(id==='\0chat-test')return entry;},configureServer(s){s.middlewares.use('/__chat_test',(_,res)=>{res.setHeader('Content-Type','text/html');res.end(`<style>html,body,#app{margin:0;height:100%;display:flex;flex-direction:column;width:100%}</style><div id="app"></div><script type="module" src="/@id/__x00__chat-test"></script>`);});}}],server:{host:'127.0.0.1',port:0},logLevel:'error'});
await server.listen();const port=server.httpServer.address().port;
const chrome=spawn(executable,['--headless=new','--no-sandbox','--disable-gpu','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:'ignore'});
let launchError;
chrome.on('error',error=>{launchError=error;});
let ws;
try {
 let debugPort;for(let i=0;i<150;i++){if(launchError)throw launchError;try{debugPort=(await readFile(join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0];break;}catch{}await new Promise(r=>setTimeout(r,50));}
 if(!debugPort)throw Error('Chrome did not start');
 const pages=await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();ws=new WebSocket(pages.find(x=>x.type==='page').webSocketDebuggerUrl);await new Promise(r=>ws.addEventListener('open',r,{once:true}));
 let seq=0;const pending=new Map();ws.addEventListener('message',e=>{let m=JSON.parse(e.data);if(m.id){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result);}});
 const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params}));});
 const evaluate=async expression=>{let r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(JSON.stringify(r.exceptionDetails));return r.result.value;};
 await call('Emulation.setFocusEmulationEnabled',{enabled:true});await call('Emulation.setDeviceMetricsOverride',{width:1000,height:700,deviceScaleFactor:1,mobile:false});await call('Page.navigate',{url:`http://127.0.0.1:${port}/__chat_test`});
 let ready=false;for(let i=0;i<200;i++){ready=await evaluate('!!document.querySelector(".saved-message .msg-copy-btn")');if(ready)break;await new Promise(r=>setTimeout(r,50));}if(!ready)throw Error('Chat did not mount');

 const mouse=async(type,x,y)=>call('Input.dispatchMouseEvent',{type,x,y,...(type==='mouseMoved'?{}:{button:'left',clickCount:1})});
 const waitFor=async expression=>{for(let i=0;i<100;i++){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,50));}throw Error('Timed out: '+expression);};
 const click=async selector=>{
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'center'})`);
  assert(await evaluate(`(()=>{let b=document.querySelector(${JSON.stringify(selector)}),m=b.closest('.from-user,.user-message'),bubble=m?.querySelector('.user-message-bubble');return !bubble||!b.closest('.msg-side-actions')||b.getBoundingClientRect().top>=bubble.getBoundingClientRect().bottom})()`),'actions stay below the painted bubble');
  const r=await evaluate(`(()=>{let b=document.querySelector(${JSON.stringify(selector)}),r=b.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
  await mouse('mouseMoved',r.x,Math.max(1,r.y-25));
  await mouse('mouseMoved',r.x,r.y);
  // The old disappearing toolbar fails here after the hover grace period.
  await new Promise(r=>setTimeout(r,700));
  assert(await evaluate(`document.elementFromPoint(${r.x},${r.y})?.closest('button,[role="button"]')===document.querySelector(${JSON.stringify(selector)})`),'button hit target: '+selector);
  await mouse('mousePressed',r.x,r.y);await mouse('mouseReleased',r.x,r.y);
 };
 const user='.saved-message.from-user';
 const copy=user+' .msg-copy-btn', fork=user+' .msg-fork-btn';
 await click(copy);await waitFor('window.copied!==null');
 assert.equal(await evaluate('window.copied'),'长用户问题\n'.repeat(100).trim());
 await click(fork);assert.equal(await evaluate('window.forks.length'),1);
 assert.equal(await evaluate('document.querySelectorAll(".markdown-content hr").length'),1);
 await click('.saved-message.from-agent .msg-copy-btn');
 assert((await evaluate('window.copied')).includes(String.raw`---\n`),'copy retains literal source escapes');
 await click(user+' .user-text-expandbar');
 await click(copy);assert((await evaluate('window.copied')).startsWith('长用户问题'));
 await evaluate('window.fixtureProps.isRunning=true');
 await waitFor(`document.querySelector(${JSON.stringify(fork)}).disabled`);
 assert.equal(await evaluate(`document.querySelector(${JSON.stringify(copy)}).disabled`),false);
 await evaluate('window.fixtureProps.isRunning=false');
 await call('Emulation.setDeviceMetricsOverride',{width:420,height:760,deviceScaleFactor:1,mobile:false});
 await evaluate("window.fixtureProps.legacyMessages[0].content='短消息'");
 await waitFor("document.querySelector('.user-text-content').textContent==='短消息'");
 await click(copy);assert.equal(await evaluate('window.copied'),'短消息');
 await click(fork);assert.equal(await evaluate('window.forks.length'),2);
 // Moving slowly through the visual gap must keep the footer interactive.
 await evaluate('document.activeElement?.blur()');
 const gap=await evaluate(`(()=>{let m=document.querySelector(${JSON.stringify(user)}),b=m.querySelector('.user-message-bubble').getBoundingClientRect(),a=m.querySelector('.msg-side-actions').getBoundingClientRect();return {x:a.right-12,y:(b.bottom+a.top)/2}})()`);
 await mouse('mouseMoved',gap.x,gap.y);
 await new Promise(r=>setTimeout(r,900));
 assert.equal(await evaluate(`getComputedStyle(document.querySelector(${JSON.stringify(user+' .msg-side-actions')})).opacity`),'1');
 await mouse('mouseMoved',0,0);
 await evaluate(`window.copied=null;document.querySelector(${JSON.stringify(copy)}).focus()`);
 assert.equal(await evaluate(`getComputedStyle(document.querySelector(${JSON.stringify(user+' .msg-side-actions')})).opacity`),'1');
 await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,text:'\r'});
 await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});
 await waitFor('window.copied!==null');assert.equal(await evaluate('window.copied'),'短消息');
 await evaluate("window.denyClipboard=true;document.execCommand=()=>false");
 await click(copy);await waitFor(`document.querySelector(${JSON.stringify(copy)}).textContent.includes('复制失败')`);
 await evaluate("document.execCommand=()=>{window.fallbackText=document.activeElement.value;return true}");
 await click(copy);await waitFor(`document.querySelector(${JSON.stringify(copy)}).textContent.includes('已复制')`);
 assert.equal(await evaluate('window.fallbackText'),'短消息');
 assert.equal(await evaluate('document.querySelectorAll("textarea").length'),0);
 const shot=await call('Page.captureScreenshot',{format:'png'});await writeFile(join(profile,'screenshot.png'),Buffer.from(shot.data,'base64'));
 // Current tasks use a second template, separate from legacy messages.
 await evaluate("window.denyClipboard=false;window.fixtureProps.currentGoal='当前任务正文'");
 await waitFor("!!document.querySelector('.user-message .msg-copy-btn')");
 await click('.user-message .msg-copy-btn');
 assert.equal(await evaluate('window.copied'),'当前任务正文');
 await click('.user-message .msg-fork-btn');
 assert.equal(await evaluate('window.forks.length'),3);
 console.log('PASS real AgentChatStream: collapsed/expanded/short/narrow bubbles, physical copy/fork clicks, header copy fidelity, keyboard focus, running fork disabled, denied clipboard fallback and failure label, escaped separator display');
 console.log('Screenshot: '+join(profile,'screenshot.png'));

}finally{ws?.close();chrome.kill();await server.close();}
