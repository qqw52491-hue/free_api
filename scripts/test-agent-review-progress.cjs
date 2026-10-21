// Real composable event path + actual Vue template, no desktop app or model requests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const { renderToString } = require('@vue/server-renderer');
const { session, emit } = require('./test-agent-task-resume.cjs');
const root = path.resolve(__dirname, '..');
const source = fs.readFileSync(path.join(root, 'src/components/agent/reviewProgress.ts'), 'utf8');
const moduleObject = { exports: {} };
vm.runInThisContext('(function(exports){' + ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText + '\n})')(moduleObject.exports);
const { isReviewTerminal } = moduleObject.exports;
const component = fs.readFileSync(path.join(root, 'src/components/agent/AgentReviewProgress.vue'), 'utf8');
const template = component.split('<template>')[1].split('</template>')[0];
const render = new Function('Vue', require('@vue/compiler-dom').compile(template, { mode: 'function' }).code)(vue);
async function html(s, running = true) {
    return renderToString(vue.createSSRApp({ render: () => render({ reviews: s.reviewProgress.value, isRunning: running, isReviewTerminal }, []) }));
}
async function main() {
    const s = await session('failed');
    await emit('channel-run-started', { session_id: 'existing-session', goal: 'review progress regression' });
    const start = Date.now() + 100;
    const base = { type: 'finish_review_progress', review_id: 'attempt-A', member_key: 'finish-review-member-1', role_label: '安全审查员', model: 'test-flash', started_at_ms: start, phase: 'waiting', description: '请求已发起，等待模型响应', elapsed_ms: 0, round: 1, tool_steps: 0, tool_budget: 8, received_chars: 0 };
    const send = p => emit('agent-progress-existing-session', { ...base, ...p });
    send({}); assert.equal(s.reviewProgress.value.length, 1);
    const logs = s.logs.value.length;
    send({ elapsed_ms: 30000 });
    assert.equal(s.logs.value.length, logs, 'heartbeat updates time without log spam');
    let text = await html(s); assert(text.includes('30 秒')); assert(text.includes('等待模型响应'));
    send({ phase: 'tool', description: '正在取证：read_file（第 1/8 步）', elapsed_ms: 31000 });
    text = await html(s); assert(text.includes('read_file'));
    send({ review_id: 'attempt-B', member_key: 'finish-review-member-2', model: 'other-model', phase: 'format_retry', description: '输出格式不合规，正在纠正', elapsed_ms: 2000 });
    assert.equal(s.reviewProgress.value.length, 2); text = await html(s); assert(text.includes('other-model')); assert(text.includes('正在纠正'));
    send({ review_id: 'attempt-B', member_key: 'finish-review-member-2', model: 'other-model', phase: 'waiting', elapsed_ms: 3000, last_notice: '输出格式不合规，正在纠正' });
    text = await html(s); assert(text.includes('最近异常：输出格式不合规'), 'retry notice remains visible while waiting again');
    send({ phase: 'passed', description: '终审通过（不代表整个任务已经完成交付）', elapsed_ms: 32000, tool_steps: 1 });
    send({ phase: 'responding', elapsed_ms: 33000 });
    assert.equal(s.reviewProgress.value[0].phase, 'passed', 'late heartbeat cannot reopen terminal member');
    send({ review_id: 'fallback-A', started_at_ms: start + 1000, model: 'backup-model', elapsed_ms: 0 });
    assert.equal(s.reviewProgress.value.length, 2); assert.equal(s.reviewProgress.value[0].model, 'backup-model');
    send({ phase: 'failed', elapsed_ms: 99000 });
    assert.equal(s.reviewProgress.value[0].model, 'backup-model', 'late old attempt cannot overwrite fallback');
    send({ review_id: 'old-run', started_at_ms: 1, member_key: 'old-member' });
    assert.equal(s.reviewProgress.value.length, 2, 'prior run event must be ignored');
    text = await html(s, false); assert(text.includes('运行已结束')); assert(!text.includes('class="is-active"'), 'stop must not leave activity animation');
    assert(!text.includes('PRIVATE_THOUGHT'));
    assert(fs.readFileSync(path.join(root, 'src/components/AgentPanel.vue'), 'utf8').includes('<AgentReviewProgress :reviews="session.reviewProgress.value"'), 'component must be mounted in the real main view');
    await emit('channel-run-started', { session_id: 'existing-session', goal: 'next run' });
    assert.equal(s.reviewProgress.value.length, 0, 'new run clears previous review cards');
    console.log('PASS review progress: live events, heartbeat, tool, format correction, member isolation, fallback/late-event guard, stop, main-view rendering, new-run reset');
}
main().then(() => process.exit(0)).catch(e => { console.error(e); process.exit(1); });
