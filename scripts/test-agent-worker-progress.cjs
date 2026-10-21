// Render the real member component and drive real session events, without a model.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const sfc = require('@vue/compiler-sfc');
const { session, emit, load } = require('./test-agent-task-resume.cjs');
const base = path.resolve(__dirname, '../src/components/agent');
const descriptor = sfc.parse(fs.readFileSync(path.join(base, 'AgentWorkerProgress.vue'), 'utf8')).descriptor;
const code = sfc.compileScript(descriptor, { id: 'worker-test', inlineTemplate: true }).content;
const moduleObject = { exports: {} };
vm.runInThisContext('(function(require,module,exports){' + ts.transpileModule(code, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText + '\n})')(require, moduleObject, moduleObject.exports);
const Component = moduleObject.exports.default;

function node(type, text = '') { return { type, text, children: [], props: {}, parent: null }; }
const renderer = vue.createRenderer({
    createElement: node, createText: text => node('#text', text), createComment: text => node('#comment', text),
    setText: (el, text) => el.text = text, setElementText: (el, text) => { el.text = text; el.children = []; },
    patchProp: (el, key, _old, value) => el.props[key] = value,
    parentNode: el => el.parent, nextSibling: el => el.parent?.children[el.parent.children.indexOf(el) + 1],
    insert: (el, parent, anchor) => {
        if (el.parent) el.parent.children.splice(el.parent.children.indexOf(el), 1);
        el.parent = parent;
        const i = anchor ? parent.children.indexOf(anchor) : -1;
        parent.children.splice(i < 0 ? parent.children.length : i, 0, el);
    },
    remove: el => { if (el.parent) el.parent.children.splice(el.parent.children.indexOf(el), 1); },
});
function all(el, predicate) { return [el, ...el.children.flatMap(child => all(child, predicate))].filter(predicate); }
function text(el) { return el.type === '#comment' ? '' : el.text + el.children.map(text).join(''); }
function toggle(el) { return all(el, child => child.props.class === 'worker-thought-toggle')[0]; }

async function main() {
    const s = await session('failed');
    await emit('channel-run-started', { session_id: 'existing-session', goal: 'member progress fixture' });
    const progress = payload => emit('agent-progress-existing-session', payload);
    progress({ type: 'action_phase', description: '主 Agent 正在等待会审' });
    const phase = s.actionPhaseText.value;
    progress({ type: 'child_reviewer_stream', child_run_id: 'review-A', role_label: '架构评审专家', stream_type: 'thinking', chunk: 'A 独立检查布局，保留原始思考。' });
    progress({ type: 'child_reviewer_stream', child_run_id: 'review-B', role_label: '性能评审专家', stream_type: 'thinking', chunk: 'B 独立检查性能。' });
    assert.equal(s.actionPhaseText.value, phase, 'child chunks must not replace the main Agent activity');
    const root = node('root');
    let inspected = '';
    const app = renderer.createApp({ render: () => vue.h('section', s.teamWorkers.value.map(worker =>
        vue.h(Component, { key: worker.id, worker, onInspect: () => inspected = worker.id }))) });
    app.mount(root);
    const cards = () => all(root, el => el.type === 'article');
    assert.equal(cards().length, 2);
    assert(text(cards()[0]).includes('A 独立检查布局'));
    assert(!text(cards()[0]).includes('B 独立检查性能'));
    assert(!text(cards()[0]).includes('代码质量审查员'), 'show the named reviewer once, without a redundant generic title');
    assert.equal(all(cards()[0], el => el.type === 'pre').length, 0, 'long thought stays collapsed initially');
    toggle(cards()[0]).props.onClick(); await vue.nextTick();
    assert.equal(toggle(cards()[0]).props['aria-expanded'], true);
    assert.equal(all(cards()[0], el => el.type === 'pre')[0].text, s.teamWorkers.value[0].thought);
    assert.equal(all(cards()[1], el => el.type === 'pre').length, 0, 'expansion is private to each physical child');
    progress({ type: 'child_reviewer_stream', child_run_id: 'review-B', stream_type: 'content', chunk: '{"decision":"approved","findings":[]}' });
    await vue.nextTick();
    assert(text(cards()[1]).includes('正在生成结论'));
    assert(!text(cards()[1]).includes('"decision"'), 'structured result fragments must not become a thought ticker');
    assert.equal(s.actionPhaseText.value, phase);
    progress({ type: 'child_reviewer_stream', child_run_id: 'review-A', stream_type: 'thinking', chunk: '\n追加来源 A，未串到 B。' });
    await vue.nextTick();
    assert(all(cards()[0], el => el.type === 'pre')[0].text.includes('追加来源 A'));
    assert(!text(cards()[1]).includes('追加来源 A'));
    all(cards()[0], el => el.props.class === 'worker-identity')[0].props.onClick();
    assert.equal(inspected, 'review-A');
    s.teamWorkers.value[0].status = 'succeeded'; s.teamWorkers.value[0].currentStep = '已回收结论';
    await vue.nextTick();
    assert(text(cards()[0]).includes('已回收结论'));
    assert.equal(all(cards()[0], el => el.props.class === 'worker-thought-preview').length, 0);
    assert(all(cards()[0], el => el.type === 'pre')[0].text.includes('追加来源 A'), 'finished thought remains available');
    const streamSource = fs.readFileSync(path.join(base, 'AgentChatStream.vue'), 'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
    const ast = ts.createSourceFile('stream.ts', streamSource, ts.ScriptTarget.Latest, true);
    const names = ['getPlanRunningPreview', 'getLine3DisplayText', 'getLine3FullText', 'getLine3Key'];
    const functions = ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text));
    const props = vue.reactive({ isThinking: false, thought: '主 Agent 已结束的旧思考', currentGoal: '任务' });
    const { getRunningStagePreview } = load(path.join(base, 'diff/stagePreview.ts'));
    const api = vm.runInNewContext(ts.transpileModule(functions.map(n => n.getText(ast)).join('\n') + '\n({getPlanRunningPreview,getLine3DisplayText,getLine3FullText,getLine3Key});', {
        compilerOptions: { target: ts.ScriptTarget.ES2022 },
    }).outputText, { props, getRunningStagePreview, liveThoughtInfo: vue.computed(() => ({ text: props.thought, lineKey: 1 })) });
    const entry = { id: 'p', title: '会审', isThinking: true, steps: [{ id: 'wait', tool: 'wait_jobs', params: {}, status: 'running', description: '等待两位审查员' }] };
    assert.notEqual(api.getPlanRunningPreview(entry).line3.actionKind, 'think', 'stale entry flag cannot promote old main thought to live thinking');
    assert(!api.getLine3DisplayText(entry).includes('旧思考'));
    props.isThinking = true; props.thought = '主 Agent 新思考';
    assert.equal(api.getLine3DisplayText(entry), props.thought);
    assert.equal(api.getLine3FullText(entry), props.thought);
    app.unmount();
    console.log('PASS member UI: real events/render/clicks, independent expansion/streams, no raw JSON ticker, completion retention, main progress source consistency');
}
main().then(() => process.exit(0), e => { console.error(e); process.exit(1); });
