// Run: node scripts/test-mobile-agent-scroll-follow.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent/channel/http/mobile_web/app.js'), 'utf8');
const elements = new Map();
const handlers = new Map();
const frames = new Map();
const writes = [];
const root = { scrollHeight: 1600, clientHeight: 600 };
let frameId = 0;
let hit = null;
let anchorDocTop = 760;

function element(id) {
    if (!elements.has(id)) {
        const classes = new Set();
        elements.set(id, {
            classList: {
                add: value => classes.add(value), remove: value => classes.delete(value),
                contains: value => classes.has(value),
                toggle: (value, force) => (force ? classes.add(value) : classes.delete(value)),
            },
        });
    }
    return elements.get(id);
}

const window = {
    scrollY: 0,
    requestAnimationFrame(fn) { frames.set(++frameId, fn); return frameId; },
    cancelAnimationFrame(id) { frames.delete(id); },
    addEventListener(name, handler) { handlers.set(name, handler); },
    scrollTo(x, y) { this.scrollY = y; writes.push(y); },
};
const document = {
    getElementById: element, scrollingElement: root, documentElement: root,
    querySelector() { return null; },
    elementFromPoint() { return hit; },
};
const context = vm.createContext({
    localStorage: { getItem() { return null; } }, window, document, URL,
    location: { href: 'http://127.0.0.1/' }, CSS: { escape: String },
});
vm.runInContext(source, context);

function run(script) { return vm.runInContext(script, context); }
function emit(name, event) { handlers.get(name)?.(event); }
function flushFrame() {
    const next = [...frames.values()];
    frames.clear();
    next.forEach(fn => fn());
}
const at = y => { window.scrollY = y; emit('scroll'); };
const latestHidden = () => element('btnScrollBottom').classList.contains('hidden');
const wheel = deltaY => emit('wheel', { deltaY, deltaX: 0, target: {} });

const timeline = element('timeline');
timeline.getBoundingClientRect = () => ({ left: 0, right: 390 });
timeline.contains = node => node === hit;
const turn = {
    isConnected: true,
    getBoundingClientRect: () => ({ top: 700 - window.scrollY }),
    querySelector(selector) {
        assert.equal(selector, '[data-step-id="15"]');
        return hit;
    },
};
function renderedStep() {
    return {
        isConnected: true, dataset: { stepId: '15' },
        closest(selector) { return selector === '[data-turn-id]' ? turn : this; },
        getBoundingClientRect: () => ({ top: anchorDocTop - window.scrollY }),
    };
}
hit = renderedStep();

run(`S.view = { name: 'flow' }; S.currentSessionId = 'A'; S.flow = { sessionId: 'A' };
    bindFlowScroll(); scheduleFlowScroll(); scheduleFlowScroll(); scheduleFlowScroll();`);
assert.equal(frames.size, 1, 'concurrent updates share one viewport write');
flushFrame();
assert.equal(window.scrollY, 1000);
assert.equal(latestHidden(), true);

run('scheduleFlowScroll()');
wheel(-30);
root.scrollHeight = 1700;
flushFrame();
assert.equal(window.scrollY, 1000, 'wheel intent stops a queued follow before the native scroll');
assert.equal(latestHidden(), false);
at(720);

run('const flowScrollTestAnchor = captureFlowAnchor()');
const stale = hit;
hit = renderedStep();
stale.isConnected = false;
anchorDocTop += 106;
root.scrollHeight += 106;
run('scheduleFlowScroll(flowScrollTestAnchor)');
flushFrame();
assert.equal(window.scrollY, 826, 'replaced step stays at its original viewport offset');

const stableWrites = writes.length;
root.scrollHeight += 240;
run('scheduleFlowScroll(captureFlowAnchor())');
emit('mobile:composer-geometry');
flushFrame();
assert.equal(window.scrollY, 826);
assert.equal(writes.length, stableWrites, 'updates below the anchor never write the scroll position');

element('btnScrollBottom').onclick();
flushFrame();
assert.equal(window.scrollY, root.scrollHeight - root.clientHeight);
assert.equal(latestHidden(), true);
const target = {};
emit('touchstart', { touches: [{ clientX: 40, clientY: 200 }], target });
emit('touchmove', { touches: [{ clientX: 41, clientY: 228 }], target });
assert.equal(latestHidden(), false, 'touch toward history detaches immediately');
at(900);
at(root.scrollHeight - root.clientHeight - 24);
assert.equal(latestHidden(), true, 'manually returning near the latest output reattaches');

document.activeElement = element('followInput');
at(940);
root.scrollHeight += 200;
emit('mobile:composer-geometry');
flushFrame();
assert.equal(window.scrollY, root.scrollHeight - root.clientHeight, 'keyboard geometry preserves follow');
assert.equal(latestHidden(), true);
run(`scheduleFlowScroll();
    S.view = { name: 'home' }; S.flow = null; resetFlowScroll();`);
assert.equal(frames.size, 0, 'leaving the session cancels pending viewport work');
assert.equal(latestHidden(), true);

run(`S.view = { name: 'flow' }; S.currentSessionId = 'B'; S.flow = { sessionId: 'B' }; scheduleFlowScroll();`);
at(0);
flushFrame();
assert.equal(window.scrollY, root.scrollHeight - root.clientHeight, 'document clamping before opening a new session does not detach');

emit('touchstart', { touches: [{ clientX: 40, clientY: 200 }], target });
emit('touchmove', { touches: [{ clientX: 40, clientY: 200 }, { clientX: 80, clientY: 250 }], target });
emit('touchmove', { touches: [{ clientX: 41, clientY: 320 }], target });
assert.equal(latestHidden(), true, 'pinch zoom does not detach following');

console.log('PASS: mobile live follow, independent manual reading, scoped step anchors, keyboard and session reset');
