// Render the actual status component and exercise disclosure without Tauri or model calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const sfc = require('@vue/compiler-sfc');

const file = path.resolve(__dirname, '../src/components/agent/AgentOutcomeNotice.vue');
const descriptor = sfc.parse(fs.readFileSync(file, 'utf8')).descriptor;
const code = sfc.compileScript(descriptor, { id: 'outcome-test', inlineTemplate: true }).content;
const rich = { props: ['content'], render() { return vue.h('rich-body', { content: this.content }, this.content); } };
const moduleObject = { exports: {} };
vm.runInThisContext('(function(require,module,exports){' + ts.transpileModule(code, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText + '\n})')(id => id.endsWith('RichContentRenderer.vue') ? { default: rich, __esModule: true } : require(id), moduleObject, moduleObject.exports);
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
        const at = anchor ? parent.children.indexOf(anchor) : -1;
        parent.children.splice(at < 0 ? parent.children.length : at, 0, el);
    },
    remove: el => { if (el.parent) el.parent.children.splice(el.parent.children.indexOf(el), 1); },
});
function all(el, test) { return [el, ...el.children.flatMap(child => all(child, test))].filter(test); }
function text(el) { return el.type === '#comment' ? '' : el.text + el.children.map(text).join(''); }

async function main() {
    const props = vue.reactive({ kind: 'cancelled', content: '已停止本轮任务。\n\n**现有检查**仍可参考。' });
    const root = node('root');
    const app = renderer.createApp({ render: () => vue.h(Component, props) }); app.mount(root);
    const card = () => all(root, el => el.type === 'section')[0];
    const bodies = () => all(root, el => el.type === 'rich-body');
    assert(String(card().props.class).includes('is-cancelled'));
    assert.equal(card().props['aria-label'], '任务已停止');
    assert(text(root).includes('本轮尚未完成交付'));
    assert.equal(bodies()[0].props.content, props.content, 'short explanations retain original Markdown');
    assert.equal(all(root, el => el.type === 'details').length, 0);

    props.kind = 'failed'; props.content = '{"error":"request timeout","retryable":true}'; await vue.nextTick();
    assert(String(card().props.class).includes('is-failed'));
    assert.equal(card().props['aria-label'], '任务未能完成');
    const jsonBody = bodies()[0].props.content;
    assert(jsonBody.startsWith('```json\n'));
    assert.deepEqual(JSON.parse(jsonBody.slice(8, -4)), JSON.parse(props.content));

    const report = '本次同步未完成。\n\n' + 'java.sql.SQLException: operation failed\n    at example.Service.sync(Service.java:42)\n'.repeat(25) + '完整诊断末尾_END';
    props.content = report; await vue.nextTick();
    let disclosure = all(root, el => el.type === 'details')[0];
    assert.equal(disclosure.props.open, false);
    assert.equal(bodies().length, 1, 'large diagnostic body is not mounted before expansion');
    assert.equal(bodies()[0].props.content, '本次同步未完成。');
    assert(!text(root).includes('完整诊断末尾_END'));
    disclosure.props.onToggle({ target: { open: true } }); await vue.nextTick();
    assert.equal(bodies().length, 2);
    const full = bodies()[1].props.content;
    assert(full.startsWith('```text\n') && full.includes(report), 'all original diagnostics remain available');
    assert(text(root).includes('完整诊断末尾_END'));
    assert(text(root).includes('收起完整信息'));

    props.kind = 'cancelled'; props.content = '后续运行已停止。'; await vue.nextTick();
    assert.equal(bodies().length, 1, 'new status never carries stale diagnostics');
    assert.equal(card().props['aria-label'], '任务已停止');
    assert(!text(root).includes('完整诊断末尾_END'));
    app.unmount();
    console.log('PASS outcome notice: stop/failure states, original Markdown, structured JSON, lazy full diagnostics, disclosure and state reset');
}
main().then(() => process.exit(0)).catch(error => { console.error(error); process.exit(1); });
