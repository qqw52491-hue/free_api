// Run: node scripts/test-mobile-agent-session-scope.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { webcrypto } = require('node:crypto');

const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent/channel/http/mobile_web/app.js'), 'utf8');

function deferred() {
    let resolve, reject;
    const promise = new Promise((done, failed) => { resolve = done; reject = failed; });
    return { promise, resolve, reject };
}

function createPage(runs, storage = new Map()) {
    const elements = new Map();
    const requests = [];
    const frames = new Map();
    const properties = new Map();
    let nextFrame = 0;
    let admissionHandler;
    let homeError;
    const visualViewport = Object.assign(new EventTarget(), { height: 800, offsetTop: 0, scale: 1 });
    const fakeWindow = Object.assign(new EventTarget(), {
        visualViewport, innerHeight: 800, scrollTo() {},
        requestAnimationFrame(callback) { frames.set(++nextFrame, callback); return nextFrame; },
        cancelAnimationFrame(id) { frames.delete(id); },
    });
    const getElementById = id => {
        if (!elements.has(id)) {
            const classes = new Set(id === 'modalNew' ? ['hidden'] : []);
            elements.set(id, {
                id, value: '', dataset: {}, writes: 0, html: '', style: {},
                get innerHTML() { return this.html; },
                set innerHTML(value) { this.writes++; this.html = value; },
                classList: {
                    add(item) { classes.add(item); },
                    remove(item) { classes.delete(item); },
                    contains(item) { return classes.has(item); },
                    toggle(item, show = !classes.has(item)) { if (show) classes.add(item); else classes.delete(item); },
                },
                setAttribute(name, value) { this[name] = value; },
                addEventListener() {}, click() {}, matches() { return id === 'followInput'; },
            });
        }
        return elements.get(id);
    };
    const fakeDocument = Object.assign(new EventTarget(), {
        getElementById, documentElement: { style: { setProperty: (key, value) => properties.set(key, value) } },
    });
    function flushFrame() {
        const callbacks = Array.from(frames.values());
        frames.clear();
        for (const callback of callbacks) callback();
    }
    const localStorage = { getItem() { return null; } };
    const sessionStorage = {
        getItem(key) { return storage.get(key) ?? null; },
        setItem(key, value) { storage.set(key, value); },
        removeItem(key) { storage.delete(key); },
    };
    const context = vm.createContext({
        crypto: { getRandomValues: webcrypto.getRandomValues.bind(webcrypto) },
        localStorage, sessionStorage, document: fakeDocument, window: fakeWindow, URL, Event,
        location: { href: 'http://127.0.0.1/' },
    });
    vm.runInContext(source, context);
    const pending = new Map();
    const uploads = [];
    const liveState = vm.runInContext('S', context);
    Object.assign(context, {
        renderSidebar() {}, renderHome() {}, async loadHome() { if (homeError) throw homeError; }, closeSidebarMobile() {}, toast() {},
        uploadFile(sessionId, file) {
            uploads.push([sessionId, file]);
            return pending.get(file)?.promise ?? Promise.resolve();
        },
        api(url, options) {
            const body = options?.body ? JSON.parse(options.body) : undefined;
            requests.push({ url, ...(body ? { body } : {}) });
            if (pending.has(url)) return pending.get(url).promise;
            if (url === '/channel/mobile/task') {
                return admissionHandler ? admissionHandler(body)
                    : Promise.resolve({ session_id: body.session_id || 'new-session', run_id: 'task-run' });
            }
            const responses = {
                '/channel/runs': { runs }, '/channel/sessions': { sessions: [{ id: 'A' }, { id: 'B' }] },
            };
            return Promise.resolve(responses[url] ?? {});
        },
    });
    return {
        ...context, requests, liveState, pending, uploads, getElementById, input: getElementById('followInput'),
        visualViewport, properties, flushFrame,
        setAdmissionHandler(handler) { admissionHandler = handler; },
        setHomeError(error) { homeError = error; },
    };
}

async function main() {
    const activeRuns = [
        { session_id: 'A', run_id: 'run-A', status: 'running' },
        { session_id: 'B', run_id: 'run-B', status: 'running' },
    ];
    const page = createPage(activeRuns);
    await page.openSession('A');
    page.input.value = '草稿 A';
    const slowA = deferred();
    const slowB = deferred();
    page.pending.set('/channel/session/A/traces', slowA);
    page.pending.set('/channel/session/B/traces', slowB);
    const refreshA = page.loadFlow('A');
    const openingB = page.openSession('B');
    assert.equal(page.getElementById('btnSend').disabled, true);
    page.input.value = '发给 B';
    await page.sendComposer();
    await page.stopRun();
    assert(!page.requests.some(({ url }) => /\/run-A\/(goal|cancel)$/.test(url)), 'B 加载时不能修改或停止 A');
    assert(page.requests.some(({ url }) => url === '/channel/session/B/traces'), 'B 的加载无需等待 A');
    slowB.resolve({ traces: [] });
    await openingB;
    assert.equal(page.liveState.flow.sessionId, 'B');
    assert.equal(page.getElementById('btnSend').disabled, false);
    assert.equal(page.input.value, '发给 B');
    slowA.resolve({ traces: [] });
    await refreshA;
    assert.equal(page.liveState.flow.sessionId, 'B', 'A 的迟到回执不会覆盖 B');
    await page.sendComposer();
    assert(page.requests.some(({ url, body }) => url === '/channel/run/run-B/goal' && body.goal === '发给 B'));

    const bTraceUrl = '/channel/session/B/traces';
    const bTraceRequests = () => page.requests.filter(({ url }) => url === bTraceUrl).length;
    const previousCount = bTraceRequests();
    const backgroundB = deferred();
    page.pending.set(bTraceUrl, backgroundB);
    const refreshB = page.loadFlow('B');
    await page.loadFlow('B');
    await page.loadFlow('B');
    assert.equal(bTraceRequests(), previousCount + 1, '同一会话只保留一个在途刷新');
    backgroundB.resolve({ traces: [] });
    await refreshB;
    assert.equal(bTraceRequests(), previousCount + 2, '连续刷新合并为一次后续更新');

    await page.openSession('A');
    assert.equal(page.input.value, '草稿 A', '不同会话的草稿分别保留');

    const afterHome = deferred();
    page.pending.set('/channel/session/A/traces', afterHome);
    const beforeHome = page.loadFlow('A');
    page.goHome();
    afterHome.resolve({ traces: [] });
    await beforeHome;
    assert.equal(page.liveState.view.name, 'home');
    assert.equal(page.liveState.flow, null, '回首页后不应发布已离开的任务状态');

    const questions = createPage(['A', 'B'].map(id => ({
        session_id: id, run_id: `run-${id}`, wait_id: `question-${id}`,
        status: 'waiting_human', paused_form: { kind: 'text', title: `问题 ${id}` },
    })));
    await questions.openSession('A');
    questions.getElementById('humanTextInput').value = '回答 A';
    const slowAnswerA = deferred();
    questions.pending.set('/channel/run/run-A/answer', slowAnswerA);
    const answeringA = questions.window.submitHumanIntervention();
    await questions.openSession('B');
    questions.getElementById('humanTextInput').value = '回答 B';
    const slowAnswerB = deferred();
    questions.pending.set('/channel/run/run-B/answer', slowAnswerB);
    const answeringB = questions.window.submitHumanIntervention();
    const questionBHtml = questions.getElementById('askCard').innerHTML;
    slowAnswerA.resolve({});
    await answeringA;
    assert.equal(questions.liveState.humanForm.waitId, 'question-B');
    assert.equal(questions.liveState.humanForm.isSubmitting, true);
    assert.equal(questions.getElementById('askCard').innerHTML, questionBHtml);
    slowAnswerB.resolve({});
    await answeringB;
    assert.deepEqual(questions.requests.filter(({ url }) => url.endsWith('/answer')), [
        { url: '/channel/run/run-A/answer', body: { text: '回答 A', wait_id: 'question-A' } },
        { url: '/channel/run/run-B/answer', body: { text: '回答 B', wait_id: 'question-B' } },
    ]);

    const waitRuns = [{
        session_id: 'A', run_id: 'run-A', wait_id: 'old-wait', status: 'waiting_human',
        paused_form: { kind: 'text', title: '请选择下一步' },
    }];
    const sameSession = createPage(waitRuns);
    await sameSession.openSession('A');
    const ask = sameSession.getElementById('askCard');
    sameSession.document.activeElement = sameSession.getElementById('humanTextInput');
    const writes = ask.writes;
    await sameSession.loadFlow('A');
    assert.equal(ask.writes, writes, '原问题仍在时保留输入框');
    waitRuns[0] = { ...waitRuns[0], wait_id: 'new-wait' };
    await sameSession.loadFlow('A');
    assert.equal(ask.writes, writes + 1, '切换到同文新问题也必须换掉聚焦的旧表单');
    assert.equal(sameSession.liveState.humanForm.waitId, 'new-wait');
    waitRuns[0] = { ...waitRuns[0], status: 'running' };
    await sameSession.loadFlow('A');
    assert.equal(ask.innerHTML, '', '已回答的问题表单随等待结束撤下');

    const compose = createPage([]);
    compose.bind();
    await compose.openSession('A');
    compose.input.value = 'A 的任务';
    compose.liveState.composerFiles.push('A 文件 1', 'A 文件 2');
    compose.liveState.taskOpts.model_id = 'model-A';
    const firstUpload = deferred();
    compose.pending.set('A 文件 1', firstUpload);
    const sendingA = compose.sendComposer();
    const duplicateUpload = compose.sendComposer();
    assert.deepEqual(compose.uploads, [['A', 'A 文件 1']], '提交尚未完成时不会重复上传附件');
    await compose.openSession('B');
    compose.input.value = 'B 的任务';
    compose.liveState.composerFiles.push('B 文件');
    compose.liveState.taskOpts.model_id = 'model-B';
    firstUpload.resolve();
    await Promise.all([sendingA, duplicateUpload]);
    assert.deepEqual(compose.uploads, [['A', 'A 文件 1'], ['A', 'A 文件 2']]);
    assert.equal(compose.input.value, 'B 的任务', 'A 的提交完成后保留 B 的输入');
    assert.deepEqual(Array.from(compose.liveState.composerFiles), ['B 文件']);
    const submitted = compose.requests.find(({ url }) => url === '/channel/mobile/task').body;
    assert.equal(submitted.session_id, 'A');
    assert.equal(submitted.project_id, null);
    assert.equal(submitted.model_routing.pro, 'model-A');
    await compose.openSession('A');
    assert.equal(compose.input.value, '');
    assert.equal(compose.liveState.composerFiles.length, 0);

    compose.getElementById('btnClip').onclick();
    await compose.openSession('B');
    compose.getElementById('clipInput').onchange({ target: { files: ['A 后选文件'] } });
    assert.deepEqual(Array.from(compose.liveState.composerFiles), ['B 文件']);
    await compose.openSession('A');
    assert.deepEqual(Array.from(compose.liveState.composerFiles), ['A 后选文件']);
    compose.getElementById('btnClip').onclick();
    compose.getElementById('clipInput').onchange({ target: { files: [{name: '<tag>.txt'}] } });
    const filesTray = compose.getElementById('composerFiles');
    assert(filesTray.innerHTML.includes('&lt;tag&gt;.txt'), '附件名按文本展示');
    filesTray.onclick({target: { closest: () => ({dataset: {removeComposerFile: '1'}}) }});
    assert.deepEqual(Array.from(compose.liveState.composerFiles), ['A 后选文件']);
    assert(!filesTray.innerHTML.includes('&lt;tag&gt;.txt'));

    const mobile = createPage([]);
    mobile.bind();
    await mobile.openSession('A');
    const dock = mobile.getElementById('composer');
    Object.assign(mobile.input, { clientWidth: 300, scrollHeight: 34 });
    dock.offsetHeight = 92;
    let geometryEvents = 0;
    mobile.window.addEventListener('mobile:composer-geometry', () => geometryEvents++);
    mobile.flushFrame();
    assert.equal(mobile.input.style.height, '34px');
    assert.equal(mobile.properties.get('--composer-spacer'), '92px');
    assert.equal(geometryEvents, 1);
    mobile.document.activeElement = mobile.input;
    Object.assign(mobile.visualViewport, { height: 490, offsetTop: 30 });
    mobile.visualViewport.dispatchEvent(new Event('resize'));
    mobile.visualViewport.dispatchEvent(new Event('scroll'));
    mobile.flushFrame();
    assert.equal(mobile.properties.get('--keyboard-inset'), '280px');
    assert.equal(mobile.properties.get('--composer-spacer'), '372px');
    assert.equal(geometryEvents, 2, '同一帧合并键盘滚动与缩放通知');
    Object.assign(mobile.input, { value: '多行草稿', scrollHeight: 75 });
    dock.offsetHeight = 128;
    mobile.window.dispatchEvent(new Event('resize'));
    mobile.flushFrame();
    assert.equal(mobile.input.style.height, '75px');
    assert.equal(mobile.properties.get('--composer-spacer'), '408px');
    mobile.visualViewport.scale = 1.3;
    mobile.visualViewport.dispatchEvent(new Event('resize'));
    mobile.flushFrame();
    assert.equal(mobile.properties.get('--keyboard-inset'), '0px', '手动放大不误判为键盘覆盖');
    mobile.window.innerHeight = 490;
    Object.assign(mobile.visualViewport, { scale: 1, offsetTop: 0 });
    mobile.window.dispatchEvent(new Event('resize'));
    mobile.flushFrame();
    assert.equal(mobile.properties.get('--visible-viewport-height'), '490px');
    assert.equal(mobile.properties.get('--composer-spacer'), '128px', '布局视口已缩小时不再叠加键盘占位');
    dock.offsetHeight = 0;
    mobile.goHome();
    mobile.flushFrame();
    assert.equal(mobile.properties.get('--composer-spacer'), '0px');

    const concurrent = createPage(activeRuns);
    await concurrent.openSession('A');
    concurrent.input.value = 'A 的目标';
    const aGoalUrl = '/channel/run/run-A/goal';
    const pendingGoal = deferred();
    concurrent.pending.set(aGoalUrl, pendingGoal);
    const sendingGoal = concurrent.sendComposer();
    const duplicateGoal = concurrent.sendComposer();
    assert.equal(concurrent.requests.filter(({ url }) => url === aGoalUrl).length, 1, '连续点击只提交一次');
    assert.equal(concurrent.getElementById('btnSend').disabled, true);
    await concurrent.openSession('B');
    assert.equal(concurrent.getElementById('btnSend').disabled, false, 'A 提交时 B 可以独立发送');
    concurrent.input.value = 'B 的目标';
    await concurrent.sendComposer();
    assert(concurrent.requests.some(({ url }) => url === '/channel/run/run-B/goal'));
    pendingGoal.resolve({});
    await Promise.all([sendingGoal, duplicateGoal]);
    await concurrent.openSession('A');
    assert.equal(concurrent.getElementById('btnSend').disabled, false, '请求结束重新允许发送');

    const savedAdmissions = new Map();
    const weakRuns = [];
    const weak = createPage(weakRuns, savedAdmissions);
    await weak.openSession('A');
    const flakyFile = deferred();
    weak.pending.set('A 第二个附件', flakyFile);
    weak.input.value = '原任务';
    weak.liveState.composerFiles.push('A 已上传附件', 'A 第二个附件');
    const failedUpload = weak.sendComposer();
    flakyFile.reject(new Error('断线'));
    await failedUpload;
    assert.deepEqual(weak.uploads, [['A', 'A 已上传附件'], ['A', 'A 第二个附件']]);
    assert.equal(weak.requests.filter(({ url }) => url === '/channel/mobile/task').length, 0);
    weak.pending.delete('A 第二个附件');
    const receipts = new Map();
    weak.setAdmissionHandler(async body => {
        receipts.set(body.request_id, { session_id: 'A', run_id: 'accepted-run' });
        weakRuns.push({ session_id: 'A', run_id: 'accepted-run', status: 'running' });
        throw new Error('桌面已收到，手机未收到响应');
    });
    await weak.sendComposer();
    assert.deepEqual(weak.uploads, [['A', 'A 已上传附件'], ['A', 'A 第二个附件'], ['A', 'A 第二个附件']]);
    const firstTask = weak.requests.find(({ url }) => url === '/channel/mobile/task').body;
    assert.equal(firstTask.goal, '原任务');
    assert.match(firstTask.request_id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await weak.loadFlow('A');
    assert.equal(weak.liveState.flow.run.run_id, 'accepted-run');
    assert.equal(weak.getElementById('btnSend').title, '核对原提交');
    weak.input.value = '后来编辑的新草稿';
    weak.setAdmissionHandler(async body => receipts.get(body.request_id));
    await weak.sendComposer();
    assert.equal(receipts.size, 1);
    assert.deepEqual(weak.requests.filter(({ url }) => url === '/channel/mobile/task').map(({ body }) => body), [firstTask, firstTask]);
    assert(!weak.requests.some(({ url }) => url === '/channel/run/accepted-run/goal'));
    assert.equal(weak.input.value, '后来编辑的新草稿');
    assert.equal(weak.liveState.composerFiles.length, 0);

    const reloadStorage = new Map();
    const firstPage = createPage([], reloadStorage);
    await firstPage.openSession('B');
    firstPage.input.value = '重载前未确认';
    firstPage.setAdmissionHandler(async () => { throw new Error('响应中断'); });
    await firstPage.sendComposer();
    const beforeReload = firstPage.requests.find(({ url }) => url === '/channel/mobile/task').body;
    const reloaded = createPage([{ session_id: 'B', run_id: 'original', status: 'running' }], reloadStorage);
    await reloaded.openSession('B');
    assert.equal(reloaded.input.value, '重载前未确认');
    assert.equal(reloaded.getElementById('btnSend').title, '核对原提交');
    await reloaded.sendComposer();
    assert.deepEqual(reloaded.requests.find(({ url }) => url === '/channel/mobile/task').body, beforeReload);
    assert(!reloaded.requests.some(({ url }) => /\/goal$/.test(url)));

    const stale = createPage([]);
    stale.liveState.sessions.push({ id: 'A', project_id: 'project-old' });
    const staleRead = deferred();
    staleRead.resolve({ session: null, traces: [] });
    stale.pending.set('/channel/session/A/traces', staleRead);
    await stale.openSession('A');
    stale.input.value = '过期项目草稿';
    await stale.sendComposer();
    assert.equal(stale.getElementById('btnSend').disabled, true);
    assert.equal(stale.input.value, '过期项目草稿');
    assert(!stale.requests.some(({ url }) => url === '/channel/mobile/task'));

    const mismatched = createPage([]);
    const projectRead = deferred();
    projectRead.resolve({ session: { id: 'B', project_id: 'project-original' }, traces: [] });
    mismatched.pending.set('/channel/session/B/traces', projectRead);
    await mismatched.openSession('B');
    mismatched.input.value = '原工作区的草稿';
    mismatched.setAdmissionHandler(async () => {
        throw Object.assign(new Error('归属已变更'), { status: 409, code: 'session_project_mismatch' });
    });
    await mismatched.sendComposer();
    assert.equal(mismatched.requests.find(({ url }) => url === '/channel/mobile/task').body.project_id, 'project-original');
    await mismatched.sendComposer();
    assert.equal(mismatched.requests.filter(({ url }) => url === '/channel/mobile/task').length, 1);
    assert.equal(mismatched.getElementById('btnSend').disabled, true);
    assert.equal(mismatched.input.value, '原工作区的草稿');

    const moved = createPage([]);
    const oldScope = deferred();
    oldScope.resolve({ session: { id: 'A', project_id: 'project-original' }, traces: [] });
    moved.pending.set('/channel/session/A/traces', oldScope);
    await moved.openSession('A');
    moved.input.value = '属于原项目的草稿';
    const newScope = deferred();
    newScope.resolve({ session: { id: 'A', project_id: 'project-new' }, traces: [] });
    moved.pending.set('/channel/session/A/traces', newScope);
    await moved.loadFlow('A');
    await moved.sendComposer();
    assert.equal(moved.input.value, '属于原项目的草稿');
    assert.equal(moved.getElementById('btnSend').disabled, true);
    assert(!moved.requests.some(({ url }) => url === '/channel/mobile/task'));

    const busyRuns = [];
    const busy = createPage(busyRuns);
    await busy.openSession('B');
    busy.input.value = '草稿仍保留';
    busy.setAdmissionHandler(async () => {
        busyRuns.push({ session_id: 'B', run_id: 'other-run', status: 'running' });
        throw Object.assign(new Error('会话已有任务运行'), { status: 409, code: 'session_in_flight' });
    });
    await busy.sendComposer();
    await busy.loadFlow('B');
    assert.equal(busy.getElementById('btnSend').title, '发送', '明确未受理时解除核对状态');
    assert.equal(busy.input.value, '草稿仍保留');

    const fresh = createPage([]);
    fresh.liveState.models = [{ id: 'model-1' }];
    fresh.setHomeError(new Error('列表请求暂时失败'));
    fresh.openModal();
    fresh.getElementById('newGoal').value = '弱网下启动新任务';
    fresh.setAdmissionHandler(async () => { throw new Error('启动确认丢失'); });
    await fresh.submitNew();
    assert.equal(fresh.getElementById('modalNew').classList.contains('hidden'), false);
    assert.equal(fresh.getElementById('newSubmit').textContent, '核对原提交');
    const newTask = fresh.requests.find(({ url }) => url === '/channel/mobile/task').body;
    assert.equal(newTask.session_id, undefined);
    assert.equal(newTask.model_id, 'model-1');
    assert.equal(newTask.project_id, null);
    fresh.setAdmissionHandler(async () => ({ session_id: 'A', run_id: 'new-run' }));
    await fresh.submitNew();
    assert.deepEqual(fresh.requests.filter(({ url }) => url === '/channel/mobile/task').map(({ body }) => body), [newTask, newTask]);
    assert(!fresh.requests.some(({ url }) => url === '/channel/session' || url === '/channel/task'));
    assert.equal(fresh.liveState.currentSessionId, 'A');
    assert.equal(fresh.getElementById('modalNew').classList.contains('hidden'), true);

    console.log('PASS: mobile session actions, scoped drafts and attachments, and stable task admission under weak-network retries');
}

main().catch(error => { console.error(error); process.exitCode = 1; });
