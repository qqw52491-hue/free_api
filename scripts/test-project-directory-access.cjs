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
const code = ts.transpileModule(`${script}\nexports.testing = { directories, loading, busy, error, notice, loadDirectories, addDirectory, changeAccess, removeDirectory, workspacePickerOpen, workspaces, workspacesLoading, workspacesError, primaryPath, availableWorkspaces, loadWorkspaces, toggleWorkspacePicker, addWorkspace, isDirectoryAdded };`, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const flush = async () => { await vue.nextTick(); await new Promise(resolve => setImmediate(resolve)); };
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function setup(handler = async () => [], dialog = async () => null, initialProps = {}) {
  const props = vue.reactive({ projectId: 'p1', rootPath: '/primary', ...initialProps });
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
  const project = { id: 'p2', name: '参考项目', root_path: '/reference' };
  saved = [];
  ui = setup(async (name, args) => {
    if (name === 'list_agent_projects') return [
      { id: 'p1', name: '当前项目', root_path: '/primary' },
      { id: 'same-root', name: '同目录项目', root_path: '/primary' }, project,
    ];
    if (name === 'set_agent_project_directory') saved = [{ path: args.path, access: args.access }];
    return saved;
  }); await flush();
  assert.equal(ui.calls.length, 1, 'workspace list must be loaded only on demand');
  await ui.toggleWorkspacePicker();
  assert.equal(ui.availableWorkspaces.value.length, 1, 'exclude current project and primary directory');
  assert.equal(ui.availableWorkspaces.value[0].name, '参考项目');
  await ui.addWorkspace(ui.availableWorkspaces.value[0]);
  assert.equal(ui.calls[2].name, 'set_agent_project_directory');
  assert.equal(ui.calls[2].args.projectId, 'p1'); assert.equal(ui.calls[2].args.path, '/reference');
  assert.equal(ui.calls[2].args.access, 'read_only'); assert.equal(ui.workspacePickerOpen.value, false); ui.stop();

  ui = setup(async name => name === 'list_agent_projects' ? [project] : [{ path: '/reference', access: 'read_write' }]);
  await flush(); await ui.toggleWorkspacePicker(); assert.equal(ui.isDirectoryAdded('/reference'), true);
  await ui.addWorkspace(project);
  assert.equal(ui.calls.length, 2, 'already added workspace must not be submitted again or downgraded');
  assert.equal(ui.directories.value[0].access, 'read_write'); ui.stop();

  ui = setup(); await flush(); await ui.toggleWorkspacePicker();
  assert.equal(ui.availableWorkspaces.value.length, 0); assert.equal(ui.workspacesLoading.value, false);
  assert.equal(ui.workspacesError.value, ''); ui.stop();

  let listAttempts = 0;
  ui = setup(async name => {
    if (name !== 'list_agent_projects') return [];
    if (++listAttempts === 1) throw Error('workspace offline');
    return [project];
  }); await flush(); await ui.toggleWorkspacePicker();
  assert.match(ui.workspacesError.value, /workspace offline/); assert.equal(ui.workspacesLoading.value, false);
  await ui.loadWorkspaces(); assert.equal(ui.workspacesError.value, '');
  assert.equal(ui.availableWorkspaces.value[0].id, 'p2'); ui.stop();

  const staleWorkspaces = deferred();
  ui = setup(async name => name === 'list_agent_projects' ? staleWorkspaces.promise : []);
  await flush(); const listing = ui.toggleWorkspacePicker();
  ui.props.projectId = 'p3'; ui.props.rootPath = '/p3'; await flush();
  staleWorkspaces.resolve([project]); await listing;
  assert.equal(ui.workspacePickerOpen.value, false); assert.equal(ui.workspaces.value.length, 0);
  assert.equal(ui.workspacesLoading.value, false, 'old project list must not leak into the new picker'); ui.stop();

  const staleWorkspaceSave = deferred();
  ui = setup(async name => name === 'list_agent_projects' ? [project] : name === 'set_agent_project_directory' ? staleWorkspaceSave.promise : []);
  await flush(); await ui.toggleWorkspacePicker(); const workspaceSaving = ui.addWorkspace(project);
  ui.props.projectId = 'p3'; ui.props.rootPath = '/p3'; await flush();
  await ui.toggleWorkspacePicker(); staleWorkspaceSave.resolve([{ path: '/reference', access: 'read_only' }]); await workspaceSaving;
  assert.equal(ui.workspacePickerOpen.value, true, 'old save must not close new project picker');
  assert.equal(ui.directories.value.length, 0); assert.equal(ui.notice.value, ''); ui.stop();

  ui = setup(async name => {
    if (name === 'list_agent_projects') return [project];
    if (name === 'set_agent_project_directory') throw Error('directory missing');
    return [];
  }); await flush(); await ui.toggleWorkspacePicker(); await ui.addWorkspace(project);
  assert.match(ui.error.value, /directory missing/); assert.equal(ui.workspacePickerOpen.value, true);
  assert.equal(ui.busy.value, false); assert.equal(ui.directories.value.length, 0); ui.stop();

  ui = setup(async name => name === 'list_agent_projects' ? [project] : []);
  await flush(); await ui.toggleWorkspacePicker(); await ui.toggleWorkspacePicker();
  assert.equal(ui.workspacePickerOpen.value, false);
  assert.equal(ui.calls.length, 2, 'closing workspace picker must not create a grant'); ui.stop();

  const sessionProps = { projectId: undefined, rootPath: undefined, sessionId: 's1' };
  saved = [];
  ui = setup(async (name, args) => {
    if (name === 'get_agent_session_directory_config') return { root_path: '/session-tmp', directories: saved };
    if (name === 'list_agent_projects') return [project];
    if (name === 'set_agent_session_directory') saved = [{ path: args.path, access: args.access }];
    if (name === 'remove_agent_session_directory') saved = [];
    return saved;
  }, async options => { assert.equal(options.defaultPath, undefined, 'new session tmp may not exist yet'); return '/reference'; }, sessionProps);
  await flush();
  assert.equal(ui.calls[0].name, 'get_agent_session_directory_config');
  assert.equal(ui.calls[0].args.sessionId, 's1'); assert.equal(ui.primaryPath.value, '/session-tmp');
  await ui.addDirectory();
  assert.equal(ui.calls[1].name, 'set_agent_session_directory');
  assert.equal(ui.calls[1].args.sessionId, 's1'); assert.equal(ui.calls[1].args.access, 'read_only');
  assert.equal(ui.calls[1].args.projectId, undefined);
  await ui.changeAccess('/reference', { target: { value: 'read_write' } });
  assert.equal(ui.directories.value[0].access, 'read_write');
  await ui.removeDirectory('/reference'); assert.equal(ui.calls[3].name, 'remove_agent_session_directory');
  await ui.toggleWorkspacePicker(); await ui.addWorkspace(project);
  assert.equal(ui.calls[5].name, 'set_agent_session_directory'); assert.equal(ui.calls[5].args.sessionId, 's1');
  assert.equal(ui.calls[5].args.access, 'read_only'); ui.stop();

  ui = setup(async name => {
    if (name === 'get_agent_session_directory_config') return { root_path: '/session-tmp', directories: [{ path: '/reference', access: 'read_only' }] };
    throw Error('session save denied');
  }, undefined, sessionProps); await flush();
  const sessionSelect = { value: 'read_write' }; await ui.changeAccess('/reference', { target: sessionSelect });
  assert.equal(sessionSelect.value, 'read_only'); assert.match(ui.error.value, /session save denied/); ui.stop();

  const sessionList = deferred();
  ui = setup(async (name, args) => args.sessionId === 's1' ? sessionList.promise : { root_path: '/s2-tmp', directories: [{ path: '/s2-ref', access: 'read_only' }] }, undefined, sessionProps);
  ui.props.sessionId = 's2'; await flush();
  sessionList.resolve({ root_path: '/s1-tmp', directories: [{ path: '/s1-ref', access: 'read_write' }] }); await flush();
  assert.equal(ui.primaryPath.value, '/s2-tmp'); assert.equal(ui.directories.value[0].path, '/s2-ref'); ui.stop();

  const sessionSave = deferred();
  ui = setup(async (name, args) => name === 'set_agent_session_directory' ? sessionSave.promise : { root_path: `/${args.sessionId}-tmp`, directories: [] }, undefined, sessionProps);
  await flush(); const savingSession = ui.changeAccess('/reference', { target: { value: 'read_write' } });
  ui.props.sessionId = 's2'; await flush(); sessionSave.resolve([{ path: '/reference', access: 'read_write' }]); await savingSession;
  assert.equal(ui.primaryPath.value, '/s2-tmp'); assert.equal(ui.directories.value.length, 0); assert.equal(ui.notice.value, ''); ui.stop();

  console.log('PASS: project permissions + workspace picker (18 scenarios), session permissions (8 scenarios).');
})().catch(error => { console.error(error); process.exitCode = 1; });
