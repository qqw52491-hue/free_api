// Exercise actual station templates and routing handlers; no native model selects or API requests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const ts = require('typescript');
const vue = require('vue');
const compiler = require('@vue/compiler-dom');
const {renderToString} = require('@vue/server-renderer');
const file = path.resolve(__dirname, '../src/components/agent/AgentComposer.vue');
const source = fs.readFileSync(file, 'utf8');
const script = source.split('<script setup lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('composer.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const names = ['getSlotChoice','isCustomSpecificModel','setReviewerBackup','setSlotChoice','onSlotSelectChange'];
const funcs = ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text));
assert.equal(funcs.length, names.length);
const exportsCode = ts.transpileModule(funcs.map(n=>n.getText(ast)).join('\n') + '\n({' + names.join(',') + '});', {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
const models = [{id:'model-a',name:'model-a',display_name:'模型 A',platform_name:'测试平台'},{id:'model-b',name:'model-b',display_name:'模型 B',platform_name:'另一平台'}];
const props = {isRunning:false,allModels:models,modelRouting:{pro:'model-a',flash:'model-b',role_models:{unrelated:'keep'},reviewer_backup_model:''}};
const events = [];
const handlers = vm.runInNewContext(exportsCode, {props,emit:(name,value)=>{events.push({name,value});props.modelRouting=value;}});
const root = compiler.baseParse(source.split('<script setup')[0].replace(/^<template>/,'').replace(/<\/template>\s*$/,''));
const bars = []; const pickers = []; let native = 0;
function walk(n) {
 if(n.type===1){
  if(n.tag==='select') native++;
  if(n.tag==='div' && n.props.some(p=>p.name==='class' && p.value?.content==='station-model-bar')) bars.push(n);
  if(n.tag==='ModelSelectDropdown' && n.props.some(p=>p.name==='class' && p.value?.content==='slot-model-picker')) pickers.push(n);
 }
 for(const c of n.children||[])walk(c);
}
walk(root);assert.equal(native,0,'AgentComposer model choices must not use native select');assert.equal(pickers.length,4);assert.equal(bars.length,4);
for(const p of pickers){assert(p.props.some(x=>x.name==='bind'&&x.arg?.content==='models'&&x.exp?.content==='allModels'));assert(p.props.some(x=>x.name==='bind'&&x.arg?.content==='disabled'&&x.exp?.content==='isRunning'));}
const seen=[];
const Picker = vue.defineComponent({name:'ModelSelectDropdown',props:['modelValue','models','disabled','placeholder','size'],emits:['update:modelValue'],setup(p,{emit}){seen.push({props:p,choose:id=>emit('update:modelValue',id)});return()=>vue.h('button',{disabled:p.disabled},p.modelValue||p.placeholder);}});
async function renderBars(){seen.length=0;for(const bar of bars){const render=new Function('Vue',compiler.compile(bar.loc.source,{mode:'function'}).code)(vue);const ctx={...handlers,...props,getModelDisplayName:id=>id};await renderToString(vue.createSSRApp({components:{ModelSelectDropdown:Picker},render:()=>render(ctx,[])}));}}
async function main(){
 await renderBars();assert.equal(seen.length,4);
 for(const [i,slot] of [[0,'writer'],[1,'reviewer'],[3,'scout']]){seen[i].choose('model-a');assert.equal(props.modelRouting.role_models[slot],'model-a');assert.equal(props.modelRouting.role_models.unrelated,'keep');handlers.onSlotSelectChange(slot,'');assert.equal(props.modelRouting.role_models[slot],slot==='reviewer'?undefined:'flash');}
 seen[2].choose('model-b');assert.equal(props.modelRouting.reviewer_backup_model,'model-b');handlers.setReviewerBackup('');assert.equal(props.modelRouting.reviewer_backup_model,'');
 handlers.onSlotSelectChange('reviewer','model-a');await renderBars();assert.equal(seen[1].props.modelValue,'model-a');assert.equal(seen[1].props.size,'sm');
 props.isRunning=true;await renderBars();assert(seen.every(p=>p.props.disabled));const count=events.length;seen[1].choose('model-b');handlers.onSlotSelectChange('reviewer','');handlers.setReviewerBackup('model-b');assert.equal(events.length,count,'running guard must prevent model changes');
 const plugin=fs.readFileSync(path.resolve(__dirname,'../src/components/common/ModelSelectDropdown.vue'),'utf8');const src=plugin.split('<script setup lang="ts">')[1].split('</script>')[0];const a=ts.createSourceFile('picker.ts',src,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);const selected=a.statements.filter(n=>ts.isFunctionDeclaration(n)&&['selectModel','closeDropdown'].includes(n.name?.text));const output=ts.transpileModule(selected.map(n=>n.getText(a)).join('\n')+'\nselectModel;',{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;const changes=[];const isOpen={value:true},searchQuery={value:'model'};const select=vm.runInNewContext(output,{emit:(...v)=>changes.push(v),isOpen,searchQuery});select(models[0]);assert.equal(changes[0][0],'update:modelValue');assert.equal(changes[0][1],'model-a');assert.equal(isOpen.value,false);assert.equal(searchQuery.value,'');
 console.log('PASS model selectors: 4 real station templates use shared picker; selection/reset/backup/disabled binding and original routing preservation; shared picker emits model ID');
}
main().then(()=>process.exit(0)).catch(e=>{console.error(e);process.exit(1)});
