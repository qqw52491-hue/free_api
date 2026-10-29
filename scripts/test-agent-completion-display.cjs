// Exercise real session events and the actual timeline projection; no model or Tauri calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const {session, emit} = require('./test-agent-task-resume.cjs');
const component = fs.readFileSync(path.resolve(__dirname, '../src/components/agent/AgentChatStream.vue'), 'utf8');
const source = component.split('<script setup lang="ts">')[1].split('</script>')[0];
// Compile the actual pending-status element rather than inventing a parallel view model.
const compiler = require('@vue/compiler-dom');
const templateAst = compiler.parse(require('@vue/compiler-sfc').parse(component).descriptor.template.content);
function findPending(node) {
    if (node.type === 1 && node.props.some(p => p.type === 7 && p.name === 'if' && p.exp?.content === 'completionPending && isRunning && !completionMessage')) return node;
    for (const child of node.children || []) {
        const found = findPending(child);
        if (found) return found;
    }
}
const pendingNode = findPending(templateAst);
const pendingTemplate = pendingNode?.loc.source;
assert(pendingTemplate, 'pending status must be mounted in the real template');
const pendingRender = new Function('Vue', compiler.compile(pendingTemplate, {mode:'function'}).code)(vue);
const pendingView = (s, overrides={}) => pendingRender({completionPending:s.completionPending.value,
    isRunning:s.isRunning.value,completionMessage:s.completionMessage.value,...overrides}, []);
function viewText(node) {
    if (typeof node === 'string') return node;
    if (Array.isArray(node)) return node.map(viewText).join('');
    return node?.type === vue.Comment ? '' : viewText(node?.children || '');
}
const ast = ts.createSourceFile('component.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const projection = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'currentTimeline');
assert(projection, 'test must execute the real currentTimeline');
const code = ts.transpileModule(projection.getText(ast) + '\ncurrentTimeline;', {
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022},
}).outputText;
function timeline(s) {
    const props = {historyTurns: [], humanInterventions: []};
    for (const key of ['steps','currentGoal','isRunning','completionMessage','finalSummary','finalArtifacts','hasError','runState','isThinking','thought']) {
        Object.defineProperty(props, key, {get: () => s[key].value});
    }
    const current = vm.runInNewContext(code, {
        props, activeThoughtStartedAt: {value: 0},
        planEntries: () => [], assignInterventionAnchors: () => {},
    });
    return () => current().filter(entry => entry.type === 'completion');
}
const progress = p => emit('agent-progress-existing-session', p);
const tool = p => emit('agent-tool-event-existing-session', p);
const state = (state, message) => emit('agent-run-state-existing-session', {state, message});
async function start() {
    const s = await session('failed');
    await emit('channel-run-started', {session_id:'existing-session', goal:'final answer regression'});
    return s;
}
function draft(text, done = false) { progress({type:'summary_stream', content:text, done}); }
async function main() {
    const fixtures = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../test/fixtures/finish-summary-fragments.json'), 'utf8'));
    for (const fixture of fixtures) {
        let s = await start();
        progress({type:'complete',success:true,finish:{summary:fixture.raw}});
        assert.equal(s.finalSummary.value, fixture.expected, `live: ${fixture.name}`);
        s = await session('succeeded', {
            traces: [{id:'saved-run',goal:'review',status:'succeeded',started_at:'2026-10-13T08:00:00Z',
                completed_at:'2026-10-13T08:01:00Z',steps:[],completion:{summary:fixture.raw}}],
            messages: [{id:'saved-message',role:'assistant',content:fixture.raw,created_at:'2026-10-13T08:01:00Z'}],
        });
        assert.equal(s.historyTurns.value[0].summary, fixture.expected, `history: ${fixture.name}`);
        assert.equal(s.historyTurns.value[0].messages.length, 0, 'saved final message must not be displayed twice');
        assert.equal(s.conversationMessages.value[0].content, fixture.raw, 'raw conversation record remains unchanged');
    }
    let s = await start();
    let entries = timeline(s);
    assert.equal(entries().length, 0);
    const long = '已完成全部修复（尚未验收的草稿）\n'.repeat(2500);
    let summaryChanges = 0, pendingChanges = 0;
    const stopSummary = vue.watch(s.finalSummary, () => summaryChanges++, {flush:'sync'});
    // Vue computed must not notify the UI for each long-summary chunk.
    assert(s.completionPending, 'session exposes only a boolean delivery-in-progress signal');
    const stopPending = vue.watch(s.completionPending, () => pendingChanges++, {flush:'sync'});
    for (let n = 128; n < long.length; n += 128) {
        draft(long.slice(0,n));
        assert.equal(s.finalSummary.value, '', 'unaccepted draft must not become a final summary');
        assert.equal(entries().length, 0, 'no final response while generating a summary');
    }
    // The UI coalesces streaming chunks; the real final chunk flushes the batch.
    draft(long, true);
    assert.equal(s.completionPending.value, true);
    assert.equal(pendingView(s).type, pendingNode.tag);
    assert(viewText(pendingView(s)).includes('尚未正式交付'));
    assert(!viewText(pendingView(s)).includes(long), 'status never interpolates the long candidate body');
    assert.equal(pendingView(s,{isRunning:false}).type, vue.Comment);
    assert.equal(pendingView(s,{completionMessage:'已提交'}).type, vue.Comment);
    assert.equal(summaryChanges, 0);
    assert.equal(pendingChanges, 1, 'unchanged status must not repaint on every chunk');
    progress({type:'step_new', step:{id:7,tool:'finish',description:'提交验收',command:{summary:long}}});
    tool({event_type:'started',step_id:7,tool:'finish',params:{summary:long}});
    assert.equal(entries().length,0);
    tool({event_type:'finish_challenged',step_id:7,tool:'finish',success:false,result:'证据不足，补正后重交'});
    assert.equal(s.streamingSummary.value,'');
    assert.equal(s.completionPending.value,false);
    assert.equal(pendingView(s).type,vue.Comment);
    assert.equal(entries().length,0,'rejection never retracts a previously published final response');
    assert.equal(s.steps.value.at(-1).status,'challenged');
    assert.equal(s.steps.value.at(-1).rawParams.summary,long,'attempted draft remains inspectable in tool details');
    stopSummary(); stopPending();

    // A non-streaming finish still shows a short pending state.
    progress({type:'step_new',step:{id:8,tool:'finish',command:{summary:'修正后交付'}}});
    assert.equal(s.completionPending.value,true);
    await state('finalizing','正在整理最终状态');
    assert.equal(entries().length,0,'finalizing is not the completion event');
    tool({event_type:'succeeded',step_id:8,tool:'finish',success:true,result:'submitted'});
    assert.equal(entries().length,0,'a tool event alone cannot publish the final response');
    progress({type:'complete',success:true,message:'任务已通过验收并提交',finish:{summary:'修正后交付',details:'完整验收后说明',artifacts:['tmp/result.md']}});
    assert.equal(s.finalSummary.value,'修正后交付\n\n完整验收后说明');
    assert.equal(s.completionPending.value,false);
    assert.equal(entries().length,1);
    assert.equal(entries()[0].summary,s.finalSummary.value);
    assert.equal(entries()[0].failed,false);
    assert.equal(entries()[0].artifacts.length,1);
    draft('迟到的草稿');
    assert.equal(s.finalSummary.value,'修正后交付\n\n完整验收后说明');

    s = await start(); entries = timeline(s); draft(long);
    progress({type:'fatal_error',message:'审查通道失败，任务未完成',finish:{summary:long}});
    assert.equal(s.finalSummary.value,'','failed completion must not publish an attempted finish payload');
    assert.equal(s.streamingSummary.value,'');
    assert.equal(entries().length,1);
    assert.equal(entries()[0].summary,'审查通道失败，任务未完成');
    assert.equal(entries()[0].failed,true);
    assert.equal(entries()[0].outcome,'failed');

    s = await start(); entries = timeline(s); draft(long);
    await s.requestStop();
    assert.equal(s.streamingSummary.value,'');
    assert.equal(s.finalSummary.value,'');
    assert.equal(s.completionPending.value,false);
    assert.equal(entries().length,1);
    assert.equal(entries()[0].failed,true,'manual stop is not successful delivery');
    assert.equal(entries()[0].outcome,'cancelled','manual stop keeps its own presentation state');
    assert(!entries()[0].summary.includes(long));
    progress({type:'complete',success:true,finish:{summary:long}});
    assert.equal(s.finalSummary.value,'','late completion cannot revive a cancelled draft');

    s = await start(); entries = timeline(s); draft(long);
    const fullReason='连接失败，无法继续\n'+'完整错误诊断'.repeat(100);
    await state('failed',fullReason);
    assert.equal(entries()[0].summary,fullReason,'full failure reason is retained even when topbar description is shortened');
    assert.equal(entries()[0].failed,true);
    assert.equal(s.finalSummary.value,'');

    s = await start(); entries = timeline(s); draft(long);
    progress({type:'complete',success:false,message:'任务未完成，已停止',output:long});
    assert.equal(entries()[0].summary,'任务未完成，已停止');
    assert.equal(s.finalSummary.value,'');

    s = await start(); entries = timeline(s); draft(long);
    progress({type:'complete',success:true,message:'服务已确认完成'});
    assert.equal(s.finalSummary.value,'','missing result body must not promote an unverified stream fallback');
    assert.equal(entries()[0].summary,'服务已确认完成');

    for (const output of [JSON.stringify({summary:'旧版结构化结果',details:'完整说明'}),'旧版纯文本结果']) {
        s = await start(); entries = timeline(s);
        progress({type:'complete',success:true,output});
        assert.equal(entries().length,1,'legacy committed result remains supported');
        assert(s.finalSummary.value.includes('旧版'));
    }
    // Running=false, an artifact, or a caller-supplied draft is not confirmation.
    s = await start(); entries = timeline(s);
    s.finalArtifacts = {value:['tmp/not-yet-accepted.md']};
    s.finalSummary = {value:long};
    assert.equal(entries().length,0);
    s.isRunning.value = false;
    assert.equal(entries().length,0,'background stop without a completion message must not claim success');
    // A background-started new run cannot carry the predecessor's final answer.
    s = await start(); entries = timeline(s);
    progress({type:'complete',success:true,finish:{summary:'上轮已交付'}});
    assert.equal(entries().length,1);
    await emit('channel-run-started',{session_id:'existing-session',goal:'new background task'});
    assert.equal(s.completionMessage.value,'');
    assert.equal(s.finalSummary.value,'');
    assert.equal(entries().length,0);
    // Existing persisted completion suppresses the live duplicate.
    s = await start();
    progress({type:'complete',success:true,finish:{summary:'已提交'}});
    const p = {historyTurns:[{goal:s.currentGoal.value,status:'succeeded'}],humanInterventions:[]};
    for (const k of ['currentGoal','isRunning']) Object.defineProperty(p,k,{get:()=>s[k].value});
    assert.equal(vm.runInNewContext(code,{props:p})().length,0);
    console.log('PASS: streamed draft isolation, stable pending status, rejection, confirmed success, failure/cancel, legacy completion, artifact-only guard, persisted dedup; real event/computed/timeline paths');
}
main().then(()=>process.exit(0), e=>{console.error(e);process.exit(1);});
