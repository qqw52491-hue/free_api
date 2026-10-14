// Unit regression: execute the real composable with mocked Tauri I/O, no model requests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const root = path.resolve(__dirname, '..');
const cache = new Map();
let mounted = [], requests = [], traceStatus = 'cancelled', creations = [], failCreation = false;
const storage = new Map();
const handlers = new Map();
global.localStorage = { getItem: k => storage.get(k) ?? null, setItem: (k,v) => storage.set(k,v) };
global.window = { setInterval, clearInterval, setTimeout, clearTimeout };
const noOp = () => {};
const format = {
    asRecord: v => v && typeof v === 'object' && !Array.isArray(v) ? v : {},
    numberOrUndefined: v => Number.isFinite(Number(v)) && v != null ? Number(v) : undefined,
    toText: v => v == null ? '' : typeof v === 'string' ? v : JSON.stringify(v),
    toTimestamp: v => typeof v === 'number' ? v : Date.parse(v) || 0,
};
async function invoke(name, args) {
    if (name === 'create_session' || name === 'create_agent_project_session') {
        creations.push({ name, args });
        if (failCreation) throw Error('cannot create session');
        return { id: 'saved-session' };
    }
    if (name === 'get_session_run_traces') return [{
        id: 'previous-run', goal: 'original complex task', status: traceStatus,
        started_at: '2026-10-13T08:00:00Z', updated_at: '2026-10-13T08:01:00Z', steps: [],
    }];
    if (name === 'run_agent_main_loop') { requests.push(args); return; }
    if (name === 'get_agent_checkpoint') return null;
    return [];
}
function load(filename) {
    filename = path.resolve(filename);
    if (cache.has(filename)) return cache.get(filename).exports;
    const m = { exports: {} }; cache.set(filename, m);
    const code = ts.transpileModule(fs.readFileSync(filename, 'utf8'), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    function localRequire(id) {
        if (id === 'vue') return { ...vue, onMounted: fn => mounted.push(fn), onUnmounted: noOp };
        if (id === '@tauri-apps/api/core') return { invoke };
        if (id === '@tauri-apps/api/event') return { listen: async (channel, fn) => { handlers.set(channel, fn); return () => { if (handlers.get(channel) === fn) handlers.delete(channel); }; } };
        if (id.endsWith('/format')) return format;
        if (id.endsWith('/usePersistedAgentConversation')) return {
            usePersistedAgentConversation: () => ({ conversationMessages: vue.ref([]), isConversationLoading: vue.ref(false), loadConversation: async () => {} }),
        };
        if (id.endsWith('/useAgentTaskResources')) return {
            useAgentTaskResources: () => new Proxy({}, { get: (_o, k) =>
                ['resources'].includes(k) ? vue.ref([]) : k.startsWith('is') ? vue.ref(false)
                : /Error|selectedResource|recordPage/.test(k) ? vue.ref(null) : noOp }),
        };
        if (id.startsWith('.')) {
            const resolved = path.resolve(path.dirname(filename), id);
            return load(fs.existsSync(resolved + '.ts') ? resolved + '.ts' : resolved);
        }
        return require(id);
    }
    const wrapper = vm.runInThisContext('(function(require,module,exports){' + code + '\n})', { filename });
    wrapper(localRequire, m, m.exports);
    return m.exports;
}
const { useAgentSession } = load(path.join(root, 'src/composables/useAgentSession.ts'));
async function session(status) {
    traceStatus = status; mounted = []; requests = []; storage.clear();
    const s = useAgentSession('existing-session');
    s.modelRouting.value.pro = 'pro-model';
    for (const fn of mounted) await fn();
    return s;
}
async function main() {
    for (const status of ['failed', 'cancelled', 'running', 'waiting_human']) {
        const s = await session(status);
        assert.equal(s.canResumeTask.value, true, status);
        s.modelRouting.value.stage_reviewer_override = 'flash';
        await s.runAgent('换模型继续');
        assert.equal(requests[0].taskStartMode, 'resume');
        assert.equal(requests[0].planMode, undefined, 'changing model must not reset plan mode');
        assert.equal(requests[0].modelRouting.stage_reviewer_override, 'flash');
    }
    let s = await session('cancelled');
    await s.runAgent('a genuinely new task', undefined, 'new');
    assert.equal(requests[0].taskStartMode, 'new');
    assert.equal(requests[0].planMode, 'auto');
    s = await session('failed');
    s.setPlanMode('plan_approval');
    await s.runAgent('继续');
    assert.equal(requests[0].taskStartMode, 'resume');
    assert.equal(requests[0].planMode, 'plan_approval', 'explicit mode selection must be honored');
    s = await session('succeeded');
    assert.equal(s.canResumeTask.value, false);
    await s.runAgent('继续');
    assert.equal(requests[0].taskStartMode, undefined, 'wording alone must not resurrect completed task');
    s = await session('failed');
    s.queuedGoals.value.push({ id: 'queue', text: 'next independent task', createdAt: Date.now() });
    await s.runAgent('继续');
    await new Promise(resolve => setTimeout(resolve, 25));
    assert.equal(requests[0].taskStartMode, 'resume');
    assert.equal(requests[1].taskStartMode, 'new', 'queued task must not inherit failed predecessor');
    requests = []; creations = []; failCreation = false;
    let promoted = [];
    s = useAgentSession('draft-directory', { onPromote: (...args) => promoted.push(args) });
    s.modelRouting.value.pro = 'pro-model';
    const ids = await Promise.all([s.ensureSession(), s.ensureSession()]);
    assert.equal(ids[0], 'saved-session'); assert.equal(ids[1], 'saved-session');
    assert.equal(creations.length, 1, 'settings and send share one creation');
    assert.equal(promoted.length, 1); assert.equal(requests.length, 0, 'opening settings must not run a model');
    assert.equal(s.activeSessionId.value, 'saved-session');
    assert.equal(await s.ensureSession(), 'saved-session'); assert.equal(creations.length, 1);
    await s.runAgent('use configured directory');
    assert.equal(requests[0].sessionId, 'saved-session'); assert.equal(creations.length, 1);

    requests = []; creations = []; failCreation = true;
    s = useAgentSession('draft-failure');
    s.modelRouting.value.pro = '';
    await assert.rejects(() => s.ensureSession(), /cannot create session/);
    assert.equal(s.activeSessionId.value, 'draft-failure');
    await s.runAgent('must not start without saved session'); assert.equal(requests.length, 0);
    failCreation = false; await s.ensureSession(); assert.equal(s.activeSessionId.value, 'saved-session');
    assert.equal(creations[2].args.modelId, '', 'configuring directories does not require selecting a model');

    creations = [];
    s = useAgentSession('draft-project', { projectId: 'p1' });
    await s.ensureSession(); assert.equal(creations[0].name, 'create_agent_project_session');
    assert.equal(creations[0].args.projectId, 'p1');
    console.log('PASS: task intent (8 scenarios) + settings session creation (6 scenarios); real composable, mocked I/O');
}
module.exports = { load, session, emit: (channel, payload) => { assert(handlers.has(channel), channel); return handlers.get(channel)({ payload }); } };
if (require.main === module) main().then(() => process.exit(0), e => { console.error(e); process.exit(1); });
