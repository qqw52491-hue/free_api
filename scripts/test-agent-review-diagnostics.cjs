// Actual SFC setup/template, production projection, composable event path; no model or real clipboard.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const vue = require('vue');
const sfc = require('@vue/compiler-sfc');
const { renderToString } = require('@vue/server-renderer');
const { load, session, emit } = require('./test-agent-task-resume.cjs');
const root = path.resolve(__dirname, '..');
const { applyReviewProgress } = load(path.join(root, 'src/components/agent/reviewProgress.ts'));
const { reviewDiagnosis, reviewStatusLabel, buildReviewDiagnostics, copyReviewDiagnostics } = load(path.join(root, 'src/components/agent/reviewDiagnostics.ts'));
const filename = path.join(root, 'src/components/agent/AgentReviewProgress.vue');
const { descriptor } = sfc.parse(fs.readFileSync(filename, 'utf8'));
const code = ts.transpileModule(sfc.compileScript(descriptor, { id: 'review-diagnostic-test', inlineTemplate: true }).content, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const m = { exports: {} };
new Function('require', 'module', 'exports', code)(id => id === 'vue' ? vue : load(path.resolve(path.dirname(filename), id + '.ts')), m, m.exports);
const Component = m.exports.default;
const base = { type: 'finish_review_progress', review_id: 'attempt-1', member_key: 'member-1', role_label: '审查成员', model: 'test-model', started_at_ms: Date.now() + 100, phase: 'failed', failure_kind: 'timeout', description: 'FIXED_DESCRIPTION', elapsed_ms: 65000, round: 3, tool_steps: 2, tool_budget: 8, timeout_retries: 1, format_corrections: 1 };
const project = p => applyReviewProgress([], { ...base, ...p }, 0)[0];
async function render(reviews, isRunning = false) { return renderToString(vue.createSSRApp(Component, { reviews, isRunning })); }
function mount(props) {
    const node = tag => ({ tag, children: [], props: {}, parent: null });
    const renderer = vue.createRenderer({
        createElement: node, createText: text => ({ ...node('#text'), text }), createComment: text => ({ ...node('#comment'), text }),
        setText: (n, text) => { n.text = text; }, setElementText: (n, text) => { n.text = text; n.children = []; },
        parentNode: n => n.parent, nextSibling: n => n.parent?.children[n.parent.children.indexOf(n) + 1] || null,
        insert(n, parent, anchor) { if (n.parent) this.remove(n); n.parent = parent; const i = parent.children.indexOf(anchor); parent.children.splice(i < 0 ? parent.children.length : i, 0, n); },
        remove(n) { if (n.parent) { const i = n.parent.children.indexOf(n); if (i >= 0) n.parent.children.splice(i, 1); n.parent = null; } },
        patchProp: (n, key, _old, value) => { n.props[key] = value; },
    });
    const tree = node('root');
    const app = renderer.createApp({ setup: () => () => vue.h(Component, props) });
    app.mount(tree);
    const all = n => [n, ...n.children.flatMap(all)];
    return { tree, find: tag => all(tree).find(n => n.tag === tag), text: () => all(tree).map(n => n.text || '').join(' '), unmount: () => app.unmount() };
}
async function main() {
    for (const [phase, kind, expected] of [['passed', '', '终审通过'], ['rejected', '', '终审不通过'], ['failed', 'timeout', '模型响应超时'], ['failed', 'request', '模型调用失败'], ['failed', 'format', '模型输出格式无效'], ['failed', 'configuration', '模型配置加载失败'], ['cancelled', 'cancelled', '审查已停止'], ['incomplete', '', '结束原因未确认'], ['skipped', '', '未执行模型终审']]) {
        const r = project({ phase, failure_kind: kind });
        assert(reviewDiagnosis(r, false).title.includes(expected));
        const html = await render([r]);
        assert(html.includes(expected)); assert(html.includes('查看诊断详情')); assert(!html.includes('is-active'));
    }
    const old = project({ timeout_retries: undefined, format_corrections: undefined, failure_kind: 'arbitrary provider error' });
    assert.equal(old.timeoutRetries, null); assert.equal(old.failureKind, '');
    assert((await render([old])).includes('未知'));
    assert(reviewDiagnosis(project({ phase: 'responding' }), false).title.includes('原因未确认'));
    assert.equal(reviewDiagnosis(project({ phase: 'responding' }), true), null);
    const privateValue = 'sk-PRIVATE_SECRET /home/private/repo https://secret.invalid Bearer PRIVATE';
    const hostile = project({ role_label: privateValue, model: privateValue, description: privateValue, last_notice: privateValue, failure_kind: privateValue });
    const summary = buildReviewDiagnostics([hostile], false);
    for (const secret of ['PRIVATE_SECRET', '/home/private', 'secret.invalid', 'Bearer', 'PRIVATE']) assert(!summary.includes(secret));
    assert(summary.includes('逻辑请求：3 次')); assert(summary.includes('非完整历史'));
    let copied = '';
    Object.defineProperty(global, 'navigator', { configurable: true, value: { clipboard: { writeText: async text => { copied = text; } } } });
    const props = vue.reactive({ reviews: [hostile], isRunning: false });
    const ui = mount(props);
    assert.equal(ui.find('details').props.open, undefined, 'only extended diagnostics stay collapsed');
    const overview = ui.find('summary');
    const texts = node => [node.text || '', ...node.children.flatMap(texts)].join(' ');
    assert(texts(overview).includes('查看诊断详情'));
    const member = ui.find('article');
    const visible = member.children.filter(n => n.tag !== 'details').flatMap(texts).join(' ');
    assert(visible.includes('未完成'));
    assert(visible.includes(hostile.label), 'member identity is visible without expanding');
    assert(visible.includes('3 次逻辑请求'), 'request count is visible by default');
    assert(visible.includes('已取证 2/8 步'), 'tool statistics are visible by default');
    assert(visible.includes('超时重发 1 次'));
    assert(visible.includes('格式补正 1 次'));
    const liveProps = vue.reactive({ reviews: [project({ phase: 'tool', description: '正在取证：read_file（第 3/8 步）', timeout_retries: 0, format_corrections: 0 })], isRunning: true });
    const live = mount(liveProps);
    const liveText = live.find('article').children.filter(n => n.tag !== 'details').flatMap(texts).join(' ');
    assert(liveText.includes('read_file'), 'actual current action is visible before opening details');
    assert(!liveText.includes('超时重发 0 次'));
    assert(!liveText.includes('格式补正 0 次'));
    live.unmount();
    assert.equal(reviewStatusLabel(project({ phase: 'responding', stream_kind: 'thinking' }), true), '正在思考');
    assert.equal(reviewStatusLabel(project({ phase: 'responding', stream_kind: 'content' }), true), '正在生成结论');
    assert.equal(project({ stream_kind: 'arbitrary' }).streamKind, '');
    assert.equal(reviewStatusLabel(project({ phase: 'waiting' }), true), '等待响应');
    assert.equal(reviewStatusLabel(project({ phase: 'responding' }), true), '接收中');
    assert.equal(reviewStatusLabel(project({ phase: 'failed', failure_kind: 'format' }), true), '格式无效');
    assert.equal(reviewStatusLabel(project({ phase: 'rejected' }), false), '存在阻塞');
    await ui.find('button').props.onClick(); await vue.nextTick();
    assert.equal(copied, summary); assert(ui.text().includes('已复制脱敏诊断'));
    global.navigator.clipboard.writeText = async () => { throw Error('denied'); };
    await ui.find('button').props.onClick(); await vue.nextTick();
    assert.equal(ui.find('textarea').props.value, summary); assert(ui.text().includes('手动复制'));
    props.reviews = [project({ review_id: 'next-attempt' })]; await vue.nextTick();
    assert(!ui.find('textarea')); assert(!ui.text().includes('已复制'));
    ui.unmount();
    let removed = false, focused = false;
    global.HTMLElement = class {};
    const field = { value: '', setAttribute() {}, style: {}, select() {}, remove() { removed = true; } };
    const previous = new global.HTMLElement(); previous.focus = () => { focused = true; };
    global.document = { createElement: () => field, activeElement: previous, body: { appendChild() {} }, execCommand: () => true };
    assert(await copyReviewDiagnostics(summary)); assert.equal(field.value, summary); assert(removed && focused);
    global.document.execCommand = () => false; assert.equal(await copyReviewDiagnostics(summary), false);
    delete global.document; delete global.HTMLElement;
    const s = await session('failed');
    await emit('channel-run-started', { session_id: 'existing-session', goal: 'diagnostic event regression' });
    const send = p => emit('agent-progress-existing-session', { ...base, phase: 'waiting', failure_kind: '', ...p });
    send({}); assert.equal(s.reviewProgress.value.length, 1);
    s.clearSession(true); s.isRunning.value = false;
    assert.equal(s.reviewProgress.value.length, 1, 'completion cleanup retains review observations');
    send({ phase: 'cancelled', failure_kind: 'cancelled', elapsed_ms: 66000 });
    assert.equal(s.reviewProgress.value[0].phase, 'cancelled', 'known late terminal event fills diagnosis after cleanup');
    send({ review_id: 'unrelated-late-attempt', started_at_ms: base.started_at_ms + 1000 });
    assert.equal(s.reviewProgress.value[0].id, base.review_id, 'ended run does not introduce unknown attempts');
    send({ phase: 'responding', elapsed_ms: 67000 });
    assert.equal(s.reviewProgress.value[0].phase, 'cancelled');
    await emit('channel-run-started', { session_id: 'existing-session', goal: 'next task' });
    assert.equal(s.reviewProgress.value.length, 0);
    s.clearSession(); assert.equal(s.reviewProgress.value.length, 0);
    console.log('PASS review diagnostics: 9 outcomes, unknown legacy fields, whitelist privacy, real SFC copy/failure/reset, clipboard fallback, completion retention, late terminal events and new-run isolation');
}
module.exports = { render, project, css: descriptor.styles.map(s => s.content).join('\n') };
if (require.main === module) main().then(() => process.exit(0)).catch(e => { console.error(e); process.exit(1); });
