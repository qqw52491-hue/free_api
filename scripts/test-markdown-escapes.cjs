// Exercise the real Markdown formatter, including the text used by code-copy buttons.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const root = path.resolve(__dirname, '..');
const file = path.join(root, 'src/components/agent/format.ts');
const compiled = ts.transpileModule(fs.readFileSync(file, 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
}).outputText;
const m = { exports: {} };
vm.runInThisContext('(function(require,module,exports){' + compiled + '\n})', { filename: file })(id => {
  if (id.endsWith('.css')) return {};
  if (id === '@tauri-apps/api/core') return { convertFileSrc: value => value };
  return require(id);
}, m, m.exports);
const { renderMarkdown, normalizeStreamingMarkdown } = m.exports;

function copiedCode(markdown) {
  const html = renderMarkdown(markdown);
  const match = html.match(/class="code-copy-btn" data-code="([^"]*)"/);
  assert(match, 'the report still renders a copyable code block');
  return decodeURIComponent(match[1]);
}

// Same shape as an audit response in a final report: escaped Markdown inside JSON.
const packet = { code: 0, data: [{ model_output: '## 回复\n\n一段回答\n', detail: 'A\tB' }] };
const json = JSON.stringify(packet, null, 2);
const report = '### 接口返回\n\n```json\n' + json + '\n```\n\n后续说明。';
const copied = copiedCode(report);
assert.equal(copied, json, 'JSON escapes must survive rendering and copying verbatim');
assert.deepEqual(JSON.parse(copied), packet, 'rendered JSON must remain valid and retain the original values');

for (const [lang, code] of [
  ['js', String.raw`const lines = text.split(/\n/); const tab = "\t";`],
  ['python', String.raw`print("first\nsecond\tcolumn")`],
  ['text', String.raw`C:\new\temp\notes.txt`],
  ['diff', String.raw`-const separator = "\t";` + '\n' + String.raw`+const separator = "\n";`],
]) {
  assert.equal(copiedCode('```' + lang + '\n' + code + '\n```'), code, lang + ' code-copy content must be lossless');
}

const inline = String.raw`使用 \n 与 \t；正则写成 ` + '`' + String.raw`/\n\t/` + '`。';
assert.equal(normalizeStreamingMarkdown(inline), inline, 'literal escapes in prose and inline code are not transport encoding');
assert.equal(normalizeStreamingMarkdown('### 标题\n\n第一段。\n\n第二段。'), '### 标题\n\n第一段。\n\n第二段。');
assert(renderMarkdown('### 标题\n\n第一段。\n\n第二段。').includes('<h3'), 'real Markdown line breaks still render normally');
const unfinished = '```js\n' + String.raw`const text = "a\nb";`;
assert.equal(normalizeStreamingMarkdown(unfinished), unfinished + '\n```', 'streaming fence completion must preserve escapes');
assert.equal(copiedCode(unfinished), String.raw`const text = "a\nb";`);
console.log('PASS Markdown escapes: audit JSON, copy fidelity, regex, strings, Windows paths, diff, inline/prose, real newlines, unfinished streaming fences');
