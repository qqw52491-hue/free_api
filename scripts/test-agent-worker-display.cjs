// Real Vue reactivity/composable + host-shaped receipts, with no model or database writes.
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const ts = require('typescript');
const vm = require('node:vm');
const vue = require('vue');
const { load, session, emit } = require('./test-agent-task-resume.cjs');
const base = path.resolve(__dirname, '../src/components/agent');
const { getStepJobIds, getStepWorkers, parseChildJobReceipts, workerDisplayRows, workerRowOwners } = load(path.join(base, 'workerAssociation.ts'));
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
 assert.deepEqual(getStepJobIds(step('step-9-review','resume_child',{child_run_id:a},`审查专家对质已启动：child_run_id=${c}。继承原任务上下文；其他成员结论保留。`)),[c]);
 const originalReview = step('step-review-origin','child_reviewer',{role_label:'架构评审专家'},`子审查任务已异步派发，child_run_id: ${a}。`);
 const resumedReview = step('step-review-resumed','resume_child',{child_run_id:a},`审查专家对质已启动：child_run_id=${c}。继承原任务上下文；其他成员结论保留。`);
 const resumedWait = step('step-review-wait','wait_jobs',{job_ids:[c]},result(c,'succeeded','child_reviewer'));
 const restoredReview = extractSubagentsFromStep(resumedReview,[originalReview,resumedReview,resumedWait])[0];
 assert.equal(restoredReview.id,c);
 assert.equal(restoredReview.roleLabel,'架构评审专家','history follows exact predecessor identity for display metadata');
 assert.equal(restoredReview.extra.resumed,true);
 assert.equal(restoredReview.durationMs,10021,'new attempt retains only its own terminal evidence');
 assert.equal(extractSubagentsFromStep(resumedReview,[resumedReview])[0].roleLabel,'审查','missing predecessor remains generic, no guessed expert');
 assert.deepEqual(getStepJobIds(step('step-10','child_worker',{},`未派发；任务描述说 child_run_id=${a}`)),[], 'task prose is not a receipt');
 const shellId = 'job-archive-check';
 const shellWait = step('step-shell','wait_jobs',{job_ids:[shellId]},result(shellId,'succeeded','shell'));
 assert.deepEqual(extractSubagentsFromStep(shellWait,[shellWait]),[], 'shell receipt is not a child Agent in history');
 const shellTimeout = step('step-shell-timeout','wait_jobs',{job_ids:[shellId]},'等待结束：仍有 1 个目标后台任务在运行');
 assert.deepEqual(extractSubagentsFromStep(shellTimeout,[shellTimeout]),[], 'unknown waiting ID cannot invent a scout');
 const mixedWait = step('step-mixed','wait_jobs',{job_ids:[a,shellId]},result(a)+result(shellId,'succeeded','shell'));
 assert.deepEqual(extractSubagentsFromStep(mixedWait,[dispatch,mixedWait]).map(w=>w.id),[a], 'mixed wait retains real children only');
 const s = await session('failed');
 await emit('channel-run-started',{session_id:'existing-session',run_id: 'fixture-run-59', goal:'worker regression'});
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
 // Evaluate the actual component's caches/grouping functions, not a copy of their logic.
 const names = ['historicalStepWorkers','liveStepWorkers','historicalWorkerRows','liveWorkerRows','ambientDisplayItems'];
 const fnNames = ['getTimelineRepeat','buildAmbientDisplayItems','getAmbientDisplayItems','getStepActiveWorkers','getAmbientWorkerRows'];
 const selected = ast.statements.filter(n => ts.isFunctionDeclaration(n) && fnNames.includes(n.name?.text)
   || ts.isVariableStatement(n) && n.declarationList.declarations.some(d => names.includes(d.name.getText(ast))));
 assert.equal(selected.length,names.length+fnNames.length);
 const ordinary = Array.from({length:35},(_,i)=>step(`step-${i+20}`,'local_shell',{},'ok'));
 const props = vue.reactive({steps:[dispatch,wait,...ordinary],historyTurns:[], get teamState(){return s.teamState.value;}});
 const timeline = vue.computed(()=>[{id:'plan',type:'plan',steps:props.steps}]);
 const ctx = {props,computed:vue.computed,extractSubagentsFromStep,workerRowOwners,
   isHistoryTurnCoveredByLive:()=>false,historicalWorkerSnapshot:worker=>worker,
   timelineRepeat:load(path.join(base,'timelineRepeats.ts')).timelineRepeat,
   stepInterventions:{value:new Map()},getStepReviewRows:()=>[],getStepHistoricalReviews:()=>[],
   stepNarrative:st=>st.thought||'',isAmbientKeyStep:()=>true,getAmbientAction:()=>({actionKind:'general'}),pushAmbientInterventions:()=>{}};
 const componentCode = ts.transpileModule(selected.map(n=>n.getText(ast)).join('\n') + '\n({getAmbientDisplayItems,getAmbientWorkerRows,getStepActiveWorkers,liveWorkerRows});',
   {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
 const component = vm.runInNewContext(componentCode,ctx);
 const initialItems = component.getAmbientDisplayItems(timeline.value[0]);
 assert.equal(initialItems.length,37);
 assert.equal(component.getAmbientWorkerRows(props.steps[0]).length,2);
 assert.equal(component.getAmbientWorkerRows(props.steps[1]).length,0,'wait summary must not duplicate dispatched member cards');
 const orphanWait = step('step-orphan','wait_jobs',{job_ids:[c]},result(c,'succeeded','child_reviewer'));
 const repeatWait = step('step-repeat','wait_jobs',{job_ids:[c]},result(c,'succeeded','child_reviewer'));
 props.steps.push(orphanWait,repeatWait);
 assert.equal(component.getAmbientWorkerRows(props.steps[props.steps.length-2]).length,1,'first known wait owns a member when formal plan dispatch has no row');
 assert.equal(component.getAmbientWorkerRows(props.steps[props.steps.length-1]).length,0);
 props.steps.splice(-2);
 const historicalDispatch = {...dispatch};
 const historicalWait = {...wait};
 props.historyTurns.push({steps:[historicalDispatch,historicalWait]});
 const history = props.historyTurns[0].steps;
 assert.equal(component.getAmbientWorkerRows(history[0]).length,2,'history owns its own member rows even with identical synthetic IDs');
 assert.equal(component.getAmbientWorkerRows(history[1]).length,0);
 assert.notEqual(component.getAmbientWorkerRows(history[0])[0],component.getAmbientWorkerRows(props.steps[0])[0],'live thoughts cannot leak into history');
 props.historyTurns.splice(0);
 assert.equal(ordinary.reduce((sum,st)=>sum+component.getAmbientWorkerRows(st).length,0),0);
 const stableItems = component.getAmbientDisplayItems(timeline.value[0]);
 const stableRows = component.liveWorkerRows.value;
 for(let i=0;i<100;i++) {
  progress({type:'child_reviewer_stream',child_run_id:c,stream_type:'thinking',chunk:'live'});
  assert.equal(component.getAmbientDisplayItems(timeline.value[0]),stableItems,'token stream must not regroup full timeline');
  assert.equal(component.liveWorkerRows.value,stableRows,'token stream must not reproject brief rows');
 }
 // Main-agent narrative and new steps are still allowed to invalidate the cached grouping.
 props.steps[0].thought='new main narrative';
 assert.notEqual(component.getAmbientDisplayItems(timeline.value[0]),initialItems);
 props.steps.push(step('step-99','local_file',{},'ok'));
 assert.equal(component.getAmbientDisplayItems(timeline.value[0]).filter(i=>i.type==='step').length,38);
 const count = Array.from({length:35},(_,i)=>step(`step-${i+20}`,'local_shell',{},'ok')).reduce((n,st)=>n+getStepWorkers(st,s.teamWorkers.value).length,0);
 assert.equal(count,0,'35 ordinary steps produce no repeated child rows');
 // Generic shell waits must not create fake scouts through either live event channel.
 const teamIdsBeforeShellWait = s.teamWorkers.value.map(w=>w.id);
 progress({type:'agent_waiting_jobs',job_ids:[shellId]});
 tool({type:'started',step_id:200,tool:'wait_jobs',action:'wait_jobs',params:{job_ids:[shellId]}});
 tool({type:'succeeded',step_id:200,tool:'wait_jobs',action:'wait_jobs',params:{job_ids:[shellId]},result:shellWait.rawResult});
 assert.deepEqual(s.teamWorkers.value.map(w=>w.id),teamIdsBeforeShellWait,'ordinary background job is never a child Agent card');
 // Parent failure must settle active child projections, retain completed results, and ignore late streams.
 progress({type:'complete',success:false,message:'审查通道失败，任务未完成'});
 assert.equal(s.runState.value,'failed');
 assert.equal(s.teamWorkers.value.find(w=>w.id===a).status,'succeeded');
 assert.equal(s.teamWorkers.value.find(w=>w.id===c).status,'failed');
 assert.equal(s.teamWorkers.value.find(w=>w.id===c).extra.stoppedByParent,true);
 progress({type:'worker_stream',child_run_id:c,delta:'late stream'});
 assert.equal(s.teamWorkers.value.find(w=>w.id===c).status,'failed');

 let syncTeam;
 window.addEventListener = (event, handler) => {
  if (event === 'agent:child-tasks-synced') syncTeam = handler;
 };
 const reopened = await session('failed');
 await emit('channel-run-started', {session_id:'existing-session', run_id:'resumed-run', goal:'继续原任务'});
 const saved = {child_run_id:a, parent_run_id:'original-run', result_available:true, collected_at:null, execution_status:'succeeded'};
 const graph = {
  sessionId:'existing-session', focus_run_id:'resumed-run', focus_run_ids:['original-run','resumed-run'],
  tasks:[
   {child_run_id:a, root_run_id:'original-run', task:'原派发子任务', depth:1, execution_status:'succeeded', handoff_accepted:true, summary:'保存的结果'},
   {child_run_id:b, root_run_id:'other-run', task:'同会话其他任务', depth:1, execution_status:'succeeded'},
  ],
  receipts:[saved], waiting_on:[{waiter_run_id:'resumed-run',target_ids:[a],kind:'jobs'}],
 };
 syncTeam({detail:graph});
 assert.equal(reopened.teamState.value.masterRunId, 'resumed-run');
 assert.deepEqual(reopened.teamState.value.workers.map(w=>[w.id,w.name]), [[a,'原派发子任务']], 'resume restores the original member only within the same logical task');
 assert.equal(reopened.teamState.value.receipts.get(a).collected_at, null);
 assert.equal(reopened.teamState.value.receipts.get(a).summary, '保存的结果');
 assert.deepEqual(reopened.teamState.value.waitingOn, graph.waiting_on);
 const displayedTeam = reopened.teamState.value;
 for (let i = 0; i < 10; i++) {
  syncTeam({detail:structuredClone(graph)});
  assert.equal(reopened.teamState.value, displayedTeam, 'identical polls keep the team projection stable');
 }
 syncTeam({detail:{...graph, receipts:[{...saved, collected_at:'2026-11-28T03:17:00Z'}], waiting_on:[]}});
 syncTeam({detail:{...graph, waiting_on:[]}});
 assert.equal(reopened.teamState.value.receipts.get(a).collected_at, '2026-11-28T03:17:00Z', 'an older snapshot cannot undo committed collection');
 assert.deepEqual(reopened.teamState.value.waitingOn, [], 'finished waits disappear');
 await emit('channel-run-started', {session_id:'existing-session', run_id:'unrelated-next-run', goal:'新任务'});
 syncTeam({detail:graph});
 assert.deepEqual(reopened.teamState.value.workers.map(w=>w.id), [], 'an old focus snapshot cannot repopulate a new run');

 const externalChild = {
  child_run_id:c, root_run_id:'unrelated-next-run', task:'核对模型路由', depth:1,
  execution_status:'running', latest_step:'正在读取会话配置',
  external_agent:{name:'本地 Codex',kind:'codex'},
 };
 const externalTeam = {sessionId:'existing-session',focus_run_id:'unrelated-next-run',tasks:[externalChild],
  waiting_on:[{waiter_run_id:'unrelated-next-run',target_ids:[c],kind:'jobs'}]};
 syncTeam({detail:externalTeam});
 const externalWorker = reopened.teamState.value.workers[0];
 assert.deepEqual(externalWorker.externalAgent, {name:'本地 Codex',kind:'codex'});
 assert.equal(externalWorker.currentStep, '正在读取会话配置');
 assert.equal(externalWorker.status, 'running');
 assert.equal(externalWorker.externalWaitReason, undefined);
 const waitingChild = {...externalChild,external_wait_reason:'等待 Codex 响应'};
 syncTeam({detail:{...externalTeam,tasks:[waitingChild]}});
 assert.equal(externalWorker.externalWaitReason, '等待 Codex 响应');
 assert.equal(externalWorker.waitingForHuman, false, 'external wait must not open a human prompt');
 syncTeam({detail:externalTeam});
 assert.equal(externalWorker.externalWaitReason, undefined, 'a fresh running snapshot clears the external wait');
 const returnedChild = {...waitingChild, execution_status:'succeeded',handoff_accepted:true,summary:'路由核对完成'};
 const returnedReceipt = {child_run_id:c,parent_run_id:'unrelated-next-run',execution_status:'succeeded',result_available:true,collected_at:null};
 const returnedTeam = {...externalTeam,tasks:[returnedChild],receipts:[returnedReceipt],waiting_on:[]};
 syncTeam({detail:returnedTeam});
 assert.equal(externalWorker.status, 'succeeded');
 assert.equal(externalWorker.externalWaitReason, undefined, 'the saved terminal state supersedes the live wait');
 assert.equal(reopened.teamState.value.receipts.get(c).collected_at, null, 'child completion alone is not collection');
 const collected = '2026-11-28T03:18:00Z';
 syncTeam({detail:{...returnedTeam,receipts:[{...returnedReceipt,collected_at:collected}]}});
 syncTeam({detail:{...externalTeam,tasks:[waitingChild]}});
 assert.equal(externalWorker.status, 'succeeded', 'late progress cannot reopen a settled external child');
 assert.equal(externalWorker.externalWaitReason, undefined);
 assert.equal(reopened.teamState.value.receipts.get(c).collected_at, collected);
 console.log('PASS: child association, batch partial failure, failover, resume, both event channels, reactivity, external wait and result collection');
}
main().then(()=>process.exit(0),e=>{console.error(e);process.exit(1)});
