const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const {parse, compileScript} = require('@vue/compiler-sfc');
const source = fs.readFileSync('src/components/agent/AttachmentCard.vue', 'utf8');
const descriptor = parse(source).descriptor;
const compiled = compileScript(descriptor, {id: 'attachment-history', inlineTemplate: true}).content;
const transpile = s => ts.transpileModule(s, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText;
const calls = [], events = [];
let reply = async () => ({image_data_url: 'data:image/png;base64,recovered'});
const invoke = async (name, args) => {calls.push({name, args}); return reply(name, args);};
const context = {exports: {}, console: {warn() {}}, require: name => {
  if (name === 'vue') return vue;
  if (name === '@tauri-apps/api/core') return {convertFileSrc: p => 'asset://' + p, invoke};
  if (name === './FileIcon.vue') return {default: vue.defineComponent({setup: () => () => vue.h('span', 'image')})};
  throw new Error(name);
}};
vm.runInNewContext(transpile(compiled), context);
const node = (type, text = '') => ({type, text, props: {}, children: [], parent: null});
const root = node('root');
const renderer = vue.createRenderer({
  createElement: node, createText: text => node('#text', text), createComment: text => node('#comment', text),
  setText: (n,t) => {n.text=t;}, setElementText: (n,t) => {n.text=t;n.children=[];},
  patchProp: (n,k,_old,v) => {n.props[k]=v;}, parentNode: n => n.parent,
  nextSibling: n => n.parent?.children[n.parent.children.indexOf(n)+1],
  insert(n,p,a) {if(n.parent)n.parent.children.splice(n.parent.children.indexOf(n),1);n.parent=p;const i=a?p.children.indexOf(a):-1;i<0?p.children.push(n):p.children.splice(i,0,n);},
  remove(n) {if(n.parent)n.parent.children.splice(n.parent.children.indexOf(n),1);},
});
const attachment = path => ({file_name:'screen.png',file_path:path,content_type:'image/png',image_data_url:null,size_bytes:100,prompt_text:'',raw_preview:null});
const state = vue.reactive({attachment: attachment('/tmp/draft-1/attachments/a.png')});
const app = renderer.createApp({setup: () => () => vue.h(context.exports.default, {attachment:state.attachment,sessionId:'formal-session',onPreviewImage:a=>events.push(a)})});
app.mount(root);
const all = () => {const out=[];const walk=n=>{out.push(n);n.children.forEach(walk);};walk(root);return out;};
const img = () => all().find(n=>n.type==='img');
const flush = async () => {for(let i=0;i<5;i++){await Promise.resolve();await vue.nextTick();}};
async function main() {
  assert(img().props.src.includes('/tmp/draft-1/attachments/a.png'));
  await img().props.onError(); await flush();
  assert.equal(calls[0].name, 'parse_session_attachment');
  assert.equal(calls[0].args.sessionId, 'formal-session');
  assert.equal(calls[0].args.path, '/tmp/draft-1/attachments/a.png');
  assert.equal(img().props.src, 'data:image/png;base64,recovered', 'lost draft path recovers from the session-owned backend result');
  root.children[0].props.onClick({stopPropagation(){}});
  assert.equal(events.length, 1);
  state.attachment = attachment('/tmp/formal/attachments/b.png'); await flush();
  assert.equal(img().props.src, 'asset:///tmp/formal/attachments/b.png', 'reused card cannot retain the previous image');
  let resolveOld;
  reply = () => new Promise(resolve=>{resolveOld=resolve;});
  const pending = img().props.onError();
  state.attachment = attachment('/tmp/formal/attachments/c.png'); await flush();
  resolveOld({image_data_url:'data:image/png;base64,stale'}); await pending; await flush();
  assert.equal(img().props.src, 'asset:///tmp/formal/attachments/c.png', 'late recovery must not replace a newer attachment');
  reply = async () => {throw new Error('no saved image');};
  await img().props.onError(); await flush();
  assert(!img(), 'unrecoverable images show a placeholder rather than a broken-image loop');

  const stream = fs.readFileSync('src/components/agent/AgentChatStream.vue','utf8');
  const script = stream.split('<script setup lang="ts">')[1].split('</script>')[0];
  const ast = ts.createSourceFile('stream.ts',script,ts.ScriptTarget.Latest,true);
  const fn = ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name?.text==='openImagePreview');
  const previewImageModal = {value:null};
  reply = async () => ({...attachment('/lost.png'),image_data_url:'data:image/png;base64,full-preview'});
  const open = vm.runInNewContext(transpile(fn.getText(ast)+'\nopenImagePreview;'), {props:{sessionId:'formal-session'},invoke,previewImageModal,console});
  await open(attachment('/lost.png'));
  assert.equal(calls.at(-1).name,'parse_session_attachment');
  assert.equal(previewImageModal.value.image_data_url,'data:image/png;base64,full-preview');
  assert(stream.match(/:session-id="sessionId"/g).length >= 3, 'legacy, task, and intervention attachment trays all pass session identity');
  app.unmount();
  console.log('PASS attachment history images: session-bound thumbnail and full preview recovery, retained original click data, card reuse, late-reply isolation, explicit unavailable state, all history trays');
}
main().catch(e=>{console.error(e);process.exitCode=1;});
