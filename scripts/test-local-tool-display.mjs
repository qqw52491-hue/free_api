// Run: node scripts/test-local-tool-display.mjs
// Use the installed TypeScript compiler to execute the actual pure display code.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
const cache = new Map();
function load(file) {
    file = path.resolve(file);
    if (cache.has(file)) return cache.get(file).exports;
    const module = { exports: {} }; cache.set(file, module);
    const code = ts.transpileModule(fs.readFileSync(file, 'utf8'), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const require = name => {
        assert.ok(name.startsWith('.'), `Unexpected runtime dependency: ${name}`);
        return load(path.resolve(path.dirname(file), `${name}.ts`));
    };
    vm.runInNewContext(code, { module, exports: module.exports, require }, { filename: file });
    return module.exports;
}
const { computeFormatStepOperation: format } = load('src/components/agent/diff/toolOperation.ts');
const cases = [
    ['default read', { tool:'local_file', params:{ path:'a.vue', start_line:440, end_line:560 } }, '读取'],
    ['logged tool-name action', { tool:'local_file', action:'local_file', params:{ path:'a.vue' } }, '读取'],
    ['inner read', { tool:'local_file', action:'local_file', params:{ action:'read', path:'a.vue' } }, '读取'],
    ['inner list', { tool:'local_file', action:'local_file', params:{ action:'list', path:'.' } }, '目录'],
    ['inner write', { tool:'local_file', action:'local_file', params:{ action:'write', path:'a.vue', content:'' } }, '写入'],
    ['explicit edit', { tool:'local_file', action:'edit', params:{ path:'a.vue', target_content:'a', replacement_content:'b' } }, '修改'],
    ['explicit read', { tool:'local_file', action:'read', params:{ path:'a.vue' } }, '读取'],
    ['serialized params', { tool:'local_file', action:'local_file', params:'{"path":"a.vue","action":"list"}' }, '目录'],
    ['nested params', { tool:'local_file', action:'local_file', params:{ command:{action:'read', path:'a.vue'} } }, '读取'],
    ['local shell', { tool:'local_shell', action:'local_shell', params:{command:'pwd'} }, '终端'],
];
for (const [name,step,label] of cases) {
    const display = format(step);
    assert.equal(display.badgeLabel,label,name);
    assert.notEqual(display.badgeIcon,'🧩',name);
}
assert.equal(format(cases[0][1]).lineRange,'#L440-560');
assert.equal(format({ tool:'remote/custom_tool',params:{} }).badgeIcon,'🧩');
// Execute the actual pure Vue classifier, without mounting the UI.
const vue = fs.readFileSync('src/components/agent/AgentChatStream.vue','utf8');
const classifier = vue.match(/function isMcpStep\([\s\S]*?\n\}/)?.[0];
assert.ok(classifier,'isMcpStep not found');
const code = ts.transpileModule(`${classifier}\nexports.isMcpStep = isMcpStep;`,{
    compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}
}).outputText;
const box = { exports:{} };vm.runInNewContext(code,box);
for (const tool of ['local_file','local_shell']) {
    assert.equal(box.exports.isMcpStep({tool,source:'MCP'},{badgeIcon:'🧩'}),false);
}
assert.equal(box.exports.isMcpStep({tool:'remote/local_file',source:'MCP'}),true);
assert.equal(box.exports.isMcpStep({tool:'remote/custom_tool'}),true);
console.log('Local tool display regression tests passed');
