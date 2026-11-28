// Run: node scripts/test-mobile-agent-rendering.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent/channel/http/mobile_web/app.js'), 'utf8');

class Node {
    constructor() {
        this.children = [];
        this.dataset = {};
        this.style = {};
        this.classList = { add() {}, remove() {} };
        this.html = '';
        this.writes = 0;
    }
    get firstElementChild() { return this.children[0] ?? null; }
    get nextElementSibling() { return this.parent?.children[this.parent.children.indexOf(this) + 1] ?? null; }
    get innerHTML() { return this.html; }
    set innerHTML(value) {
        this.writes++;
        this.html = value;
        this.children.forEach(child => { child.parent = null; });
        this.children = [];
    }
    insertBefore(node, before) {
        node.remove();
        this.children.splice(before ? this.children.indexOf(before) : this.children.length, 0, node);
        node.parent = this;
    }
    append(...nodes) { nodes.forEach(node => this.insertBefore(node, null)); }
    remove() {
        if (!this.parent) return;
        this.parent.children.splice(this.parent.children.indexOf(this), 1);
        this.parent = null;
    }
}

const nodes = new Map();
const get = id => {
    if (!nodes.has(id)) nodes.set(id, new Node());
    return nodes.get(id);
};
const calls = { markdown: 0, steps: 0 };
const context = vm.createContext({
    localStorage: { getItem() { return null; } },
    document: { getElementById: get, createElement: () => new Node() },
    window: {}, URL, calls,
    location: { href: 'http://127.0.0.1/' },
});
vm.runInContext(source, context);
const run = code => vm.runInContext(code, context);
context.inlineText = '[查看 app.js](https://example.com/app.js?x=1&y=2)';
assert.match(run('formatInline(inlineText)'), /href="https:\/\/example\.com\/app\.js\?x=1&amp;y=2"/);
context.inlineText = '`app.js`';
assert.equal(run('formatInline(inlineText)'), '<code class="inline-code">app.js</code>');
context.inlineText = '<img src=x onerror=alert(1)>';
assert.equal(run('formatInline(inlineText)'), '&lt;img src=x onerror=alert(1)&gt;');
run(`
const originalMarkdown = formatMarkdown;
formatMarkdown = (...args) => { calls.markdown++; return originalMarkdown(...args); };
const originalSteps = renderStepsTree;
renderStepsTree = (...args) => { calls.steps++; return originalSteps(...args); };
S.view = { name: 'flow' };
`);

const traces = Array.from({length: 80}, (_, index) => ({
    id: `run-${index}`, goal: `阅读 **第 ${index} 轮**`, status: 'succeeded',
    completion: { summary: `完成 **第 ${index} 轮**` },
    steps: Array.from({length: 8}, (_, step) => ({
        id: step + 1, tool: 'search', action: `扫描 ${step}`, status: 'succeeded',
        params: { path: 'src/example.ts' }, output: '<script>example</script>',
        planId: step < 4 ? 'first' : 'last', thought: '这是思考内容',
    })),
}));
context.f = { traces };
const paint = () => run('renderTimeline(f, true, "", "")');
paint();
const timeline = get('timeline');
const [first, second] = timeline.children;
const last = timeline.children.at(-1);
assert.equal(timeline.children.length, 80);
assert.doesNotMatch(first.children[1].innerHTML, /data-step-id="1"/, '折叠阶段不挂载子步骤');
assert.doesNotMatch(first.children[1].innerHTML, /example\.ts/, '折叠步骤不序列化详细参数');
assert.match(first.children[1].innerHTML, /data-step-id="8"/, '仍展示默认展开阶段');
assert.equal(last.children[2].innerHTML, '', '活动轮次不显示旧的最终结果');

calls.markdown = calls.steps = 0;
const timelineWrites = timeline.writes;
const firstStepWrites = first.children[1].writes;
for (let tick = 0; tick < 12; tick++) {
    const active = traces.at(-1);
    active.steps = [...active.steps.slice(0, -1), { ...active.steps.at(-1), action: `活动步骤 ${tick}` }];
    paint();
}
assert.deepEqual(calls, { markdown: 0, steps: 12 }, '活动轮次更新不重复解析历史');
assert.equal(timeline.writes, timelineWrites);
assert.equal(timeline.children[0], first);
assert.equal(timeline.children[1], second);
assert.equal(timeline.children.at(-1), last);
assert.equal(first.children[1].writes, firstStepWrites);
assert.match(last.children[1].innerHTML, /活动步骤 11/);

run(`S.expandedTurns.set('run-0', { plans: new Map([['plan-first', true]]),
  thoughts: new Set(['1']), steps: new Set(['1']), revision: 1 })`);
paint();
assert.match(first.children[1].innerHTML, /data-step-id="1"/);
assert.match(first.children[1].innerHTML, /思考内容/);
assert.match(first.children[1].innerHTML, /&lt;script&gt;example&lt;\/script&gt;/);
assert.doesNotMatch(second.children[1].innerHTML, /data-step-id="1"/, '各轮相同步骤和阶段 ID 互不干扰');

traces.splice(0, 2, traces[1], traces[0]);
paint();
assert.equal(timeline.children[0], second);
assert.equal(timeline.children[1], first);

async function verifyDetailCache() {
    let requests = 0;
    let fail = false;
    context.api = async url => {
        requests++;
        assert.equal(url, '/channel/trace/run-1');
        if (fail) throw new Error('temporary unavailable');
        return { trace: traces[0] };
    };
    context.summary = { id: 'run-1', step_count: 8, updated_at: 'now', status: 'succeeded' };
    run('const details = new Map()');
    const first = await run('fetchTraceDetail(summary, details)');
    assert.equal(await run('fetchTraceDetail(summary, details)'), first);
    assert.equal(requests, 1, '已读轮次没有重复请求详情');
    context.summary.updated_at = 'later';
    fail = true;
    assert.equal((await run('fetchTraceDetail(summary, details)')).steps, first.steps);
    context.summary.step_count = 0;
    await run('fetchTraceDetail(summary, details)');
    assert.equal(requests, 2, '明确没有步骤时直接使用摘要');
}

async function verifyColdHistory() {
    const stored = Array.from({ length: 9 }, (_, index) => ({
        id: `cold-${index}`, step_count: 1, updated_at: 'first', goal: '重复的任务内容',
        status: index === 8 ? 'running' : 'succeeded',
    }));
    const requests = [];
    const inflight = new Map();
    let activeRunId = 'cold-8';
    let maxOlder = 0;
    const oldestInFlight = () => [...inflight.keys()].filter(id => id !== 'cold-8').length;
    context.api = async (url, options) => {
        if (url === '/channel/runs') {
            return { runs: [{ session_id: 'cold', run_id: activeRunId, status: 'running', goal_preview: '重复的任务内容' }] };
        }
        if (url.endsWith('/traces')) {
            assert.equal(options.headers['X-Channel-Trace-Metadata'], 'compact');
            return { traces: url.includes('/cold/') ? stored.map(trace => ({ ...trace })) : [] };
        }
        if (url.endsWith('/artifacts')) return { artifacts: [] };
        assert.match(url, /^\/channel\/trace\/cold-[0-8]$/);
        const id = url.slice(url.lastIndexOf('/') + 1);
        const trace = { ...stored.find(trace => trace.id === id), steps: [{ id: 1 }] };
        requests.push([id, trace.updated_at]);
        assert.ok(!inflight.has(id), '同一轮详情不会重复并发请求');
        return new Promise(resolve => {
            inflight.set(id, () => { inflight.delete(id); resolve({ trace }); });
            maxOlder = Math.max(maxOlder, oldestInFlight());
        });
    };
    const settle = id => inflight.get(id)();
    function choose(sessionId) {
        context.target = sessionId;
        run(`S.view = { name: 'flow' }; S.currentSessionId = target; S.flow = null;`);
        return run('loadFlow(target)');
    }
    async function until(predicate) {
        for (let i = 0; i < 50; i++) {
            if (predicate()) return;
            await new Promise(resolve => setImmediate(resolve));
        }
        assert.fail('等待模拟详情超时');
    }
    run(`S.sessions = [{id: 'cold'}, {id: 'new'}]; renderFlow = () => {};`);
    let metadataFinished = false;
    const firstPaint = choose('cold').then(() => { metadataFinished = true; });
    await until(() => inflight.has('cold-8'));
    assert.equal(run('S.flow.traces.length'), 9, '旧详情在途时先显示会话和各轮摘要');
    assert.equal(run('S.flow.run.run_id'), 'cold-8', '主操作已绑定到当前运行');
    assert.deepEqual(requests, [['cold-8', 'first']], '第一条详情请求优先给最新轮次');
    await until(() => metadataFinished);
    await firstPaint;
    settle('cold-8');
    await until(() => inflight.has('cold-4'));
    assert.deepEqual([...inflight.keys()], ['cold-7', 'cold-6', 'cold-5', 'cold-4']);

    stored[8].updated_at = 'updated';
    const liveRefresh = run("loadFlow('cold')");
    await until(() => inflight.has('cold-8'));
    settle('cold-8');
    await liveRefresh;
    await until(() => run('S.flow.traces.at(-1).steps?.length === 1'));
    assert.equal(run('S.flow.traces.at(-1).updated_at'), 'updated', '较早详情未返回时最新轮仍可更新');
    ['cold-7', 'cold-6', 'cold-5', 'cold-4'].forEach(settle);
    await until(() => inflight.has('cold-0'));
    [...inflight.keys()].forEach(settle);
    await until(() => run('S.flow.traces.every(trace => trace.steps?.length === 1)'));
    assert.equal(maxOlder, 4);
    assert.equal(requests.length, 10, '已收取详情的历史轮次不会在轮询中重复请求');

    activeRunId = 'new-repeat';
    await run("loadFlow('cold')");
    assert.equal(run('S.flow.traces.length'), 10, '重复的提示词仍有自己的新执行占位');
    assert.equal(run('S.flow.traces.at(-1).id'), 'new-repeat');

    stored[1].updated_at = 'late';
    await run("loadFlow('cold')");
    assert.ok(inflight.has('cold-1'));
    await choose('new');
    settle('cold-1');
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(run('S.flow.sessionId'), 'new', '切换会话后旧详情不得发布');
    assert.equal(run('S.flow.traces.length'), 0);
}

async function verifyReconnectWhilePriorityDetailIsStalled() {
    let version = 'before-drop';
    let runsRequests = 0;
    const detailRequests = new Map();
    run("S.view = { name: 'flow' }; S.currentSessionId = 'weak'; S.sessions = [{ id: 'weak' }]; S.flow = null;");
    context.api = url => {
        if (url === '/channel/runs') {
            runsRequests++;
            return Promise.resolve({ runs: [{ session_id: 'weak', run_id: 'weak-run', state_detail: version, status: 'running' }] });
        }
        if (url.endsWith('/traces')) {
            return Promise.resolve({ traces: [{ id: 'weak-run', status: 'running', step_count: 1, updated_at: version }] });
        }
        if (url.endsWith('/artifacts')) return Promise.resolve({ artifacts: [] });
        assert.equal(url, '/channel/trace/weak-run');
        const responseVersion = version;
        return new Promise(resolve => {
            detailRequests.set(responseVersion, () => resolve({ trace: {
                id: 'weak-run', updated_at: responseVersion, steps: [{ id: responseVersion }],
            } }));
        });
    };
    const drain = () => new Promise(resolve => setImmediate(resolve));
    void run("loadFlow('weak')");
    await drain();
    assert.equal(run('S.flow.run.state_detail'), 'before-drop');
    assert.equal(typeof detailRequests.get('before-drop'), 'function');

    version = 'reconnected';
    await run("loadFlow('weak')");
    assert.equal(runsRequests, 2, '网络恢复后旧详情不能占住快照刷新名额');
    assert.equal(run('S.flow.run.state_detail'), 'reconnected');
    detailRequests.get('reconnected')();
    await drain();
    assert.equal(run('S.flow.traces[0].steps[0].id'), 'reconnected');

    detailRequests.get('before-drop')();
    await drain();
    assert.equal(run('S.flow.traces[0].steps[0].id'), 'reconnected', '旧详情恢复后也不能覆盖新快照');
}

async function main() {
    await verifyDetailCache();
    await verifyColdHistory();
    await verifyReconnectWhilePriorityDetailIsStalled();
    console.log('PASS: mobile keyed turns, long-chat live costs, scoped expansion, staged detail and stalled-detail reconnect');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
