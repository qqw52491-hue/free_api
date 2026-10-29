// Mount the real SFC using Vue's renderer with in-memory host nodes and mocked IPC.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const {parse, compileScript} = require('@vue/compiler-sfc');
const transpile = source => ts.transpileModule(source, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText;
const utilContext = {exports: {}};
vm.runInNewContext(transpile(fs.readFileSync('src/utils/modelConfigTransfer.ts', 'utf8')), utilContext);
const config = {version: 1, platforms: [{name: 'Platform A', base_url: 'https://example.test/v1', api_key: 'secret-test-key', models: [
  {name: 'model-a', display_name: 'Model A', max_tokens: 8192, temperature: 0.5, context_window: 128000, enabled: true, provider_capabilities: {}},
  {name: 'model-b', display_name: 'Model B', max_tokens: 4096, temperature: 0.7, context_window: 0, enabled: false, provider_capabilities: {}},
]}]};
const calls = [], copied = [], emitted = [];
let failImport = false, savePath = '/tmp/model-config-test.json';
const invoke = async (name, args) => {
  calls.push({name, args: JSON.parse(JSON.stringify(args))});
  if (name === 'export_model_config') return JSON.parse(JSON.stringify(config));
  if (name === 'preview_model_config') {
    const value = JSON.parse(args.json);
    if (!Array.isArray(value.platforms)) throw new Error('invalid format');
    return value;
  }
  if (name === 'import_model_config') {
    if (failImport) throw new Error('test transaction failure');
    return {added_platforms: 1, reused_platforms: 0, added_models: args.config.platforms[0].models.length, skipped_models: 0, platforms_without_key: 0};
  }
  if (name === 'save_model_config_file') return;
  throw new Error(`Unexpected IPC: ${name}`);
};
const source = fs.readFileSync('src/components/ModelConfigTransfer.vue', 'utf8');
const {descriptor} = parse(source);
const script = compileScript(descriptor, {id: 'model-config-test', inlineTemplate: true});
const context = {
  exports: {}, console, HTMLElement: class {},
  document: {activeElement: null, querySelector: () => ({focus() {}})},
  navigator: {clipboard: {writeText: async text => {copied.push(text);}}},
  require: name => {
    if (name === 'vue') return vue;
    if (name === '@tauri-apps/api/core') return {invoke};
    if (name === '@tauri-apps/plugin-dialog') return {save: async () => savePath};
    if (name === '../utils/modelConfigTransfer') return utilContext.exports;
    throw new Error(name);
  },
};
vm.runInNewContext(transpile(script.content), context);
global.document = context.document;
const node = (type, text = '') => ({type, tagName: type.toUpperCase(), text, props: {}, children: [], parent: null, addEventListener() {}, removeEventListener() {}});
const body = node('body'), root = node('root');
const renderer = vue.createRenderer({
  createElement: tag => node(tag), createText: text => node('#text', text), createComment: text => node('#comment', text),
  setText: (n, text) => {n.text = text;}, setElementText: (n, text) => {n.text = text; n.children = [];},
  patchProp: (n, key, _previous, next) => {n.props[key] = next;},
  insert(n, parent, anchor) {
    if (n.parent) n.parent.children.splice(n.parent.children.indexOf(n), 1);
    n.parent = parent;
    const index = anchor ? parent.children.indexOf(anchor) : -1;
    if (index < 0) parent.children.push(n); else parent.children.splice(index, 0, n);
  },
  remove(n) {if (n.parent) n.parent.children.splice(n.parent.children.indexOf(n), 1);},
  parentNode: n => n.parent,
  nextSibling: n => n.parent?.children[n.parent.children.indexOf(n) + 1],
  querySelector: () => body,
});
const app = renderer.createApp(context.exports.default, {onImported: () => emitted.push('imported')});
app.mount(root);
const all = () => {const out = []; const visit = n => {out.push(n); n.children.forEach(visit);}; visit(root); visit(body); return out;};
const text = n => n.text + n.children.filter(c => c.type !== '#comment').map(text).join('');
const find = predicate => {const n = all().find(predicate); assert(n, 'Expected rendered element'); return n;};
const button = label => find(n => n.type === 'button' && text(n) === label);
const settle = async () => {for (let i = 0; i < 8; i++) {await Promise.resolve(); await vue.nextTick();}};
const click = async label => {const n = button(label); assert(!n.props.disabled, `${label} should be enabled`); await n.props.onClick(); await settle();};
const inputJson = async value => {
  const input = find(n => n.type === 'textarea' && !n.props.readonly);
  input.props['onUpdate:modelValue'](value);
  input.props.onInput();
  await settle();
};
const search = async value => {
  const input = find(n => n.type === 'input' && n.props['aria-label'] === '搜索模型或平台');
  input.props['onUpdate:modelValue'](value);
  await settle();
};
const checkboxes = () => all().filter(n => n.type === 'input' && n.props.type === 'checkbox');
async function main() {
  await click('导入配置');
  assert.equal(calls.length, 0, 'opening import must not read/write DB');
  await inputJson(JSON.stringify(config));
  await click('解析并预览');
  assert.equal(calls.at(-1).name, 'preview_model_config');
  let boxes = all().filter(n => n.type === 'input' && n.props.type === 'checkbox');
  assert.equal(boxes.length, 3);
  assert(boxes.every(n => n.props.checked || n.checked), 'import defaults to all models, including disabled ones');
  const previewCallCount = calls.length;
  await search('  MODEL B  ');
  assert.equal(checkboxes().length, 2, 'model alias search filters within the platform');
  assert(text(body).includes('另有 1 个已选模型未显示'));
  await click('取消匹配项');
  await search('');
  assert.equal(checkboxes().length, 3);
  assert(!checkboxes()[2].checked, 'canceling a match persists after clearing search');
  assert(checkboxes()[1].checked, 'the hidden model remains selected');
  assert.equal(calls.length, previewCallCount, 'search and filtered selection make no IPC requests');
  boxes = checkboxes();
  boxes[2].props['onUpdate:modelValue'](false);
  await settle();
  failImport = true;
  await click('导入所选配置');
  assert(text(body).includes('test transaction failure'));
  assert.equal(emitted.length, 0);
  failImport = false;
  await click('导入所选配置');
  const imported = calls.findLast(c => c.name === 'import_model_config').args.config;
  assert.equal(imported.platforms[0].models.length, 1);
  assert.equal(imported.platforms[0].models[0].name, 'model-a');
  assert.equal(imported.platforms[0].api_key, 'secret-test-key');
  assert.equal(emitted.length, 1);
  assert(button('导入所选配置').props.disabled, 'success clears preview to prevent accidental replay');
  await inputJson(JSON.stringify(config)); await click('解析并预览');
  await inputJson('{bad json');
  assert(button('导入所选配置').props.disabled, 'editing source invalidates the old preview');
  await click('解析并预览');
  assert(button('导入所选配置').props.disabled, 'invalid JSON cannot use previous preview');
  const file = find(n => n.type === 'input' && n.props.type === 'file');
  await file.props.onChange({target: {files: [{text: async () => JSON.stringify(config)}], value: 'file'}});
  await settle();
  assert(!button('导入所选配置').props.disabled, 'file selection uses the same preview flow');
  await click('关闭');
  await click('导出配置');
  assert(!text(body).includes('暂无可导出的平台'), 'loaded platforms must not also display the empty-state message');
  await search('model-b');
  await click('复制 JSON');
  assert.equal(JSON.parse(copied.at(-1)).platforms[0].models.length, 2, 'filtering alone must not remove hidden selected models from export');
  await click('取消匹配项');
  await click('复制 JSON');
  assert.equal(JSON.parse(copied.at(-1)).platforms[0].models[0].name, 'model-a');
  assert.equal(JSON.parse(copied.at(-1)).platforms[0].models.length, 1);
  await click('选择匹配项');
  await search('secret-test-key');
  assert(text(body).includes('未找到匹配的平台或模型'));
  assert(button('选择匹配项').props.disabled);
  assert(!button('保存 JSON 文件').props.disabled, 'no search results must not disable exporting previously selected content');
  await click('清空搜索');
  await click('取消全选');
  await search('model-b');
  await click('选择匹配项');
  await click('复制 JSON');
  assert.equal(JSON.parse(copied.at(-1)).platforms[0].models.length, 1);
  assert.equal(JSON.parse(copied.at(-1)).platforms[0].models[0].name, 'model-b', 'selecting matches from none adds only that model');
  await search('Platform A');
  assert.equal(checkboxes().length, 4, 'platform search includes all its models and the key checkbox');
  const groupCheckbox = checkboxes()[1];
  await groupCheckbox.props.onChange({target: {checked: true}});
  await settle();
  await search('');
  await click('复制 JSON');
  assert(!copied.at(-1).includes('secret-test-key'));
  assert(!copied.at(-1).includes('api_key'));
  boxes = all().filter(n => n.type === 'input' && n.props.type === 'checkbox');
  boxes[0].props['onUpdate:modelValue'](true); // explicit include-keys checkbox
  await settle();
  await click('复制 JSON');
  assert(copied.at(-1).includes('secret-test-key'));
  savePath = null;
  await click('保存 JSON 文件');
  assert(!calls.some(c => c.name === 'save_model_config_file'), 'cancel save creates no file');
  savePath = '/tmp/model-config-test.json';
  await click('保存 JSON 文件');
  assert.equal(calls.at(-1).name, 'save_model_config_file');
  assert.equal(calls.at(-1).args.targetPath, savePath);
  assert.equal(calls.at(-1).args.config.platforms[0].api_key, 'secret-test-key');
  await click('取消全选');
  assert(button('复制 JSON').props.disabled && button('保存 JSON 文件').props.disabled);
  // 取消全选后，单个模型复选框必须保持可点，且单独勾选某个模型应自动激活该平台导出
  boxes = checkboxes();
  assert(!boxes[2].props.disabled, 'model checkbox must not be disabled when platform is unselected');
  boxes[2].props['onUpdate:modelValue'](true);
  await settle();
  assert(!button('复制 JSON').props.disabled, 'selecting an individual model enables export');
  await click('复制 JSON');
  const singleExport = JSON.parse(copied.at(-1));
  assert.equal(singleExport.platforms.length, 1);
  assert.equal(singleExport.platforms[0].models.length, 1);
  assert.equal(singleExport.platforms[0].models[0].name, 'model-a');
  boxes = checkboxes();
  boxes[2].props['onUpdate:modelValue'](false);
  await settle();
  assert(button('复制 JSON').props.disabled, 'unchecking the single model disables export again');
  await click('全选');
  assert(!button('复制 JSON').props.disabled);
  await click('关闭'); await click('导出配置'); await click('复制 JSON');
  assert(!copied.at(-1).includes('api_key'), 'new export defaults to no credentials again');
  app.unmount();
  console.log('PASS actual config dialog: pasted/file imports, preview invalidation, model/platform search, scoped batch and group choices, hidden selection export, no-match state, error retry, IPC payloads, explicit keys, save/cancel, reopen');
}
main().catch(e => {console.error(e); process.exitCode = 1;});
