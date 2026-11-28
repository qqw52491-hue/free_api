// Unit regression: execute the real composable with mocked Tauri I/O, no model requests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const root = path.resolve(__dirname, '..');
const cache = new Map();
let mounted = [], unmounted = [], requests = [], traceStatus = 'cancelled', creations = [], failCreation = false;
let traceFixtures, messageFixtures = [];
let invokeFixtures = {};
let deferredListen;
const storage = new Map();
const handlers = new Map();
global.localStorage = { getItem: k => storage.get(k) ?? null, setItem: (k,v) => storage.set(k,v) };
global.window = { setInterval, clearInterval, setTimeout, clearTimeout, addEventListener: () => {}, removeEventListener: () => {} };
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
    if (name === 'run_agent_main_loop') requests.push(args);
    if (invokeFixtures[name]) return invokeFixtures[name](args);
    if (name === 'get_session_run_traces') return traceFixtures ?? [{
        id: 'previous-run', goal: 'original complex task', status: traceStatus,
        started_at: '2026-10-13T08:00:00Z', updated_at: '2026-10-13T08:01:00Z', steps: [],
    }];
    if (name === 'run_agent_main_loop') return;
    if (name === 'get_agent_checkpoint') return null;
    if (name === 'get_agent_execution_snapshot') return { active: null, waiting_checkpoint: null };
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
        if (id === 'vue') return { ...vue, onMounted: fn => mounted.push(fn), onUnmounted: fn => unmounted.push(fn) };
        if (id === '@tauri-apps/api/core') return { invoke };
        if (id === '@tauri-apps/api/event') return { listen: async (channel, fn) => {
            if (deferredListen) await deferredListen;
            handlers.set(channel, fn);
            return () => { if (handlers.get(channel) === fn) handlers.delete(channel); };
        } };
        if (id.endsWith('/format')) return format;
        if (id.endsWith('/usePersistedAgentConversation')) return {
            usePersistedAgentConversation: () => ({ conversationMessages: vue.ref(messageFixtures), isConversationLoading: vue.ref(false), loadConversation: async () => {}, invalidateConversationLoad: noOp }),
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
const { useModelRoutingPreferences } = load(path.join(root, 'src/composables/useModelRoutingPreferences.ts'));
const routingDefaults = structuredClone(vue.toRaw(useModelRoutingPreferences().value));
async function session(status, fixtures = {}) {
    traceStatus = status; mounted = []; unmounted = []; requests = []; storage.clear();
    useModelRoutingPreferences().value = structuredClone(routingDefaults);
    traceFixtures = fixtures.traces;
    messageFixtures = fixtures.messages ?? [];
    invokeFixtures = fixtures.invoke ?? {};
    const s = useAgentSession('existing-session');
    s.modelRouting.value.pro = 'pro-model';
    for (const fn of mounted) await fn();
    return s;
}

async function replaceWaitingTask() {
    const event = (channel, payload) => handlers.get(channel)({payload});
    const pausedRun = async (runId = 'old-run') => {
        await event('channel-run-started', {session_id:'existing-session',run_id:runId,goal:'改造 License'});
        await event('agent-progress-existing-session', {type:'agent_paused',run_id:runId,wait_id:'old-wait',message:'是否继续改造？'});
    };
    const ask = '停止改造，解释为什么反复读取';
    const activeStop = {delivered:true,has_active_run:true,active_run_id:'old-run'};

    let releaseOld;
    let s = await session('cancelled', {invoke:{
        run_agent_main_loop: () => requests.length === 1 ? new Promise(resolve => {releaseOld = resolve;}) : undefined,
        cancel_agent_execution: ({runId}) => ({...activeStop,active_run_id:runId}),
        send_human_intervention: () => assert.fail('新任务不能写进人工回答'),
    }});
    const old = s.runAgent('改造 License', undefined, 'new');
    await new Promise(resolve => setImmediate(resolve));
    const oldDesktopId = requests[0].desktopRunId;
    await pausedRun(oldDesktopId);
    assert.equal(s.isWaitingForHuman.value, true);
    assert.equal(await s.endTaskAndStartNew(ask), true);
    await event('channel-run-done', {session_id:'existing-session',run_id:oldDesktopId,status:'cancelled'});
    assert.equal(requests.length, 1, '本地旧任务还未完成收尾');
    releaseOld();
    await old;
    assert.equal(requests.length, 2);
    assert.equal(requests[1].goal, ask);
    assert.equal(requests[1].taskStartMode, 'new');

    s = await session('running', {invoke:{cancel_agent_execution: () => activeStop}});
    await pausedRun();
    assert.equal(await s.endTaskAndStartNew(ask), true);
    await event('channel-run-done', {session_id:'existing-session',run_id:'unrelated-run',status:'cancelled'});
    assert.equal(requests.length, 0, '其他运行的完成事件不能启动新问题');
    await event('channel-run-done', {session_id:'existing-session',run_id:'old-run',status:'cancelled'});
    await event('channel-run-done', {session_id:'existing-session',run_id:'old-run',status:'cancelled'});
    assert.equal(requests.length, 1);
    assert.equal(requests[0].taskStartMode, 'new');

    s = await session('waiting_human', {invoke:{
        get_agent_execution_snapshot: () => ({active:null,waiting_checkpoint:{version:1,user_goal:'改造 License'}}),
        cancel_agent_waiting_checkpoint: ({checkpointVersion}) => assert.equal(checkpointVersion, 1),
    }});
    assert.equal(s.isWaitingForHuman.value, true);
    assert.equal(await s.endTaskAndStartNew(ask), true);
    assert.equal(requests[0].taskStartMode, 'new');

    s = await session('running', {invoke:{cancel_agent_execution: () => ({...activeStop,delivered:false}),get_agent_execution_snapshot: () => ({active:{run_id:'old-run',wait_id:'old-wait',question:'是否继续改造？'},waiting_checkpoint:null})}});
    await pausedRun();
    assert.equal(await s.endTaskAndStartNew(ask), false);
    assert.equal(s.isWaitingForHuman.value, true);
    assert.equal(await s.endTaskAndStartNew(ask), false, '停止失败后可重试且不会残留上一份新任务');
    await event('channel-run-done', {session_id:'existing-session',run_id:'old-run',status:'cancelled'});
    assert.equal(requests.length, 0, '失败的切换不能留待未来误启动');
}

function deferred() {
    let resolve;
    const promise = new Promise(done => { resolve = done; });
    return { promise, resolve };
}

async function testExecutionBoundaries() {
    const event = (channel, payload) => handlers.get(channel)({payload});
    const progress = payload => event('agent-progress-existing-session', payload);
    const started = runId => event('channel-run-started', {session_id:'existing-session',run_id:runId,goal:'外部任务'});
    const done = runId => event('channel-run-done', {session_id:'existing-session',run_id:runId,status:'done'});
    const live = (runId, waitId, question) => ({
        active:{run_id:runId,goal:'外部任务',wait_id:waitId,question},
        waiting_checkpoint:null,
    });

    let snapshot = live('R1', null, null);
    const reply = deferred();
    const answerRequests = [];
    const stopRequests = [];
    let s = await session('running', { invoke: {
        get_agent_execution_snapshot: () => snapshot,
        send_human_intervention: args => {
            answerRequests.push(args);
            if (args.waitId === 'W1') return reply.promise;
            throw Error('question expired');
        },
        cancel_agent_execution: args => {
            stopRequests.push(args);
            return {delivered:true,has_active_run:true,active_run_id:'R2'};
        },
    }});
    assert.equal(s.isRunning.value, true, 'first open snapshots the actual active external run');
    assert.equal(s.activeRunId.value, 'R1');
    assert.equal(s.canResumeTask.value, false);
    await s.runAgent('do not start a second run');
    assert.equal(requests.length, 0);
    await progress({type:'agent_paused',run_id:'R1',wait_id:'W1',message:'第一张审批'});
    const oldKey = s.humanWaitKey.value;
    const answer = s.answerHuman('第一张的回答', undefined, oldKey);
    snapshot = live('R1', 'W2', '第二张审批');
    await progress({type:'agent_paused',run_id:'R1',wait_id:'W2',message:'第二张审批'});
    reply.resolve({id:'answer-1'});
    assert.equal(await answer, true);
    assert.deepEqual(answerRequests[0], {sessionId:'existing-session',runId:'R1',waitId:'W1',message:'第一张的回答'});
    assert.equal(s.pauseReason.value, '第二张审批', 'old acknowledgement cannot clear the new question');
    assert.equal(s.isWaitingForHuman.value, true);
    assert.equal(await s.answerHuman('旧卡再次提交', undefined, oldKey), false);
    assert.equal(answerRequests.length, 1, 'a click from the old card must not send an answer to the new card');
    assert.equal(await s.answerHuman('后端拒绝的回答', undefined, s.humanWaitKey.value), false);
    assert.equal(answerRequests[1].waitId, 'W2');
    assert.equal(requests.length, 0, 'a normal failure never launches checkpoint recovery');
    assert.equal(s.pauseReason.value, '第二张审批');
    assert.equal(await s.requestStop(), false, 'an unrelated receipt does not report a successful stop');
    assert.deepEqual(stopRequests, [{sessionId:'existing-session',runId:'R1'}]);
    assert.equal(s.activeRunId.value,'R1', 'unrelated stop receipt does not adopt another run');
    unmounted.forEach(fn => fn());

    let saved = {version:23,user_goal:'保存的任务',last_error:'保存的审批'};
    const savedFixture = {
        get_agent_execution_snapshot: () => ({active:null,waiting_checkpoint:saved}),
        cancel_agent_waiting_checkpoint: args => {
            assert.deepEqual(args, {sessionId:'existing-session',checkpointVersion:23});
            saved = null;
        },
    };
    s = await session('waiting_human', {invoke:savedFixture});
    assert.equal(s.humanWaitKey.value,'saved:23');
    await done('stale-r');
    assert.equal(s.humanWaitKey.value,'saved:23', 'an unrelated done must not dismiss a saved wait');
    assert.equal(await s.requestStop(), true);
    assert.equal(s.isWaitingForHuman.value,false);
    s = await session('cancelled', {invoke:savedFixture});
    assert.equal(s.isWaitingForHuman.value,false, 'stopped saved wait stays dismissed after reopen');
    saved = {version:24,user_goal:'另一个保存的任务'};
    s = await session('waiting_human', {invoke:savedFixture});
    assert.equal(await s.answerHuman('明确回答保存的审批'), true);
    assert.equal(requests[0].taskStartMode, 'resume');
    assert.equal(requests[0].waitingCheckpointVersion, 24);

    const previousTrace = deferred();
    let reads = 0;
    const trace = status => [{id:'racing-run',goal:'外部任务',status,started_at:'2026-10-13T08:00:00Z',steps:[]}];
    s = await session('succeeded', {invoke:{
        get_session_run_traces: () => ++reads === 2 ? previousTrace.promise : trace('succeeded'),
    }});
    const oldRead = started('racing-run');
    await done('racing-run');
    assert.equal(s.historyTurns.value[0].status, 'succeeded');
    previousTrace.resolve(trace('running'));
    await oldRead;
    assert.equal(s.historyTurns.value[0].status, 'succeeded', 'late old read cannot roll terminal history backwards');
    assert.equal(s.canResumeTask.value, false);
    unmounted.forEach(fn => fn());

    const localExit = deferred();
    const inFlightLocalStops = [];
    s = await session('succeeded', {invoke:{
        run_agent_main_loop: () => localExit.promise,
        cancel_agent_execution: ({runId}) => {
            inFlightLocalStops.push(runId);
            return {delivered:true,has_active_run:true,active_run_id:runId};
        },
    }});
    const localAttempt = s.runAgent('本地任务');
    await new Promise(resolve => setImmediate(resolve));
    const localRunId = requests[0].desktopRunId;
    assert.match(localRunId, /^[0-9a-f-]{36}$/);
    assert.equal(await s.requestStop(), true, 'stop is queued while its own registration is pending');
    assert.deepEqual(inFlightLocalStops, []);
    await started('external-started-first');
    assert.equal(s.activeRunId.value, 'external-started-first');
    assert.deepEqual(inFlightLocalStops, [], 'an external admission does not consume the earlier local stop');
    await done('external-started-first');
    await started(localRunId);
    assert.deepEqual(inFlightLocalStops, [localRunId], 'queued stop targets only the locally submitted run');
    await done(localRunId);
    localExit.resolve();
    await localAttempt;
    unmounted.forEach(fn => fn());

    const listenGate = deferred();
    deferredListen = listenGate.promise;
    useAgentSession('existing-session');
    const mount = mounted.at(-1)();
    unmounted.at(-1)();
    listenGate.resolve();
    await mount;
    deferredListen = undefined;
    assert.equal(handlers.size, 0, 'listeners registered after unmount are released immediately');
}

async function main() {
    for (const status of ['failed', 'cancelled', 'running', 'waiting_human']) {
        const s = await session(status);
        assert.equal(s.canResumeTask.value, true, status);
        assert.equal(s.modelRouting.value.reviewer_backup_model, '', 'default backup must be off');
        s.modelRouting.value.reviewer_backup_model = 'explicit-backup';
        s.modelRouting.value.stage_reviewer_override = 'flash';
        await s.runAgent('换模型继续');
        assert.equal(requests[0].taskStartMode, 'resume');
        assert.equal(requests[0].planMode, undefined, 'changing model must not reset plan mode');
        assert.equal(requests[0].modelRouting.stage_reviewer_override, 'flash');
        assert.equal(requests[0].modelRouting.reviewer_backup_model, 'explicit-backup');
    }
    let s = await session('cancelled');
    await s.runAgent('a genuinely new task', undefined, 'new');
    assert.equal(requests[0].taskStartMode, 'new');
    assert.equal(requests[0].planMode, 'auto');
    s = await session('failed');
    s.setPlanMode('plan_approval');
    await s.runAgent('继续');
    assert.equal(requests[0].taskStartMode, 'resume');
    assert.equal(requests[0].planMode, undefined, 'resume must preserve original mode even after editing new-task settings');
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
    await replaceWaitingTask();
    await testExecutionBoundaries();
    invokeFixtures = {};
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

    s = await session('cancelled');
    const other = useAgentSession('second-open-session');
    s.updateModelRouting({ ...s.sessionModelRouting.value, disabled_role_cards: ['reviewer:chosen'] });
    const preferences = useModelRoutingPreferences();
    preferences.value.pro = 'updated-project-pro';
    assert.equal(other.sessionModelRouting.value.pro, 'updated-project-pro');
    assert.equal(other.sessionModelRouting.value.disabled_role_cards, undefined, 'role exclusions belong to the session');
    await s.runAgent('resume after updating the model');
    assert.equal(requests[0].taskStartMode, 'resume');
    assert.equal(requests[0].modelRouting.pro, 'updated-project-pro');
    assert.deepEqual(requests[0].modelRouting.disabled_role_cards, ['reviewer:chosen']);
    assert.equal(JSON.parse(storage.get('free-api.agent.model-routing.v1')).disabled_role_cards, undefined);
    console.log('PASS: task intent, replacement, scoped controls, saved waits, initial snapshot and late refresh/listener isolation; real composable with mocked I/O');
}
module.exports = { load, session, emit: (channel, payload) => { assert(handlers.has(channel), channel); return handlers.get(channel)({ payload }); } };
if (require.main === module) main().then(() => process.exit(0), e => { console.error(e); process.exit(1); });
