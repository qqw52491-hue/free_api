// Real Markdown lexer/renderer, production block grouping, and gallery Vue events.
// Tauri path conversion and native dialog methods are isolated; no external images/models.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const sfc = require('@vue/compiler-sfc');
const { marked } = require('marked');
const root = path.resolve(__dirname, '..');
const cache = new Map();
function evaluate(source, filename, imports) {
  const m = { exports: {} };
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText;
  vm.runInThisContext('(function(require,module,exports){' + code + '\n})', { filename })(imports, m, m.exports);
  return m.exports;
}
function load(file) {
  if (cache.has(file)) return cache.get(file);
  const result = evaluate(fs.readFileSync(file, 'utf8'), file, id => {
    if (id.endsWith('.css')) return {};
    if (id === '@tauri-apps/api/core') return { convertFileSrc: value => 'asset://localhost/' + encodeURIComponent(value) };
    if (id.startsWith('.')) return load(path.resolve(path.dirname(file), id + '.ts'));
    return require(id);
  });
  cache.set(file, result); return result;
}
const { renderMarkdown, resolveMarkdownMediaSource } = load(path.join(root, 'src/components/agent/format.ts'));
const { standaloneImages } = load(path.join(root, 'src/components/charts/markdownImages.ts'));
const componentFile = path.join(root, 'src/components/charts/MarkdownImageGallery.vue');
const descriptor = sfc.parse(fs.readFileSync(componentFile, 'utf8')).descriptor;
const Gallery = evaluate(sfc.compileScript(descriptor, { id: 'gallery-test', inlineTemplate: true }).content, componentFile, require).default;
const source = sfc.parse(fs.readFileSync(path.join(root, 'src/components/charts/RichContentRenderer.vue'), 'utf8')).descriptor.scriptSetup.content;
const ast = ts.createSourceFile('rich.ts', source, ts.ScriptTarget.Latest, true);
const grouping = ast.statements.filter(n => ts.isVariableStatement(n) && n.declarationList.declarations.some(d => ['parsedBlocks', 'MERMAID_KEYWORDS'].includes(d.name.getText(ast))));
const props = vue.reactive({ content: '' });
const blocks = vm.runInNewContext(ts.transpileModule(grouping.map(n => n.getText(ast)).join('\n') + '\nparsedBlocks;', { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText,
  { computed: vue.computed, props, marked, standaloneImages, renderSegmentMarkdown: renderMarkdown });
function parse(content) { props.content = content; return Array.from(blocks.value); }
const a = 'https://images.example.test/a.png', b = 'https://images.example.test/b.png';

function node(type, text = '') {
  const el = { type, text, children: [], props: {}, parent: null, open: false };
  el.showModal = () => el.open = true;
  el.close = () => { el.open = false; el.props.onClose?.(); };
  return el;
}
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
  }, remove: el => { if (el.parent) el.parent.children.splice(el.parent.children.indexOf(el), 1); },
});
function all(el, test) { return [el, ...el.children.flatMap(child => all(child, test))].filter(test); }
function byClass(el, cls) { return all(el, child => child.props.class === cls); }
function text(el) { return el.type === '#comment' ? '' : el.text + el.children.map(text).join(''); }
async function main() {
  let result = parse(`## 配图\n\n![A](${a})\n![B](${b})\n\n正文继续。`);
  assert.deepEqual(result.map(b => b.type), ['markdown', 'images', 'markdown']);
  assert.deepEqual(result[1].images.map(i => i.src), [a,b]);
  result = parse(`![A](${a})\n\n[![B](${b} "标题 B")](https://source.example.test/article)`);
  assert.equal(result.length, 1); assert.equal(result[0].images.length, 2, 'adjacent standalone paragraphs form one gallery');
  assert.equal(result[0].images[1].sourceHref, 'https://source.example.test/article');
  assert.equal(parse(`![A][ref]\n\n[ref]: ${a}`)[0].images[0].src, a, 'reference-style images remain standard Markdown');
  for (const markdown of [`正文 ![A](${a}) 继续。`, `\`![A](${a})\``, '```markdown\n![A](' + a + ')\n```', `> ![A](${a})`, `- ![A](${a})`, `[![A](${a})](/local/report.md)`]) {
    assert(parse(markdown).every(block => block.type === 'markdown'), 'mixed prose/quoted images/code/local links must not be swallowed by a gallery');
  }
  result = parse(`![视频](https://example.test/clip.mp4)\n\n![音频](https://example.test/audio.mp3)`);
  assert.equal(result[0].type, 'markdown'); assert(result[0].html.includes('<video')); assert(result[0].html.includes('<audio'));
  const local = '/tmp/真实图片.png';
  assert.equal(parse(`![本地](<${local}>)`)[0].images[0].src, resolveMarkdownMediaSource(local));
  assert(renderMarkdown(`![本地](<${local}>)`).includes(resolveMarkdownMediaSource(local)), 'gallery and ordinary Markdown use identical local mapping');
  for (const href of ['javascript:alert%281%29', 'vbscript:msgbox%281%29', 'data:text/html;base64,AAAA']) {
    assert.equal(parse(`![不安全](<${href}>)`)[0].images[0].src, '', 'executable/non-image data URI must not be bound to the gallery');
  }
  const data = 'data:image/png;base64,iVBORw0KGgo=';
  assert.equal(parse(`![内嵌](${data})`)[0].images[0].src, data);
  result = parse('## 标题\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n$E=mc^2$\n\n```js\nconst x = 1;\n```\n\n```mermaid\ngraph TD; A-->B\n```\n\n```echarts\n{"series":[]}\n```');
  assert.deepEqual(result.map(b => b.type), ['markdown', 'mermaid', 'echarts']);
  assert(result[0].html.includes('<table')); assert(result[0].html.includes('katex')); assert(result[0].html.includes('code-copy-btn'));
  result = parse(`![A](${a})\n\n分隔正文。\n\n![B](${b})`);
  assert.deepEqual(result.map(b => b.type), ['images', 'markdown', 'images']);
  assert.equal(parse('![尚未结束](https://images.example.test/')[0].type, 'markdown', 'partial streaming image syntax stays text until complete');

  const galleryProps = vue.reactive({ images: [{src:a,alt:'A',title:''},{src:b,alt:'B',title:'标题 B',sourceHref:'https://source.example.test/article'}] });
  const host = node('root');
  const app = renderer.createApp({ render: () => vue.h(Gallery, galleryProps) }); app.mount(host);
  assert.equal(byClass(host, 'image-figure').length, 2);
  assert.equal(all(host, el => el.type === 'a')[0].props.href, galleryProps.images[1].sourceHref);
  await byClass(host, 'image-thumbnail')[0].props.onClick(); await vue.nextTick();
  const dialog = byClass(host, 'image-preview-dialog')[0];
  assert(dialog.open); assert(text(dialog).includes('1 / 2'));
  all(dialog, el => el.props['aria-label'] === '下一张图片')[0].props.onClick(); await vue.nextTick();
  assert(text(dialog).includes('2 / 2')); assert.equal(all(dialog, el => el.type === 'img')[0].props.src, b);
  galleryProps.images.push({src:data,alt:'追加',title:''}); await vue.nextTick();
  assert(text(dialog).includes('2 / 2'), 'an opened preview retains its own snapshot while streaming appends');
  all(dialog, el => el.props['aria-label'] === '关闭图片预览')[0].props.onClick(); await vue.nextTick(); assert(!dialog.open);
  const thumb = byClass(host, 'image-thumbnail')[0];
  all(thumb, el => el.type === 'img')[0].props.onError(); await vue.nextTick();
  assert(byClass(host, 'image-thumbnail')[0].props.disabled); assert(text(host).includes('图片加载失败'));
  byClass(host, 'image-retry')[0].props.onClick(); await vue.nextTick();
  assert(!byClass(host, 'image-thumbnail')[0].props.disabled); assert(all(byClass(host, 'image-thumbnail')[0], el => el.type === 'img').length);
  const retriedSrc = all(byClass(host, 'image-thumbnail')[0], el => el.type === 'img')[0].props.src;
  assert(retriedSrc.includes('_image_retry=1')); assert.equal(galleryProps.images[0].src, a, 'retry must not rewrite the original Markdown source');
  await byClass(host, 'image-thumbnail')[0].props.onClick(); await vue.nextTick();
  assert.equal(all(dialog, el => el.type === 'img')[0].props.src, retriedSrc, 'preview uses the same retried observation');
  all(dialog, el => el.props['aria-label'] === '关闭图片预览')[0].props.onClick(); await vue.nextTick();
  galleryProps.images[0].src = a + '?signature=keep'; await vue.nextTick();
  all(byClass(host, 'image-thumbnail')[0], el => el.type === 'img')[0].props.onError(); await vue.nextTick();
  byClass(host, 'image-retry')[0].props.onClick(); await vue.nextTick();
  assert.equal(all(byClass(host, 'image-thumbnail')[0], el => el.type === 'img')[0].props.src, a + '?signature=keep', 'query-bearing URLs are retried verbatim to preserve signatures');
  app.unmount();
  console.log('PASS Markdown images: real grouping/rendering, reference/linked/local/data images, URI safety, streaming, unchanged prose/code/media/charts, native-dialog calls, navigation, source links, failure/retry');
}
main().then(() => process.exit(0), e => { console.error(e); process.exit(1); });
