// Exercise the real component handlers without model or database I/O.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require('typescript');
const vm = require('node:vm');
const compiler = require('@vue/compiler-dom');
function extract(file, names, context) {
  const source = fs.readFileSync(file, 'utf8');
  const script = source.split('<script setup lang="ts">')[1].split('</script>')[0];
  const ast = ts.createSourceFile(file, script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const funcs = ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text));
  assert.equal(funcs.length, names.length);
  const js = ts.transpileModule(funcs.map(n => n.getText(ast)).join('\n') + '\n({' + names.join(',') + '});', {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  return { handlers: vm.runInNewContext(js, context), source };
}
const events = [];
let confirmAnswer = true; let confirmations = [];
const refs = {enabled:{value:true}, stages:{value:{plan:true, final_review:false}}, workspace:{value:'shared'}, profile:{value:'task'}, planMode:{value:'auto'}, completion:{value:'delivery'}, toolMode:{value:'allowlist'}, toolsText:{value:'read_file'}, parseError:{value:false}, specialized:{get value(){return refs.profile.value === 'reviewer' || refs.workspace.value === 'isolated';}}};
const roleProps = {allowedKinds:[]};
const role = extract('src/components/agent/RoleExecutionSettings.vue',['applyPreset','publish','toolNames','specialChangeWarning','approveChanges','changeProfile','changeWorkspace','kindExpansionWarning','changeEnabled','changeToolMode','resetInputs'],{...refs, props:roleProps, names:{task:'通用子任务',worker:'调查助手',writer:'实施助手',reviewer:'专用审查'}, lastEmitted:undefined, window:{confirm:(msg)=>{confirmations.push(msg);return confirmAnswer;}}, emit:(...args)=>events.push(args)});
role.handlers.applyPreset({profile:'writer',planMode:'auto',completion:'handoff'});
let saved = JSON.parse(events.find(e=>e[0]==='update:modelValue')[1]);
assert.deepEqual(saved.execution.allowed_tools,['read_file']);
assert.deepEqual(saved.execution.stages,{plan:true,final_review:false}, 'ordinary preset retains stage settings');
assert.equal(saved.profile,'writer');
assert(confirmations.length > 0, 'changing execution and handoff requires confirmation');
refs.toolsText.value = '';
events.length=0;
role.handlers.applyPreset({profile:'task',planMode:'auto',completion:'delivery'});
assert.deepEqual(JSON.parse(events[0][1]).execution.allowed_tools,[], 'an explicit empty restriction must remain empty');
confirmAnswer=false; events.length=0;
role.handlers.applyPreset({profile:'reviewer',planMode:'auto',completion:'handoff'});
assert.equal(events.length,0,'cancel preset must not publish');
assert.equal(refs.profile.value,'task'); assert.equal(refs.stages.value.plan,true);
const dropdown={value:'reviewer'};
role.handlers.changeProfile({target:dropdown});
assert.equal(dropdown.value,'task'); assert.equal(events.length,0, 'cancel direct profile change preserves settings');
confirmAnswer=true;
role.handlers.changeProfile({target:{value:'reviewer'}});
saved = JSON.parse(events[0][1]);
assert.equal(saved.profile,'reviewer'); assert.equal(saved.execution.stages,undefined);
assert.deepEqual(saved.execution.allowed_tools,[]);
// Changing away from an isolated writer also changes workspace, so cancelling must restore the selector.
refs.profile.value='writer'; refs.workspace.value='isolated'; confirmAnswer=false; events.length=0;
const profileSelect={value:'task'}; role.handlers.changeProfile({target:profileSelect});
assert.equal(profileSelect.value,'writer'); assert.equal(refs.workspace.value,'isolated'); assert.equal(events.length,0);
// Turning off defaults or switching to inherited tools must not silently discard restrictions.
refs.profile.value='task'; refs.workspace.value='shared'; refs.enabled.value=true; refs.toolMode.value='allowlist'; refs.toolsText.value='read_file';
confirmAnswer=false; events.length=0;
const inheritSelect={value:'inherit'}; role.handlers.changeToolMode({target:inheritSelect});
assert.equal(inheritSelect.value,'allowlist'); assert.equal(refs.toolMode.value,'allowlist'); assert.equal(events.length,0);
const enabledBox={checked:false}; role.handlers.changeEnabled({target:enabledBox});
assert.equal(enabledBox.checked,true); assert.equal(refs.enabled.value,true); assert.equal(events.length,0);
confirmAnswer=true; role.handlers.changeEnabled({target:{checked:false}});
assert.equal(refs.enabled.value,false); assert.equal(events[0][1],'');
assert.equal(refs.toolMode.value,'inherit'); assert.equal(refs.toolsText.value,'', 're-enabling must not silently restore a removed restriction');
// Corrupt saved defaults cannot be silently overwritten or claimed to preserve their restrictions.
refs.parseError.value=true; confirmAnswer=false; events.length=0; confirmations.length=0;
role.handlers.applyPreset({profile:'task',planMode:'auto',completion:'delivery'});
assert.equal(events.length,0); assert.match(confirmations[0],/工具限制可能无法保留/);
const enableCorrupt={checked:true}; role.handlers.changeEnabled({target:enableCorrupt});
assert.equal(enableCorrupt.checked,false); assert.equal(events.length,0);
refs.parseError.value=false;
// Loading an empty or damaged role must not reuse the previously edited role's settings.
const roleScript=role.source.split('<script setup lang="ts">')[1].split('</script>')[0];
const roleAst=ts.createSourceFile('role.ts',roleScript,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
const resetDecl=roleAst.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name?.text==='resetInputs');
const watchStmt=roleAst.statements.find(n=>ts.isExpressionStatement(n)&&ts.isCallExpression(n.expression)&&n.expression.expression.getText(roleAst)==='watch');
assert(resetDecl && watchStmt);
let watchCallback;
const watcherJs=ts.transpileModule(resetDecl.getText(roleAst)+'\n'+watchStmt.getText(roleAst),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
vm.runInNewContext(watcherJs,{...refs,props:{modelValue:''},lastEmitted:undefined,watch:(_get,cb)=>{watchCallback=cb;}});
refs.profile.value='writer'; refs.toolMode.value='allowlist'; refs.toolsText.value='write_file'; refs.stages.value={final_review:true};
watchCallback(''); assert.equal(refs.profile.value,'task'); assert.equal(refs.toolMode.value,'inherit'); assert.equal(refs.toolsText.value,''); assert.equal(Object.keys(refs.stages.value).length,0);
refs.profile.value='writer'; refs.toolMode.value='allowlist'; watchCallback('{broken'); assert.equal(refs.parseError.value,true); assert.equal(refs.profile.value,'task'); assert.equal(refs.toolMode.value,'inherit');
watchCallback(JSON.stringify({profile:'worker',execution:{allowed_tools:['read_file'],workflow:{completion:'handoff'}}}));
assert.equal(refs.parseError.value,false); assert.equal(refs.profile.value,'worker'); assert.equal(refs.toolsText.value,'read_file');
// Expanding allowed role types must be explicit; cancelling changes neither profile nor configuration.
refs.enabled.value=true; refs.profile.value='task'; refs.workspace.value='shared'; refs.planMode.value='auto'; refs.completion.value='delivery'; refs.stages.value={plan:true}; refs.toolMode.value='allowlist'; refs.toolsText.value='read_file'; roleProps.allowedKinds=['task'];
confirmAnswer=false; confirmations.length=0; events.length=0;
const workerSelect={value:'worker'}; role.handlers.changeProfile({target:workerSelect});
assert.equal(workerSelect.value,'task'); assert.equal(events.length,0); assert.match(confirmations[0],/加入角色允许挂载的类型/);
confirmAnswer=true; events.length=0; role.handlers.changeProfile({target:{value:'worker'}});
saved=JSON.parse(events.find(e=>e[0]==='update:modelValue')[1]);
assert.equal(saved.profile,'worker'); assert.deepEqual(saved.execution.allowed_tools,['read_file']); assert.deepEqual(saved.execution.stages,{plan:true});
assert(events.some(e=>e[0]==='profile-change' && e[1]==='worker'));
// Changing workflow preserves tool limits, and changing tool limits preserves workflow.
roleProps.allowedKinds=[]; refs.profile.value='task'; refs.planMode.value='cross_review'; refs.stages.value={plan:true,final_review:false}; events.length=0;
role.handlers.publish(); saved=JSON.parse(events[0][1]); assert.deepEqual(saved.execution.allowed_tools,['read_file']);
events.length=0; role.handlers.changeToolMode({target:{value:'inherit'}}); saved=JSON.parse(events[0][1]);
assert.equal(saved.execution.allowed_tools,undefined); assert.equal(saved.execution.workflow.plan_mode,'cross_review'); assert.deepEqual(saved.execution.stages,{plan:true,final_review:false});
// Removing the last allowed type would mean unrestricted, so that expansion needs confirmation.
const roleForm={allowed_kinds:['task']}; confirmAnswer=false;
const kindHandlers=extract('src/views/RoleManagerView.vue',['toggleKind'],{form:roleForm,window:{confirm:()=>confirmAnswer}}).handlers;
kindHandlers.toggleKind('task'); assert.deepEqual(roleForm.allowed_kinds,['task']);
confirmAnswer=true; kindHandlers.toggleKind('task'); assert.deepEqual(roleForm.allowed_kinds,[]);
const sent=[]; const locked={value:true}; const props={isRunning:false,modelRouting:{stages:{plan:false,final_review:true},stage_review_committee:false}};
const composer = extract('src/components/agent/AgentComposer.vue',['setFixedStage','setConcurrencyMode','selectPlanMode'],{workflowLocked:locked,props,emit:(...args)=>sent.push(args),withFixedStageChange:(r,k,m,stages)=>({...r,stages}),planMenuOpen:{value:false},activeModeGuide:{value:null}});
composer.handlers.setFixedStage('stage_review','auto',{}); composer.handlers.setConcurrencyMode('committee'); composer.handlers.selectPlanMode('auto');
assert.equal(sent.length,0, 'locked handlers cannot alter policy');
locked.value=false; composer.handlers.setFixedStage('stage_review','auto',{}); composer.handlers.setConcurrencyMode('committee');
assert.equal(sent.length,2,'stage controls must not emit a plan mode change');
assert(sent.every(e=>e[0]==='update:model-routing'));
sent.length=0; composer.handlers.selectPlanMode('plan_approval');
assert.equal(sent[0][1].stages.plan,true, 'human approval requires plan');
assert.equal(sent[0][1].stages.final_review,true, 'approval retains other stages');
assert.equal(props.modelRouting.stages.plan,false,'does not mutate props');
// Evaluate the real summary computed against current, legacy and unavailable checkpoints.
const componentScript = composer.source.split('<script setup lang="ts">')[1].split('</script>')[0];
const sourceAst=ts.createSourceFile('composer.ts',componentScript,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
const snapshotDeclaration=sourceAst.statements.find(n=>ts.isVariableStatement(n) && n.declarationList.declarations.some(d=>d.name.getText(sourceAst)==='workflowSnapshotText'));
assert(snapshotDeclaration);
const summaryJs=ts.transpileModule(snapshotDeclaration.getText(sourceAst)+'\nworkflowSnapshotText', {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
const snapshotProps={taskWorkflowSnapshot:null};
const summary=vm.runInNewContext(summaryJs,{props:snapshotProps,computed:fn=>({get value(){return fn();}})});
assert.match(summary.value,/尚未读取到/);
snapshotProps.taskWorkflowSnapshot={plan_mode:'cross_review',stages:{plan:true,stage_review:false},task_review_routing:{stage_review_committee:true,stage_reviewer_override:'disabled'}};
assert.match(summary.value,/交叉审查/); assert.match(summary.value,/阶段审查 关闭/); assert.match(summary.value,/多人/);
snapshotProps.taskWorkflowSnapshot={plan_mode:'auto',stages:{},task_review_routing:null};
assert.match(summary.value,/旧任务未保存/);
snapshotProps.taskWorkflowSnapshot={plan_mode:'auto',stages:{},task_review_routing:{stage_review_committee:false,stage_reviewer_override:'disabled'}};
assert.match(summary.value,/阶段审查 关闭（旧配置）/);
const ast=compiler.baseParse(composer.source.split('<script setup')[0].replace(/^<template>/,'').replace(/<\/template>\s*$/,''));
let checked=0;
function visit(n){ if(n.type===1){const click=n.props.find(p=>p.name==='on'&&p.arg?.content==='click'&&/setConcurrencyMode/.test(p.exp?.content||'')); if(click){ assert(n.props.some(p=>p.name==='bind'&&p.arg?.content==='disabled'&&p.exp?.content==='workflowLocked')); checked++; }} for(const c of n.children||[])visit(c); }
visit(ast); assert.equal(checked,2);
assert(composer.source.includes(':disabled="workflowLocked" :approval='));
assert(!composer.source.includes("setTriggerMode("), 'stage review has one control');
console.log('PASS: role preset preservation/confirmation; stage control decoupling; workflow lock');
