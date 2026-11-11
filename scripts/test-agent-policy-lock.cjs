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
const events=[]; let confirmAnswer=true; const confirmations=[];
const refs={flowMode:{value:'custom'},stages:{value:{plan:true,final_review:false}},workspace:{value:'shared'},profile:{value:'task'},planMode:{value:'plan_approval'},completion:{value:'delivery'},toolMode:{value:'allowlist'},toolsText:{value:'read_file'},parseError:{value:false},specialized:{get value(){return refs.profile.value==='reviewer'||refs.workspace.value==='isolated';}}};
const roleProps={allowedKinds:[]};
const role=extract('src/components/agent/RoleExecutionSettings.vue',['applyPreset','publish','toolNames','specialChangeWarning','approveChanges','changeProfile','changeWorkspace','kindExpansionWarning','changeFlowMode','changeToolMode','resetInputs','repairConfig'],{...refs,props:roleProps,names:{task:'通用子任务',worker:'调查助手',writer:'实施助手',reviewer:'专用审查'},lastEmitted:undefined,window:{confirm:message=>{confirmations.push(message);return confirmAnswer;}},emit:(...args)=>events.push(args)});
assert(!role.source.includes('使用角色默认值'),'the destructive master switch is removed');
confirmAnswer=false;const flowSelect={value:'inherit'};role.handlers.changeFlowMode({target:flowSelect});
assert.equal(flowSelect.value,'custom');assert.equal(events.length,0);assert.equal(refs.planMode.value,'plan_approval');
confirmAnswer=true;role.handlers.changeFlowMode({target:{value:'inherit'}});
let saved=JSON.parse(events[0][1]);assert.deepEqual(saved.execution.allowed_tools,['read_file']);assert.deepEqual(saved.execution.workflow,{});assert.equal(saved.execution.stages,undefined);assert.equal(saved.profile,'task');
events.length=0;role.handlers.changeFlowMode({target:{value:'custom'}});saved=JSON.parse(events[0][1]);assert.equal(saved.execution.workflow.plan_mode,'plan_approval');assert.deepEqual(saved.execution.allowed_tools,['read_file']);
confirmAnswer=false;events.length=0;const tools={value:'inherit'};role.handlers.changeToolMode({target:tools});assert.equal(tools.value,'allowlist');assert.equal(events.length,0);
confirmAnswer=true;role.handlers.changeToolMode({target:{value:'inherit'}});saved=JSON.parse(events[0][1]);assert.equal(saved.execution.allowed_tools,undefined);assert.equal(saved.execution.workflow.plan_mode,'plan_approval');assert.deepEqual(saved.execution.stages,{plan:true,final_review:false});
refs.toolMode.value='allowlist';refs.toolsText.value='';events.length=0;role.handlers.applyPreset({profile:'worker',planMode:'auto',completion:'handoff'});saved=JSON.parse(events[0][1]);assert.deepEqual(saved.execution.allowed_tools,[]);assert.deepEqual(saved.execution.stages,{plan:true,final_review:false});
refs.profile.value='';refs.flowMode.value='inherit';refs.toolsText.value='read_file';events.length=0;role.handlers.publish();saved=JSON.parse(events[0][1]);assert.equal(saved.profile,undefined);assert.deepEqual(saved.execution.allowed_tools,['read_file']);assert.deepEqual(saved.execution.workflow,{});
refs.parseError.value=true;events.length=0;role.handlers.publish();assert.equal(events.length,0,'damaged configurations cannot be silently replaced');confirmAnswer=false;role.handlers.repairConfig();assert.equal(events.length,0);confirmAnswer=true;role.handlers.repairConfig();assert.deepEqual(JSON.parse(events[0][1]).execution.workflow,{});
// Load both legacy bundles and new independent configurations through the actual watcher.
const roleScript=role.source.split('<script setup lang="ts">')[1].split('</script>')[0];const roleAst=ts.createSourceFile('role.ts',roleScript,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
const watchStmt=roleAst.statements.find(n=>ts.isExpressionStatement(n)&&ts.isCallExpression(n.expression)&&n.expression.expression.getText(roleAst)==='watch');let watchCallback;
vm.runInNewContext(ts.transpileModule(watchStmt.getText(roleAst),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText,{...refs,props:{modelValue:''},lastEmitted:undefined,resetInputs:role.handlers.resetInputs,watch:(_get,cb)=>watchCallback=cb});
const legacy={profile:'writer',execution:{allowed_tools:['read_file'],stages:{final_review:true},workflow:{plan_mode:'plan_approval',completion:'handoff'}}};watchCallback(JSON.stringify(legacy));events.length=0;role.handlers.publish();saved=JSON.parse(events[0][1]);assert.deepEqual(saved,legacy,'editing preserves all configured legacy values');
watchCallback(JSON.stringify({execution:{allowed_tools:[]}}));assert.equal(refs.profile.value,'');assert.equal(refs.flowMode.value,'inherit');assert.equal(refs.toolMode.value,'allowlist');assert.equal(refs.toolsText.value,'');
assert(!role.source.includes('<option value="isolated">'));
watchCallback(JSON.stringify({profile:'writer',execution:{workspace:'isolated',allowed_tools:['read_file']}}));
confirmAnswer=false; events.length=0; role.handlers.changeWorkspace();
assert.equal(refs.workspace.value,'isolated'); assert.equal(events.length,0);
confirmAnswer=true; role.handlers.changeWorkspace();
saved=JSON.parse(events[0][1]);
assert.equal(saved.execution.workspace,undefined);
assert.deepEqual(saved.execution.allowed_tools,['read_file']);
watchCallback('');assert.equal(refs.flowMode.value,'inherit');assert.equal(refs.toolMode.value,'inherit');watchCallback('{broken');assert.equal(refs.parseError.value,true);
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
// Inheriting workflow values must not clear an independent role tool restriction.
refs.parseError.value=false; refs.flowMode.value='custom'; refs.profile.value='task'; refs.workspace.value='shared'; refs.planMode.value=''; refs.completion.value=''; refs.toolMode.value='allowlist'; refs.toolsText.value='read_file'; events.length=0;
role.handlers.publish(); const inheritedWorkflow=JSON.parse(events[0][1]);
assert.deepEqual(inheritedWorkflow.execution.workflow,{});
assert.deepEqual(inheritedWorkflow.execution.allowed_tools,['read_file']);
console.log('PASS: role preset preservation/confirmation; stage control decoupling; workflow lock');
