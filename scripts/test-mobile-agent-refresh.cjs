// Deterministic timer and EventSource fixtures for the phone console's real app.js.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent/channel/http/mobile_web/app.js'), 'utf8');
const elements = new Map();
const handles = new Map();
const listeners = new Map();
const connections = [];
let handle = 0;
let intervals = 0;
let binds = 0;

function element(id) {
    if (!elements.has(id)) {
        const classes = new Set();
        elements.set(id, {
            classList: {
                add: name => classes.add(name),
                remove: name => classes.delete(name),
                contains: name => classes.has(name),
                toggle(name, enabled) {
                    if (enabled) classes.add(name);
                    else classes.delete(name);
                },
            },
        });
    }
    return elements.get(id);
}

class LocalEventSource {
    readyState = 1;
    constructor() { connections.push(this); }
    close() { this.readyState = 2; }
    emit(sessionId, type) { this.onmessage?.({ data: JSON.stringify({ session_id: sessionId, event: { type } }) }); }
}

const document = {
    hidden: false,
    getElementById: element,
    addEventListener(name, callback) { listeners.set(name, callback); },
};
const createTimeout = (callback, delay) => { handles.set(++handle, { callback, delay }); return handle; };
const context = vm.createContext({
    document, URL, AbortController, location: { href: 'http://127.0.0.1/' },
    window: { EventSource: LocalEventSource }, EventSource: LocalEventSource,
    localStorage: { getItem: () => null },
    setTimeout: createTimeout,
    clearTimeout: id => handles.delete(id),
    setInterval: () => { intervals++; },
});
vm.runInContext(source, context);
const state = vm.runInContext('S', context);
const loadModels = context.loadModels;
const realApi = context.api;
const calls = [];
let slowHome;
Object.assign(context, {
    bind() { binds++; },
    applyTheme() {},
    async loadModels() {},
    async loadHome() { calls.push('home'); if (slowHome) await slowHome; },
    async loadFlow(session) { calls.push(session); },
    async api(route) { return route === '/channel/role-cards' ? { role_cards: [{ name: 'reviewer' }] } : {}; },
});
element('app').classList.remove('hidden');
element('modalNew').classList.add('hidden');
element('artPreview').classList.add('hidden');
state.token = 'fixture';

function runTimer(delay) {
    const scheduled = [...handles].find(([, timer]) => timer.delay === delay);
    assert(scheduled, 'missing timer of ' + delay + 'ms');
    handles.delete(scheduled[0]);
    return scheduled[1].callback();
}

async function main() {
    await loadModels();
    assert.equal(state.roleCards[0].name, 'reviewer', 'use the live route’s role_cards response');
    state.view = { name: 'flow' };
    state.currentSessionId = 'A';
    context.connectSSE();
    const first = connections[0];
    first.onopen();
    for (let i = 0; i < 12; i++) first.emit('A', 'progress');
    first.emit('B', 'progress');
    assert.equal(handles.size, 1);
    await runTimer(150);
    assert.deepEqual(calls, ['A'], 'a burst schedules one refresh for the shown session');
    first.emit('B', 'progress');
    assert.equal(handles.size, 0, 'another session cannot refresh the shown flow');

    state.view = { name: 'home' };
    let finishHome;
    slowHome = new Promise(resolve => { finishHome = resolve; });
    first.emit('A', 'progress');
    const homeRequest = runTimer(400);
    for (let i = 0; i < 8; i++) first.emit('A', 'progress');
    assert.deepEqual(calls, ['A', 'home']);
    assert.equal(handles.size, 0, 'slow responses cannot overlap their next event refresh');
    finishHome();
    slowHome = null;
    await homeRequest;
    await runTimer(400);
    assert.deepEqual(calls, ['A', 'home', 'home'], 'one trailing refresh observes updates during the request');

    await context.boot();
    await context.boot();
    assert.equal(binds, 1);
    assert.equal(intervals, 1, 'signing in again cannot start a second fallback timer');
    first.emit('A', 'progress');
    document.hidden = true;
    listeners.get('visibilitychange')();
    assert.equal(first.readyState, 2);
    assert.equal(handles.size, 0);
    context.pollChannel();
    first.emit('A', 'progress');
    assert.equal(handles.size, 0, 'backgrounded or stale streams do not poll');

    document.hidden = false;
    state.view = { name: 'flow' };
    state.currentSessionId = 'B';
    listeners.get('visibilitychange')();
    assert.equal(connections.length, 2);
    connections[1].onopen();
    await runTimer(150);
    assert.equal(calls.at(-1), 'B', 'returning to the tab refreshes the current session immediately');

    const reconnecting = connections[1];
    reconnecting.readyState = 0;
    reconnecting.onerror();
    assert.equal(handles.size, 0, 'the browser owns retry while EventSource is connecting');
    reconnecting.readyState = 1;
    reconnecting.onopen();
    await runTimer(150);
    assert.equal(calls.at(-1), 'B', 'native reconnect must reload even without a new event');

    reconnecting.close();
    reconnecting.onerror();
    await runTimer(3000);
    assert.equal(connections.length, 3);
    reconnecting.emit('B', 'progress');
    assert.equal(handles.size, 0, 'events queued by the closed connection are ignored');
    const restored = connections[2];
    restored.onopen();
    await runTimer(150);
    assert.equal(calls.at(-1), 'B');
    context.showGate('令牌已失效');
    assert.equal(restored.readyState, 2);
    context.pollChannel();
    assert.equal(handles.size, 0, 'a locked page cannot continue refreshing');

    element('app').classList.remove('hidden');
    let reply;
    context.fetch = () => new Promise(resolve => { reply = resolve; });
    const oldRequest = realApi('/channel/sessions');
    state.token = 'new-fixture';
    reply({ status: 401 });
    await assert.rejects(oldRequest, /unauthorized/);
    assert.equal(element('app').classList.contains('hidden'), false, 'an old response cannot lock the new sign-in');

    context.fetch = async () => ({
        status: 200,
        ok: true,
        async json() { throw new SyntaxError('fixture: incomplete response body'); },
    });
    await assert.rejects(realApi('/channel/session/B/traces'), /响应未收完整/);
    assert.equal(element('connText').textContent, '响应未收完整，等待重试');

    context.fetch = async () => ({ status: 204, ok: true });
    assert.equal(Object.keys(await realApi('/channel/session/B')).length, 0);
    assert.equal(element('connText').textContent, '已连接到当前桌面窗口');

    let bodyStarted;
    const readingBody = new Promise(resolve => { bodyStarted = resolve; });
    context.fetch = async (_url, { signal }) => ({
        status: 200,
        ok: true,
        json: () => new Promise((_, reject) => {
            signal.addEventListener('abort', () => reject(signal.reason), { once: true });
            bodyStarted();
        }),
    });
    const stalledBody = realApi('/channel/session/B/traces');
    await readingBody;
    runTimer(20000);
    await assert.rejects(stalledBody, /读取超时/);
    assert.equal(handles.size, 0);

    context.fetch = async (_url, { signal }) => {
        assert.equal(signal, undefined, 'a running submission is never subject to the read deadline');
        return { status: 204, ok: true };
    };
    await realApi('/channel/run/B/goal', { method: 'POST', body: '{}' });
    assert.equal(handles.size, 0);
}

main().then(() => console.log('PASS: mobile event burst coalescing, reconnect, partial/stalled response, visibility resume and token expiry'));
