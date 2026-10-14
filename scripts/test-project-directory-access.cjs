// Execute the component's real setup logic with Vue reactivity and mocked Tauri I/O.
// No native dialog, disk mutation or model request is performed by this test.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../src/components/ProjectDirectoryAccess.vue'), 'utf8');
const script = source.match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
const code = ts.transpileModule(`${script}\nexports.testing = { directories, loading, busy, error, notice, loadDirectories, addDirectory, changeAccess, removeDirectory };`, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const flush = async () => { await vue.nextTick(); await new Promise(resolve => setImmediate(resolve)); };
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function setup(handler = async () => [], dialog = async () => null) {
  const props = vue.reactive({ projectId: 'p1', rootPath: '/primary' });
  const calls = []; const exports = {}; const scope = vue.effectScope();
  scope.run(() => vm.runInNewContext(code, {
    exports, defineProps: () => props,
    require: id => id === 'vue' ? vue : id === '@tauri-apps/api/core' ? {
      invoke: async (name, args) => { calls.push({ name, args }); return handler(name, args); },
    } : { open: dialog },
  }));
  return { ...exports.testing, props, calls, stop: () => scope.stop() };
}
(async () => {
  let ui = setup(); await flush();
  assert.equal(ui.calls[0].name, 'list_agent_project_directories');
  assert.equal(ui.directories.value.length, 0); assert.equal(ui.loading.value, false); ui.stop();

  let saved = [];
  ui = setup(async (name, args) => {
    if (name === 'set_agent_project_directory') saved = [{ path: args.path, access: args.access }];
    if (name === 'remove_agent_project_directory') saved = [];
    return saved;
  }, async () => '/reference');
  await flush(); await ui.addDirectory();
  assert.equal(ui.calls[1].args.access, 'read_only'); assert.equal(ui.calls[1].args.projectId, 'p1');
  assert.equal(ui.directories.value[0].access, 'read_only');
  const select = { value: 'read_write' }; await ui.changeAccess('/reference', { target: select });
  assert.equal(ui.calls[2].args.access, 'read_write'); assert.equal(ui.directories.value[0].access, 'read_write');
  await ui.removeDirectory('/reference'); assert.equal(ui.directories.value.length, 0);
  assert.equal(ui.calls[3].name, 'remove_agent_project_directory');
  assert.match(ui.notice.value, /不重置原任务和审查/); ui.stop();

  ui = setup(async () => []); await flush(); await ui.addDirectory();
  assert.equal(ui.calls.length, 1, 'cancelled native selection must not create a grant'); ui.stop();

  ui = setup(async name => {
    if (name === 'list_agent_project_directories') return [{ path: '/reference', access: 'read_only' }];
    throw Error('save failed');
  }); await flush();
  const failedSelect = { value: 'read_write' }; await ui.changeAccess('/reference', { target: failedSelect });
  assert.equal(failedSelect.value, 'read_only'); assert.equal(ui.directories.value[0].access, 'read_only');
  assert.match(ui.error.value, /save failed/); assert.equal(ui.busy.value, false); ui.stop();

  const oldList = deferred();
  ui = setup(async (name, args) => args.projectId === 'p1' ? oldList.promise : [{ path: '/p2-ref', access: 'read_only' }]);
  ui.props.projectId = 'p2'; await flush(); oldList.resolve([{ path: '/p1-ref', access: 'read_write' }]); await flush();
  assert.equal(ui.directories.value[0].path, '/p2-ref', 'stale p1 read must not replace p2 settings'); ui.stop();

  const oldSave = deferred();
  ui = setup(async (name, args) => name === 'set_agent_project_directory' ? oldSave.promise : args.projectId === 'p2' ? [{ path: '/p2-ref', access: 'read_only' }] : [{ path: '/p1-ref', access: 'read_only' }]);
  await flush(); const mutation = ui.changeAccess('/p1-ref', { target: { value: 'read_write' } });
  ui.props.projectId = 'p2'; await flush(); oldSave.resolve([{ path: '/p1-ref', access: 'read_write' }]); await mutation;
  assert.equal(ui.directories.value[0].path, '/p2-ref', 'stale p1 write response must not replace p2 settings');
  assert.equal(ui.notice.value, ''); assert.equal(ui.busy.value, false); ui.stop();

  ui = setup(async () => { throw Error('offline'); }); await flush();
  assert.match(ui.error.value, /offline/); assert.equal(ui.loading.value, false); ui.stop();
  console.log('PASS: initial load, default read-only add, write includes read, removal, dialog cancel, failure rollback, stale load, stale save, load error (9 scenarios).');
})().catch(error => { console.error(error); process.exitCode = 1; });
