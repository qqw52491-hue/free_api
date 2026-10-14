// Real Vue reactivity/composable + host-shaped receipts, with no model or database writes.
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const ts = require('typescript');
const vm = require('node:vm');
const vue = require('vue');
const { load, session, emit } = require('./test-agent-task-resume.cjs');
const base = path.resolve(__dirname, '../src/components/agent');
const { getStepJobIds, getStepWorkers, parseChildJobReceipts, workerDisplayRows } = load(path.join(base, 'workerAssociation.ts'));
const { extractSubagentsFromStep } = load(path.join(base, 'workerUtils.ts'));
const a = '3583c312-31fe-4cf5-94c8-26e1acb57838', b = '6f21b859-56d1-451e-a23e-6dfaebc720cb', c = '9c36b52a-6af8-4f6a-bbe6-1b8c76754541';
const dispatchText = `[1/4] ✅ 已派发 child_run_id=${a} 任务: 检查源项目\n[2/4] ✅ 已派发 child_run_id=${b} 任务: 检查目标项目\n[3/4] ❌ 派发失败：并发已满\n[4/4] ❌ 派发失败：并发已满`;
const result = (id, status = 'succeeded', tool = 'child_worker') => `✅ 已成功等待并接收到 1 个后台任务结果：\n\n- 【任务 ID: ${id} | 类型: ${tool}】状态: ${status}, 耗时: 10021ms\n  描述: 独立只读侦察专员\n  输出: {"findings":["source evidence"],"confidence":"high","evidence_refs":["src/file:1"]}`;
function step(id, tool, params, rawResult, status = 'succeeded') { return { id, stepId: Number(id.replace(/\D/g,'')) || 0, tool, action: tool, params, rawResult, result: rawResult, status, timestamp: 1000, description: '任务', planId: 'p', planTitle: 'phase' }; }
async function main() {
 const dispatch = step('step-1','child_worker',{dispatches:[{task:'source',role_label:'源项目'},{task:'target',role_label:'目标项目'}]},dispatchText);
 const wait = step('step-2','wait_jobs', JSON.stringify({job_ids:[a,b],timeout_secs:45}),result(a));
 assert.deepEqual(getStepJobIds(dispatch),[a,b], 'only two successful dispatches, not four requested');
 assert.deepEqual(getStepJobIds(wait),[a,b], 'raw JSON params and partial receipt');
 const children = extractSubagentsFromStep(wait,[dispatch,wait]);
 assert.deepEqual(children.map(w=>[w.id,w.status]),[[a,'succeeded'],[b,'pending']], 'partial return never completes unreturned task');
 assert.equal(children[0].durationMs,10021); assert.equal(children[0].findings.length,1);
 assert.equal(children[1].roleLabel,'目标项目', 'batch branch metadata stays independent');
 const timeout = step('step-3','wait_jobs',{job_ids:[b]},'等待结束：仍有后台任务在运行中，本次等待窗口内无新完成。可再次 wait_jobs 挂起等待。');
 assert.equal(parseChildJobReceipts(timeout).length,0); assert.equal(extractSubagentsFromStep(timeout,[dispatch,wait,timeout])[0].status,'pending');
 assert.deepEqual(getStepJobIds(step('step-4','local_shell',{job_ids:[a]},result(a))),[], 'ordinary tools do not inherit child IDs');
 assert.deepEqual(getStepWorkers(step('step-5','child_worker',{},''),children),[], 'unknown identity is not the entire team');
 assert.deepEqual(extractSubagentsFromStep(step('step-6','wait_jobs',{},''),[dispatch]),[], 'no closest-dispatch fallback');
 for (const tool of ['child_worker','child_reviewer','child_writer']) {
  const prefix = {child_worker:'侦察 worker 已异步派发',child_reviewer:'子审查任务已异步派发',child_writer:'写入 worker 已在隔离副本上开工'}[tool];
  assert.deepEqual(getStepJobIds(step('step-7',tool,{},`${prefix}，child_run_id: ${a}。更多说明`)),[a]);
 }
 const failure = step('step-8','wait_jobs',{job_ids:[b]},result(b,'invalid_output','child_reviewer') + `\n🔁 审查者故障转移：上次结论异常（invalid_output），已自动改用备用审查员重派 (child_run_id: ${c})`);
 // Current host failover uses colon, unlike dispatch/resume's equals.
 assert.deepEqual(getStepJobIds(failure),[b,c]);
 assert.equal(parseChildJobReceipts(failure).find(r=>r.id===b).status,'failed');
 assert(!parseChildJobReceipts(failure).find(r=>r.id===b).output.includes('故障转移'));
 assert.deepEqual(getStepJobIds(step('step-9','resume_child',{child_run_id:a},`✅ 侦察 worker 追问已成功启动：新 child_run_id=${c}（继承历史）。`)),[c], 'resume params are OLD id');
 assert.deepEqual(getStepJobIds(step('step-10','child_worker',{},`未派发；任务描述说 child_run_id=${a}`)),[], 'task prose is not a receipt');
 const s = await session('failed');
 await emit('channel-run-started',{session_id:'existing-session',goal:'worker regression'});
 const progress = p => emit('agent-progress-existing-session',p);
 const tool = p => emit('agent-tool-event-existing-session',p);
 // Both channels are used in production: planned/progress + actual tool receipts.
 progress({type:'step_new',step:{id:0,tool:'child_worker',action:'child_worker',params:dispatch.params,description:'batch'}});
 assert.equal(s.teamWorkers.value.length,1); assert.equal(s.teamWorkers.value[0].extra.dispatchPlaceholder,true);
 progress({type:'child_worker_stream',child_run_id:a,stream_type:'thinking',chunk:'first'});
 progress({type:'child_worker_stream',child_run_id:b,stream_type:'thinking',chunk:'second'});
 tool({type:'succeeded',step_id:0,tool:'child_worker',action:'child_worker',params:dispatch.params,result:dispatchText});
 assert.deepEqual(s.teamWorkers.value.map(w=>w.id),[a,b], 'receipt replaces placeholder, does not merge same-role children');
 assert(s.teamWorkers.value.every(w=>w.status==='running'), 'successful dispatch is not successful child');
 assert.equal(s.teamWorkers.value[0].thought,'first'); assert.equal(s.teamWorkers.value[1].thought,'second');
 let teamRebuilds=0, rowRebuilds=0;
 const stableTeam=vue.computed(()=>{teamRebuilds++;return s.teamState.value;});
 const rows=vue.computed(()=>{rowRebuilds++;return workerDisplayRows(stableTeam.value.workers);});
 const initialRows=rows.value;
 for(let i=0;i<100;i++) { progress({type:'child_worker_stream',child_run_id:a,stream_type:'thinking',chunk:'x'}); assert.equal(rows.value,initialRows); }
 assert.equal(teamRebuilds,1); assert.equal(rowRebuilds,1, '100 thought chunks cause ZERO list reconstruction');
 tool({type:'started',step_id:1,tool:'wait_jobs',action:'wait_jobs',params:{job_ids:[a,b]}});
 tool({type:'succeeded',step_id:1,tool:'wait_jobs',action:'wait_jobs',params:{job_ids:[a,b]},result:result(a)});
 assert.equal(s.teamWorkers.value.find(w=>w.id===a).roleLabel,'源项目','wait must preserve a configured custom role label');
 assert.equal(s.teamWorkers.value.find(w=>w.id===a).status,'succeeded'); assert.equal(s.teamWorkers.value.find(w=>w.id===b).status,'running');
 progress({type:'step_new',step:{id:2,tool:'wait_jobs',params:{job_ids:[b]}}});
 progress({type:'step_done',step_id:2,output:timeout.rawResult});
 assert.equal(s.teamWorkers.value.find(w=>w.id===b).status,'running','progress completion/timeout not child completion');
 progress({type:'step_new',step:{id:3,tool:'wait_jobs',params:{job_ids:[b]}}});
 progress({type:'step_done',step_id:3,output:failure.rawResult});
 assert.equal(s.teamWorkers.value.find(w=>w.id===b).status,'failed'); assert.equal(s.teamWorkers.value.find(w=>w.id===c).status,'running');
 progress({type:'step_new',step:{id:4,tool:'child_reviewer',params:{question:'review'}}});
 assert.equal(s.teamWorkers.value.filter(w=>w.extra?.dispatchPlaceholder).length,1);
 progress({type:'step_error',step_id:4,output:'派发失败，模型不可用'});
 assert.equal(s.teamWorkers.value.filter(w=>w.extra?.dispatchPlaceholder).length,0,'failed dispatch clears pending placeholder');
 const pending = step('step-repeated','child_worker',{},undefined,'pending');
 const placeholder = {id:'dispatch-step-repeated',extra:{parentStepId:pending.id,dispatchPlaceholder:true}};
 assert.equal(extractSubagentsFromStep(pending,[pending],[placeholder]).length,1);
 assert.equal(extractSubagentsFromStep({...pending},[pending],[placeholder]).length,0,'historical step id cannot take current placeholder');
 const explicitLive = extractSubagentsFromStep(dispatch,[dispatch,wait],s.teamWorkers.value);
 assert.equal(explicitLive.length,2); assert.equal(explicitLive[1].thought,'second','detail uses correct live child, not static stale copy');
 assert.equal(rows.value.get(b).status,'failed'); assert(rowRebuilds>1,'real status changes DO refresh display');
 // Execute the actual stream grouping function: ordinary steps never get embedded whole-team rows.
 const source = fs.readFileSync(path.join(base,'AgentChatStream.vue'),'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
 const ast = ts.createSourceFile('stream.ts',source,ts.ScriptTarget.Latest,true);
 const functions = ['buildAmbientDisplayItems','getAmbientDisplayItems'];
 for (const name of functions) assert(ast.statements.some(n=>ts.isFunctionDeclaration(n)&&n.name?.text===name));
 assert(!source.match(/return workers;\s*}\s*\n\nasync function handlePreviewArtifact/));
 assert(!source.includes('startedAt >= step.timestamp - 5000'), 'no role/time identity heuristic');
 assert(source.includes('const ambientDisplayItems = computed'), 'grouping is computed, not per-render');
 // Evaluate the actual component's caches/grouping functions, not a copy of their logic.
 const names = ['stepWorkersByStep','ambientWorkerRows','ambientDisplayItems'];
 const fnNames = ['buildAmbientDisplayItems','getAmbientDisplayItems','getStepActiveWorkers','getAmbientWorkerRows'];
 const selected = ast.statements.filter(n => ts.isFunctionDeclaration(n) && fnNames.includes(n.name?.text)
   || ts.isVariableStatement(n) && n.declarationList.declarations.some(d => names.includes(d.name.getText(ast))));
 assert.equal(selected.length,names.length+fnNames.length);
 const ordinary = Array.from({length:35},(_,i)=>step(`step-${i+20}`,'local_shell',{},'ok'));
 const props = vue.reactive({steps:[dispatch,wait,...ordinary],historyTurns:[], get teamState(){return s.teamState.value;}});
 const timeline = vue.computed(()=>[{id:'plan',type:'plan',steps:props.steps}]);
 const ctx = {props,computed:vue.computed,timelineEntries:timeline,extractSubagentsFromStep,workerDisplayRows,
   stepNarrative:st=>st.thought||'',isAmbientKeyStep:()=>true,getAmbientAction:()=>({actionKind:'general'}),pushAmbientInterventions:()=>{}};
 const componentCode = ts.transpileModule(selected.map(n=>n.getText(ast)).join('\n') + '\n({getAmbientDisplayItems,getAmbientWorkerRows,getStepActiveWorkers,ambientWorkerRows});',
   {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
 const component = vm.runInNewContext(componentCode,ctx);
 const initialItems = component.getAmbientDisplayItems(timeline.value[0]);
 const initialWorkerRows = component.ambientWorkerRows.value;
 assert.equal(initialItems.length,37);
 assert.equal(component.getAmbientWorkerRows(props.steps[0]).length,2);
 assert.equal(ordinary.reduce((sum,st)=>sum+component.getAmbientWorkerRows(st).length,0),0);
 for(let i=0;i<100;i++) {
  progress({type:'child_reviewer_stream',child_run_id:c,stream_type:'thinking',chunk:'live'});
  assert.equal(component.getAmbientDisplayItems(timeline.value[0]),initialItems,'token stream must not regroup full timeline');
  assert.equal(component.ambientWorkerRows.value,initialWorkerRows,'token stream must not reproject brief rows');
 }
 // Main-agent narrative and new steps are still allowed to invalidate the cached grouping.
 props.steps[0].thought='new main narrative';
 assert.notEqual(component.getAmbientDisplayItems(timeline.value[0]),initialItems);
 props.steps.push(step('step-99','local_file',{},'ok'));
 assert.equal(component.getAmbientDisplayItems(timeline.value[0]).filter(i=>i.type==='step').length,38);
 const count = Array.from({length:35},(_,i)=>step(`step-${i+20}`,'local_shell',{},'ok')).reduce((n,st)=>n+getStepWorkers(st,s.teamWorkers.value).length,0);
 assert.equal(count,0,'35 ordinary steps produce no repeated child rows');
 console.log('PASS: exact dispatch/wait/history association, batch partial failure, failover, resume, both real event channels, 100-chunk reactivity regression');
}
main().then(()=>process.exit(0),e=>{console.error(e);process.exit(1)});
