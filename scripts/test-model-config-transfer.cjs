const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const source = fs.readFileSync('src/utils/modelConfigTransfer.ts', 'utf8');
const compiled = ts.transpileModule(source, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText;
const context = {exports: {}};
vm.runInNewContext(compiled, context);
const {selectConfig, selectedConfig, toggleConfigPlatform, MODEL_CONFIG_EXAMPLE} = context.exports;
const config = {version: 1, platforms: [
  {name: 'A', base_url: 'https://a.test/v1', api_key: 'private-a', models: [{name: 'one', enabled: true}, {name: 'two', enabled: false}]},
  {name: 'B', base_url: 'https://b.test/v1', api_key: 'private-b', models: [{name: 'three'}]},
  {name: 'Empty', base_url: 'http://localhost:11434/v1', models: []},
]};
const before = JSON.stringify(config);
const rows = selectConfig(config);
assert.equal(rows.length, 3);
assert(rows.every(r => r.selected && r.models.every(m => m.selected)), 'imports and exports default to all selected, including disabled models');
assert(!JSON.stringify(selectedConfig(rows, false)).includes('private-'), 'public exports omit keys');
assert(!JSON.stringify(selectedConfig(rows, false)).includes('api_key'));
rows[0].models[1].selected = false;
toggleConfigPlatform(rows[1], false);
let selected = selectedConfig(rows, true);
assert.equal(selected.platforms.length, 2);
assert.equal(selected.platforms[0].api_key, 'private-a');
assert.equal(selected.platforms[0].models.length, 1);
assert.equal(selected.platforms[0].models[0].name, 'one');
assert.equal(selected.platforms[1].models.length, 0, 'platform-only export is supported');
assert(!JSON.stringify(selected).includes('selected'));
toggleConfigPlatform(rows[1], true);
assert(rows[1].models.every(m => m.selected));
rows.forEach(r => toggleConfigPlatform(r, false));
assert.equal(selectedConfig(rows, true).platforms.length, 0);
assert.equal(JSON.stringify(config), before, 'selection never mutates source configuration');
assert.equal(JSON.parse(MODEL_CONFIG_EXAMPLE).version, 1);
console.log('PASS model config transfer: default/selective/platform-only selection, key omission, reselect, no source mutation, portable JSON');

const {filterConfigPlatforms, toggleConfigMatch} = context.exports;
const searchRows = selectConfig(config);
let matches = filterConfigPlatforms(searchRows, '  b.TEST  ');
assert.equal(matches.length, 1);
assert.equal(matches[0].row, searchRows[1], 'filter results retain original selection identity');
assert.equal(matches[0].models.length, 1);
assert.equal(matches[0].sourceIndex, 1, 'stable source identity is independent of the filtered list index');
matches = filterConfigPlatforms(searchRows, 'two');
assert.equal(matches.length, 1);
assert.equal(matches[0].models[0], searchRows[0].models[1]);
toggleConfigMatch(matches[0], false);
assert.equal(searchRows[0].models[0].selected, true, 'hidden selections are untouched by cancel-matches');
assert.equal(searchRows[0].models[1].selected, false);
assert.equal(searchRows[0].selected, true);
toggleConfigPlatform(searchRows[0], false);
toggleConfigMatch(matches[0], true);
assert.equal(searchRows[0].selected, true);
assert.equal(searchRows[0].models[0].selected, false, 'select-matches must not select a hidden model');
assert.equal(searchRows[0].models[1].selected, true);
assert.equal(selectedConfig(searchRows, true).platforms[0].models[0].name, 'two');
assert.equal(filterConfigPlatforms(searchRows, 'private-a').length, 0, 'credentials are never part of search');
assert.equal(filterConfigPlatforms(searchRows, 'missing').length, 0);
assert.equal(filterConfigPlatforms(searchRows, 'empty')[0].models.length, 0, 'platform-only entries remain searchable');
assert.equal(filterConfigPlatforms(searchRows, '  ').length, 3);
assert.equal(filterConfigPlatforms(searchRows, '')[0].models[1].selected, true, 'clearing search preserves selections');
const aliasRows = selectConfig({version: 1, platforms: [{name: '供应商甲', base_url: 'https://alias.test', models: [{name: 'actual-api-id', display_name: '中文别名'}]}]});
assert.equal(filterConfigPlatforms(aliasRows, '中文别名')[0].models[0].model.name, 'actual-api-id');
console.log('PASS config search: platform/endpoint/model/alias matching, case and whitespace, reference preservation, scoped batch choices, no credential matching, platform-only and empty results');

// Execute the real model-add handlers: a capacity preset must never invent an API model name.
const managerSource = fs.readFileSync('src/views/ApiManager.vue', 'utf8');
const managerScript = managerSource.split('<script setup lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('manager.ts', managerScript, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const handlers = ast.statements.filter(n => ts.isFunctionDeclaration(n) && ['addModel', 'quickApplySpecToPlatform'].includes(n.name?.text));
assert.equal(handlers.length, 2);
assert(!managerSource.includes('onQuickSpecSelect'), 'selecting a spec must not fill the model-name input');
const preset = {id: 'capacity-128k', name: '128K 通用规格', context_window: 128000, max_tokens: 8192, default_temperature: 0.5};
const modelName = {value: ''}, selectedSpec = {value: ''}, tab = {value: 'specs'}, ipc = [];
let focused = 0;
const handlerCode = ts.transpileModule(handlers.map(n => n.getText(ast)).join('\n') + '\n({addModel, quickApplySpecToPlatform});', {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText;
const actual = vm.runInNewContext(handlerCode, {
  selectedPlatformId: {value: 'platform-a'}, activeQuickSpec: {value: preset}, newModelName: modelName,
  quickSelectedSpecId: selectedSpec, activeTab: tab, nextTick: async () => {},
  newModelInput: {value: {focus: () => {focused++;}}}, invoke: async (name, args) => {ipc.push({name, args});},
  selectPlatform: async () => {}, refreshModelCounts: async () => {}, emit: () => {},
});
async function testModelNaming() {
  await actual.quickApplySpecToPlatform(preset);
  assert.equal(modelName.value, '');
  assert.equal(selectedSpec.value, preset.id);
  assert.equal(tab.value, 'platforms');
  assert.equal(ipc.length, 0, 'using a spec must not insert a guessed model');
  assert.equal(focused, 1);
  await actual.addModel();
  assert.equal(ipc.length, 0, 'empty model name must not be submitted');
  modelName.value = '  my-real-model  ';
  await actual.quickApplySpecToPlatform(preset);
  assert.equal(modelName.value, '  my-real-model  ', 'preset selection preserves user input');
  await actual.addModel();
  assert.equal(ipc.length, 1);
  assert.equal(ipc[0].name, 'add_model_with_spec');
  assert.equal(ipc[0].args.name, 'my-real-model');
  assert.equal(ipc[0].args.displayName, 'my-real-model', 'display name must not be replaced by the capacity preset name');
  assert.equal(ipc[0].args.contextWindow, 128000);
  assert.equal(ipc[0].args.maxTokens, 8192);
  assert.equal(modelName.value, '');
  console.log('PASS model naming: specs only supply parameters, no guessed name or implicit insert; explicit model name and display name are preserved');
}
testModelNaming().catch(error => {console.error(error); process.exitCode = 1;});
