// Run: node scripts/test-agent-read-view.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const { parse, compileScript } = require('@vue/compiler-sfc');

function evaluate(source, require, globals = {}) {
    const box = { exports: {} };
    const code = ts.transpileModule(source, {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    vm.runInNewContext(code, { module: box, exports: box.exports, require, console, setTimeout, clearTimeout, ...globals });
    return box.exports;
}

const readOutput = evaluate(fs.readFileSync('src/components/agent/views/readOutput.ts', 'utf8'), require);
const { readOutputLines, highlightReadLines, highlightReadText } = readOutput;
const original = '📄 文件: 目录/canvas.vue | 共 1235 行\n---\n 490:         <div>中文</div>\n 491:             <span>内容</span>\n 492:         </div>\n';
const rows = readOutputLines(original, 490, 492);
assert.equal(rows[0].text, '📄 文件: 目录/canvas.vue | 共 1235 行');
assert.equal(rows[0].number, null);
assert.equal(rows[2].number, 490);
assert.equal(rows[2].text, '<div>中文</div>');
assert.equal(rows[3].text, '    <span>内容</span>', 'relative indentation is preserved');
assert.equal(original.includes('490:         <div>中文</div>'), true, 'original output remains unchanged');
assert.equal(readOutputLines(' 490: a\n 492: b', 490, 492), null, 'gaps must fall back to original output');
assert.equal(readOutputLines(' 491: a', 490, 492), null, 'wrong start must fall back');
assert.equal(readOutputLines(' 490: a\n 491: b', 490, 490), null, 'outside range must fall back');
assert.equal(readOutputLines('plain unnumbered output', 1, 2), null);
assert.equal(readOutputLines(' 490: \t中文\r\n 491: \t\t下一行\r\n', 490, 491)[1].text, '\t下一行');

// Highlight the entire snippet before splitting rows so multiline syntax remains intact.
const commentRows = readOutputLines(' 1: /* comment\n 2: still a comment */\n 3: const value = "中文";', 1, 3);
const colored = highlightReadLines(commentRows, 'file.ts');
assert(colored[0].html.includes('hljs-comment'));
assert(colored[1].html.includes('hljs-comment'));
assert(colored[2].html.includes('hljs-keyword'));
assert(colored[2].html.includes('hljs-string'));
const unescape = html => html.replace(/<[^>]*>/g, '').replace(/&(amp|lt|gt|quot|#x27);/g, (_, entity) => ({amp:'&',lt:'<',gt:'>',quot:'"','#x27':"'"})[entity]);
for (let i = 0; i < colored.length; i++) {
    assert.equal(unescape(colored[i].html), commentRows[i].text, 'highlighting preserves code text');
    assert.equal((colored[i].html.match(/<span\b/g) || []).length, (colored[i].html.match(/<\/span>/g) || []).length, 'each row closes its spans');
}
const unsafe = '<img src=x onerror="alert(1)"> & 中文';
for (const filename of ['a.vue', 'a.ts', 'unknown.txt']) {
    const html = highlightReadText(unsafe, filename);
    assert(!html.includes('<img'), 'file content must never create executable HTML');
    assert.equal(unescape(html), unsafe);
}
assert(highlightReadText('<button class="run">运行</button>', 'canvas.vue').includes('hljs-name'));

// Execute the real compiled setup: an immediate watcher used to read activeTab before initialization.
const descriptor = parse(fs.readFileSync('src/components/agent/AgentInspector.vue', 'utf8')).descriptor;
const component = evaluate(compileScript(descriptor, { id: 'read-inspector-regression' }).content, name => {
    if (name === 'vue') return vue;
    if (name === './workerUtils') return { extractSubagentsFromStep: () => [] };
    return {}; // Child rendering and backend calls are outside this setup regression.
}).default;

async function main() {
    const props = vue.reactive({ selectedStep: { id: 'read-first', stepId: 0, tool: 'local_file' }, steps: [], resources: [], llmParseErrors: [] });
    const scope = vue.effectScope();
    const state = scope.run(() => component.setup(props, { expose() {} }));
    assert.equal(state.activeTab.value, 'detail');
    assert.equal(state.selectedAgentId.value, 'master');
    state.activeTab.value = 'team';
    props.selectedStep = { id: 'read-next', stepId: 1, tool: 'local_file' };
    await vue.nextTick();
    assert.equal(state.activeTab.value, 'detail', 'selecting an ordinary step exits the team tab');
    scope.stop();

    // Use the real copy handler; visual common-indent folding must not alter copied evidence.
    let copied;
    const cleanup = [];
    const expandable = evaluate(fs.readFileSync('src/components/agent/views/useExpandable.ts', 'utf8'), () => vue);
    const readDescriptor = parse(fs.readFileSync('src/components/agent/views/ReadView.vue', 'utf8')).descriptor;
    const readComponent = evaluate(compileScript(readDescriptor, { id: 'read-copy-regression' }).content, name => {
        if (name === 'vue') return { ...vue, onBeforeUnmount: callback => cleanup.push(callback) };
        if (name === './useExpandable') return expandable;
        if (name === './readOutput') return readOutput;
        return {};
    }, { navigator: { clipboard: { async writeText(text) { copied = text; } } } }).default;
    const readState = readComponent.setup({ view: { path: '目录/canvas.vue', start_line: 490, end_line: 492 }, detail: true, output: original }, { expose() {} });
    await readState.copyOutput();
    assert.equal(copied, original, 'copy preserves the exact original output including indentation');
    assert.equal(readState.copyLabel.value, '已复制');
    cleanup.forEach(callback => callback());
    console.log('PASS: numbered output, multiline syntax highlighting, escaped HTML, Unicode/CRLF, initial selection, tab switching and exact original copy');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
