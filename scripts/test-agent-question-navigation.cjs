// Synthetic messages and the production timeline projection; no backend or model calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const { load } = require('./test-agent-task-resume.cjs');
const root = path.resolve(__dirname, '../src/components/agent');
const { buildQuestionMarks, questionPreview, questionAnswerPreview } = load(path.join(root, 'questionNavigation.ts'));

const question = (id, text, pending = false) => ({ id, kind: 'question', text, pending });
const answer = (id, text) => ({ id, kind: 'answer', text });
const marks = buildQuestionMarks([
    answer('orphan', '不能配给未来的问题'),
    question('unanswered', '相同问题'),
    question('answered', '相同问题'),
    answer('a', '## 结论\n\n已经 **完成**，见 [证据](https://example.invalid)。'),
    question('live', '新的问题', true),
]);
assert.equal(marks.length, 3, 'repeated text has independent message anchors');
assert.equal(marks[0].answer, '', 'a later answer cannot fill an earlier unanswered question');
assert.equal(marks[1].answer, '结论 已经 完成，见 证据。');
assert.equal(marks[2].answer, '');
assert.equal(marks[2].pending, true);
assert.equal(marks[0].hasAnswer, false);
assert.equal(questionAnswerPreview(marks[0]), '尚未回答');
assert.equal(questionAnswerPreview(marks[2]), '正在处理，尚未回答');
for (const body of ['```rust\nfn main() {}\n```', '📎 [附件: result.xlsx]', '<!-- 无可见摘录 -->']) {
    const [answered, next] = buildQuestionMarks([question('code', '请给示例', true), answer('reply', body), question('next', '下一个问题')]);
    assert.equal(answered.hasAnswer, true);
    assert.equal(answered.pending, false);
    assert.equal(answered.answer, '');
    assert.equal(questionAnswerPreview(answered), '已回答，暂无文字摘录');
    assert.equal(next.hasAnswer, false, 'answer presence cannot leak into the next question');
}
const [blank] = buildQuestionMarks([question('blank', '还在等', true), answer('empty', ' \n ')]);
assert.equal(blank.hasAnswer, false);
assert.equal(blank.pending, true);
assert(fs.readFileSync(path.join(root, 'AgentQuestionNav.vue'), 'utf8').includes('questionAnswerPreview(preview)'), 'tooltip uses the tested answer-presence formatter');
const attachment = buildQuestionMarks([question('attachment', '📎 [附件: image.png]')]);
assert.equal(attachment[0].question, '附件消息');
assert.equal(questionPreview('𠮷'.repeat(100), 72), '𠮷'.repeat(72) + '…', 'Unicode truncation preserves code points');
assert.equal(questionPreview('<!-- hidden -->\n# 标题\n```js\nignored()\n```\n正文', 72), '标题 正文');

const component = fs.readFileSync(path.join(root, 'AgentChatStream.vue'), 'utf8');
const source = component.split('<script setup lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('stream.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
function declaration(name) {
    const node = ast.statements.find(node => ts.isVariableStatement(node)
        && node.declarationList.declarations.some(item => item.name.getText(ast) === name));
    assert(node, name); return node.getText(ast);
}
function fn(name) {
    const node = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name.text === name);
    assert(node, name); return node.getText(ast);
}
let projections = 0;
const props = vue.reactive({
    legacyMessages: [
        { id: 'q0', role: 'user', content: '旧问题' },
        { id: 'a0', role: 'assistant', content: '旧回答' },
        { id: 'legacy-human', role: 'user', content: '已经显示为人工确认' },
    ],
    historyTurns: [],
    humanInterventions: [{ messageId: 'legacy-human' }],
    isRunning: false,
});
const historyTimelineEntries = vue.ref([
    { id: 'history-user-run1', type: 'user', content: '历史问题' },
    { id: 'p1', type: 'plan', status: 'running' },
    { id: 'human1', type: 'message', role: 'user', content: '确认', isHuman: true },
    { id: 'history-completion-run1', type: 'completion', summary: '**历史回答**' },
]);
const liveTimelineEntries = vue.ref([{ id: 'live-review', type: 'review', workers: [] }]);
const compiled = text => ts.transpileModule(text, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
const projection = [
    ...['showLiveTimeline', 'liveHumanMessageIds', 'visibleLegacyMessages'].map(declaration),
    fn('questionNavigationEntries'),
    ...['historicalQuestionNavigationKey', 'liveQuestionNavigationKey', 'questionMarks'].map(declaration),
    '({get marks(){return questionMarks.value}});',
].join('\n');
const state = vm.runInNewContext(compiled(projection), {
    computed: vue.computed, props, historyTimelineEntries, liveTimelineEntries,
    buildQuestionMarks: entries => { projections++; return buildQuestionMarks(entries); },
});
assert.deepEqual(Array.from(state.marks, m => m.id), ['legacy-q0', 'history-user-run1']);
assert.equal(state.marks[0].answer, '旧回答');
assert.equal(state.marks[1].answer, '历史回答');
assert.equal(projections, 1);
for (let i = 0; i < 100; i++) {
    historyTimelineEntries.value[1] = { id: 'p1', type: 'plan', phase: `heartbeat-${i}` };
    liveTimelineEntries.value[0].workers = [{ id: 'review', currentStep: `heartbeat-${i}` }];
    assert.equal(state.marks.length, 2);
}
assert.equal(projections, 1, 'token/heartbeat-only timeline updates do not rebuild every preview');
historyTimelineEntries.value[3].summary = '更新后的真实回答';
assert.equal(state.marks[1].answer, '更新后的真实回答');
props.isRunning = true;
liveTimelineEntries.value.unshift({ id: 'live-user-goal', type: 'user', content: '运行中的新问题' });
assert.equal(state.marks[2].pending, true);
assert.equal(state.marks[2].answer, '', 'a running question cannot borrow the last answer');
liveTimelineEntries.value.push({ id: 'live-completion', type: 'completion', summary: '新回答' });
assert.equal(state.marks[2].answer, '新回答');
assert.equal(state.marks[2].pending, false);
props.legacyMessages = [];
historyTimelineEntries.value = [];
liveTimelineEntries.value = [];
assert.equal(state.marks.length, 0, 'switching to an empty conversation leaves no stale questions');

const calls = [];
const followLiveTail = { value: true };
const activeQuestionId = { value: null };
const target = { scrollIntoView: args => calls.push(['scroll', args]), classList: {
    add: name => calls.push(['highlight', name]), remove: () => {},
} };
const scroller = { scrollHeight: 2000, clientHeight: 600, scrollTop: 1400, querySelector: selector => {
    calls.push(['selector', selector]); return selector.includes('missing') ? null : target;
} };
let queuedFollow = null;
let historyExpanded = false;
const revealWaits = new Map();
assert(component.includes('@before-expand="pauseLiveFollow"'), 'real virtual expansion pauses following before moving the viewport');
const navigationSource = [
    declaration('timelineNavigationRequest'),
    ...['pauseLiveFollow', 'scrollToTimelineEntry', 'jumpToQuestion', 'isNearBottom', 'onStreamScroll', 'followLatest', 'resumeLiveFollow'].map(fn),
    '({pauseLiveFollow,scrollToTimelineEntry,jumpToQuestion,onStreamScroll,followLatest,resumeLiveFollow});',
].join('\n');
const navigation = vm.runInNewContext(compiled(navigationSource), {
    streamElement: { value: scroller }, props: { isRunning: true, viewActive: true }, BOTTOM_THRESHOLD_PX: 56,
    legacyHistoryList: { value: { async reveal(id) { calls.push(['reveal-legacy', id]); return true; } } },
    timelineSectionsRef: { value: { async reveal(id) {
        calls.push(['reveal-timeline', id]);
        if (id === 'history-user-run1' && !historyExpanded) {
            historyExpanded = true;
            navigation.pauseLiveFollow();
        }
        await revealWaits.get(id);
        return !id.includes('missing');
    } } },
    nextTick: vue.nextTick,
    CSS: { escape: id => id }, followLiveTail, activeQuestionId, scrollRafId: 42, navigationLeavingBottom: false,
    requestAnimationFrame: callback => { queuedFollow = callback; return 43; },
    cancelAnimationFrame: id => { calls.push(['cancel', id]); queuedFollow = null; },
    scheduleQuestionPosition: () => {}, setTimeout: () => 0,
});
async function verifyNavigation() {
    const jump = navigation.scrollToTimelineEntry;
    const historyJump = jump('history-user-run1');
    assert.equal(followLiveTail.value, false, 'virtual expansion detaches live follow before revealing an anchor');
    assert(calls.some(([kind, value]) => kind === 'cancel' && value === 42), 'pending tail scroll cannot override navigation');
    assert.equal(await historyJump, true);
    assert(calls.some(([kind, value]) => kind === 'reveal-timeline' && value === 'history-user-run1'));
    assert(calls.some(([kind, value]) => kind === 'selector' && value === '[data-tl="history-user-run1"]'));
    assert(calls.some(([kind, value]) => kind === 'scroll' && value.block === 'center'));
    assert(calls.some(([kind, value]) => kind === 'highlight' && value === 'tl-anchor-flash'));
    await jump('legacy-q0');
    assert(calls.some(([kind, value]) => kind === 'reveal-legacy' && value === 'q0'));
    assert(calls.some(([kind, value]) => kind === 'selector' && value === '[data-tl="legacy-q0"]'));

    // Smooth scrolling emits several events before moving out of the bottom threshold.
    for (const top of [1400, 1399, 1390, 1344, 1343, 900]) {
        scroller.scrollTop = top;
        navigation.onStreamScroll({ currentTarget: scroller });
        navigation.followLatest(); // Simulate the existing watcher receiving new output.
        assert.equal(followLiveTail.value, false, 'programmatic jump must remain detached at ' + top);
        assert.equal(queuedFollow, null, 'new output cannot enqueue a tail scroll during history navigation');
        assert.equal(scroller.scrollTop, top);
    }
    scroller.scrollTop = 1400;
    navigation.onStreamScroll({ currentTarget: scroller });
    assert.equal(followLiveTail.value, true, 'returning to the bottom after leaving it resumes follow');
    await navigation.jumpToQuestion('history-user-run1');
    assert.equal(activeQuestionId.value, 'history-user-run1', 'jump highlights the reached question');
    navigation.resumeLiveFollow();
    assert.equal(followLiveTail.value, true, 'explicit follow overrides a navigation detach immediately');
    assert.equal(typeof queuedFollow, 'function');
    queuedFollow(); queuedFollow = null;
    const scrollCount = calls.filter(([kind]) => kind === 'scroll').length;
    await navigation.jumpToQuestion('missing');
    assert.equal(calls.filter(([kind]) => kind === 'scroll').length, scrollCount, 'a missing anchor cannot scroll another entry');
    assert.equal(followLiveTail.value, true, 'a missing anchor must not detach a reader');
    assert.equal(activeQuestionId.value, 'history-user-run1', 'a missing anchor cannot become the active question');
    await jump('history-user-run1');
    scroller.scrollTop = 1390;
    navigation.onStreamScroll({ currentTarget: scroller });
    assert.equal(followLiveTail.value, false, 'a second jump gets a fresh bottom-threshold guard');

    let finishOlderReveal;
    revealWaits.set('older-question', new Promise(resolve => { finishOlderReveal = resolve; }));
    const older = navigation.jumpToQuestion('older-question');
    await navigation.jumpToQuestion('newer-question');
    assert.equal(activeQuestionId.value, 'newer-question');
    finishOlderReveal();
    await older;
    assert.equal(activeQuestionId.value, 'newer-question', 'the slower reveal cannot replace a newer active mark');
    assert(!calls.some(([kind, value]) => kind === 'selector' && value === '[data-tl="older-question"]'));
    console.log('PASS question navigation: answer presence, stable history/live projection, virtual reveal, active jump ownership and live-follow isolation');
}
verifyNavigation().catch(error => { console.error(error); process.exitCode = 1; });
