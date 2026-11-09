// Real Vue component in a headless browser; no model, database or desktop host required.
import { createServer } from 'vite';
import vue from '@vitejs/plugin-vue';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const executable = process.env.CHROME_BIN || (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'chromium');
const profile = await mkdtemp(join(tmpdir(), 'role-settings-ui-'));
const entry = `import {createApp,h,reactive} from 'vue';
import Role from '/src/components/agent/RoleExecutionSettings.vue';
const state=reactive({raw:JSON.stringify({profile:'task',execution:{allowed_tools:['read_file'],workflow:{plan_mode:'auto',completion:'delivery'},stages:{plan:true}}}),allowed:['task'],answer:true,confirmations:[]});
window.testState=state;window.confirm=msg=>{state.confirmations.push(msg);return state.answer};
createApp({render:()=>h(Role,{modelValue:state.raw,allowedKinds:state.allowed,'onUpdate:modelValue':v=>state.raw=v,'onProfile-change':v=>{if(state.allowed.length&&!state.allowed.includes(v))state.allowed.push(v)}})}).mount('#app');`;
const server = await createServer({root,configFile:false,plugins:[vue(),{name:'role-settings-test',resolveId:id=>id==='virtual:role-settings-test'?'\0role-settings-test':undefined,load:id=>id==='\0role-settings-test'?entry:undefined,configureServer(s){s.middlewares.use('/__role_test',(_,res)=>{res.setHeader('Content-Type','text/html');res.end('<div id="app"></div><script type="module" src="/@id/__x00__role-settings-test"></script>')})}}],server:{host:'127.0.0.1',port:0},logLevel:'error'});
let chrome, ws;
try {
 await server.listen();
 chrome=spawn(executable,['--headless=new','--no-sandbox','--disable-gpu','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:'ignore'});
 let launchError; chrome.on('error',e=>launchError=e);
 let port; for(let i=0;i<160;i++){if(launchError)throw launchError;try{port=(await readFile(join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0];break}catch{}await new Promise(r=>setTimeout(r,50));} if(!port)throw Error('Chrome did not start');
 const pages=await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();ws=new WebSocket(pages.find(p=>p.type==='page').webSocketDebuggerUrl);await new Promise(r=>ws.addEventListener('open',r,{once:true}));
 let seq=0;const pending=new Map();ws.addEventListener('message',e=>{const m=JSON.parse(e.data);if(m.id){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result)}});
 const call=(method,params={})=>new Promise((resolve,reject)=>{pending.set(++seq,{resolve,reject});ws.send(JSON.stringify({id:seq,method,params}))});
 const evalJS=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(JSON.stringify(r.exceptionDetails));return r.result.value};
 await call('Page.navigate',{url:`http://127.0.0.1:${server.httpServer.address().port}/__role_test`});
 for(let i=0;i<150 && !await evalJS('!!document.querySelector(".execution-settings select")');i++) await new Promise(r=>setTimeout(r,50));
 const choose=async(selector,value)=>{await evalJS(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});if(!e)throw Error('missing select');e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('change',{bubbles:true}))})()`);await new Promise(r=>setTimeout(r,30))};
 const tools='section[aria-label="工具权限"] select'; const stages='section[aria-label="工作流程"] fieldset select'; const roles='section[aria-label="执行方式"] select';
 await evalJS('testState.answer=false'); await choose(tools,'inherit');
 assert.equal(await evalJS(`document.querySelector(${JSON.stringify(tools)}).value`),'allowlist'); assert.deepEqual(await evalJS('JSON.parse(testState.raw).execution.allowed_tools'),['read_file']);
 await evalJS('testState.answer=true');await choose(stages,'off');
 assert.deepEqual(await evalJS('JSON.parse(testState.raw).execution.allowed_tools'),['read_file']); assert.equal(await evalJS('JSON.parse(testState.raw).execution.stages.plan'),false);
 await evalJS('testState.answer=false');await choose(roles,'worker'); assert.equal(await evalJS('JSON.parse(testState.raw).profile'),'task'); assert.deepEqual(await evalJS('JSON.parse(JSON.stringify(testState.allowed))'),['task']);
 await evalJS('testState.answer=true');await choose(roles,'worker'); assert.equal(await evalJS('JSON.parse(testState.raw).profile'),'worker'); assert.deepEqual(await evalJS('JSON.parse(JSON.stringify(testState.allowed))'),['task','worker']); assert.deepEqual(await evalJS('JSON.parse(testState.raw).execution.allowed_tools'),['read_file']);
 await choose(tools,'inherit'); assert.equal(await evalJS('Object.hasOwn(JSON.parse(testState.raw).execution,"allowed_tools")'),false); assert.equal(await evalJS('JSON.parse(testState.raw).execution.stages.plan'),false);
 await evalJS(`testState.raw='{broken';testState.answer=false`);await new Promise(r=>setTimeout(r,30)); await evalJS('document.querySelector(".enable input").click()');
 assert.equal(await evalJS('testState.raw'),'{broken'); assert.equal(await evalJS('document.querySelector(".enable input").checked'),false);
 console.log('PASS real role settings: independent workflow and permissions, cancelled/confirmed expansion, corrupted-config protection');
} finally {ws?.close();chrome?.kill();await server.close();await rm(profile,{recursive:true,force:true});}
