const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require(process.cwd() + '/node_modules/typescript');
const vm = require('node:vm');
const source = fs.readFileSync('src/components/agent/AgentChatStream.vue', 'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('component.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const node = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'isHistoryTurnCoveredByLive');
assert(node);
const partial = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'historyStepsNotCoveredByLive');
assert(partial);
const code = ts.transpileModule(partial.getText(ast) + '\n' + node.getText(ast) + '\n({isHistoryTurnCoveredByLive, historyStepsNotCoveredByLive});', {compilerOptions:{module:ts.ModuleKind.CommonJS}}).outputText;
const props = {isRunning:true,activeRunId:'new',steps:[]};
const {isHistoryTurnCoveredByLive: f, historyStepsNotCoveredByLive: remaining} = vm.runInNewContext(code,{props});
const old = {id:'old',messages:[],artifacts:[],steps:[{id:'history-new-step-1-0',outputId:'receipt-1'}]};
const current = {id:'new',messages:[],artifacts:[],steps:[{id:'history-new-step-1-0',outputId:'receipt-1'}]};
assert.equal(f(old),false); assert.equal(f(current),false);
props.steps=[{id:'step-1',outputId:'receipt-1'}]; assert.equal(f(old),false); assert.equal(f(current),true);
props.activeRunId=null; assert.equal(f(current),false);
props.activeRunId='new'; current.steps.push({id:'2',outputId:'receipt-2'}); assert.equal(f(current),false); assert.equal(remaining(current).length, 1); assert.equal(remaining(current)[0].id, '2');
props.steps.push({id:'retry',outputId:'receipt-1'}); assert.equal(remaining(current).length, 2);

const sessionSource = fs.readFileSync('src/composables/useAgentSession.ts', 'utf8');
const sessionAst = ts.createSourceFile('session.ts', sessionSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const sessionHelperNames = [
    'isMessageInsideRunInterval',
    'findRunReplyMessageOwners',
    'findRunGoalMessageIds',
    'parsePersistedMessageTimestamp',
    'selectMessagesForRun',
    'selectLegacyConversationMessages',
    'findPausedStepId',
];
const sessionNodes = sessionHelperNames.map((name) => {
    const decl = sessionAst.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === name);
    assert(decl, `missing ${name}`);
    return decl.getText(sessionAst);
});
const matcherCode = ts.transpileModule(sessionNodes.join('\n\n'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const sessionExports = {};
vm.runInNewContext(matcherCode, { exports: sessionExports });
const {
    isMessageInsideRunInterval,
    findRunReplyMessageOwners,
    findRunGoalMessageIds: findGoalIds,
    selectMessagesForRun,
    selectLegacyConversationMessages,
    findPausedStepId,
} = sessionExports;
const runs = [
    { id: 'A', goal: '讲一个很长的故事', startedAt: 10000, completedAt: 13000, summary: '', steps: [] },
    { id: 'B', goal: '换个任务', startedAt: 13100, completedAt: 16000, summary: '', steps: [] },
    { id: 'C', goal: '换个任务', startedAt: 16100, completedAt: 19000, summary: '', steps: [] },
];
const messages = [
    ['legacy', '换个任务', 1000],
    ['start-A', '讲一个很长的故事', 10001],
    ['start-B', '换个任务', 13101],
    ['answer-B', '换个任务', 14000],
    ['start-C', '换个任务', 16101],
].map(([id, content, ms]) => ({ id, role: 'user', content, created_at: new Date(ms).toISOString() }));
assert.deepEqual([...findGoalIds(messages, runs)], ['start-A', 'start-B', 'start-C']);
assert.deepEqual([...findGoalIds(messages, [{ ...runs[0], goalMessageId: 'explicit-id' }])], ['explicit-id']);
// Keep a message if two actual runs overlap at its timestamp.
assert.deepEqual([...findGoalIds(messages, [...runs, { ...runs[1], id: 'overlap' }])], ['start-A', 'start-C']);

// Adjacent runs (<5s apart) must NOT leak non-goal replies into legacy messages.
assert.equal(isMessageInsideRunInterval(14000, runs[0]), false);
assert.equal(isMessageInsideRunInterval(14000, runs[1]), true);
const goalIds = findGoalIds(messages, runs);
const emptyOwners = findRunReplyMessageOwners(runs);
assert.deepEqual(
    selectMessagesForRun(messages, runs[1], runs, goalIds, emptyOwners).map(m => m.id),
    ['answer-B'],
);
assert.deepEqual(
    selectLegacyConversationMessages(messages, runs, goalIds, emptyOwners, { isRunning: false }).map(m => m.id),
    ['legacy'],
);

// Explicit human_reply view.message_id binds a reply directly to its owning run,
// even when timestamps overlap or fall outside updatedAt; ambiguous cross-run IDs fall back.
const runsWithReplyView = [
    { ...runs[0], steps: [{ tool: 'ask_human', view: { kind: 'human_reply', message_id: 'answer-B' } }] },
    runs[1],
    runs[2],
];
const replyOwners = findRunReplyMessageOwners(runsWithReplyView);
assert.equal(replyOwners.get('answer-B'), 'A');
assert.deepEqual(
    selectMessagesForRun(messages, runsWithReplyView[0], runsWithReplyView, goalIds, replyOwners).map(m => m.id),
    ['answer-B'],
);
assert.deepEqual(
    selectMessagesForRun(messages, runsWithReplyView[1], runsWithReplyView, goalIds, replyOwners).map(m => m.id),
    [],
);
assert.deepEqual(
    selectLegacyConversationMessages(
        [...messages, { id: 'late-reply', role: 'user', content: '好', created_at: new Date(99999).toISOString() }],
        [{ ...runs[0], steps: [{ action: 'ask', view: { kind: 'human_reply', message_id: 'late-reply' } }] }],
        new Set(['start-A']),
        new Map([['late-reply', 'A']]),
        { isRunning: false },
    ).map(m => m.id),
    ['legacy', 'start-B', 'answer-B', 'start-C'],
);
assert.equal(
    findGoalIds(
        [{ id: 'reply-first', role: 'user', content: '讲一个很长的故事', created_at: new Date(10001).toISOString() }],
        [runs[0]],
        new Map([['reply-first', 'A']]),
    ).has('reply-first'),
    false,
);
assert.equal(
    findRunReplyMessageOwners([
        runsWithReplyView[0],
        { ...runs[1], steps: [{ tool: 'ask_human', view: { kind: 'human_reply', message_id: 'answer-B' } }] },
    ]).get('answer-B'),
    null,
);
// Running active session uses exact run start without -5000ms slack.
assert.deepEqual(
    selectLegacyConversationMessages(
        [{ id: 'just-before', role: 'user', content: '旧消息', created_at: new Date(19000).toISOString() },
         { id: 'in-run', role: 'user', content: '运行中回复', created_at: new Date(20000).toISOString() }],
        [],
        new Set(),
        new Map(),
        { isRunning: true, runStartedAt: 20000 },
    ).map(m => m.id),
    ['just-before'],
);
// Subsequent pause must not hijack an earlier completed ask_human step.
assert.equal(
    findPausedStepId([
        { id: 'step-0', status: 'succeeded', tool: 'ask_human', action: 'ask_human' },
        { id: 'step-1', status: 'running', tool: 'local_shell', action: 'exec' },
    ]),
    'step-1',
);
assert.equal(
    findPausedStepId([
        { id: 'step-0', status: 'succeeded', tool: 'ask_human', action: 'ask_human' },
    ]),
    undefined,
);
assert.equal(
    findPausedStepId([
        { id: 'step-0', status: 'succeeded', tool: 'ask_human', action: 'ask_human' },
        { id: 'step-1', status: 'running', tool: 'ask_human', action: 'ask_human' },
    ]),
    'step-1',
);

const replyMatcher = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'matchSavedHumanReplies');
assert(replyMatcher);
const matchReplies = vm.runInNewContext(ts.transpileModule(
    replyMatcher.getText(ast) + '\nmatchSavedHumanReplies;',
    { compilerOptions: { target: ts.ScriptTarget.ES2022 } },
).outputText);
const ask = (id, timestamp) => ({ id, tool: 'ask_human', timestamp, rawResult: '人类的回复是: 好' });
const reply = (id, timestamp) => ({ id, role: 'user', timestamp, content: '好' });
const asks = [ask('question-1', 1069), ask('question-2', 14023)];
const replies = [reply('answer-1', 1000), reply('answer-2', 14000)];
const matched = matchReplies(asks, replies);
assert.equal(matched.get('answer-1').id, 'question-1');
assert.equal(matched.get('answer-2').id, 'question-2');
assert.equal(matchReplies(asks, [...replies, reply('ambiguous', 13999)]).has('answer-2'), false);
assert.equal(
    matchReplies(
        [{ id: 'ask-alias', action: 'ask', timestamp: 2000, view: { kind: 'human_reply', message_id: 'answer-1' } }],
        replies,
    ).get('answer-1').id,
    'ask-alias',
);
assert.equal(
    matchReplies(
        [{ id: 'recovery-confirm', tool: 'ask_human', action: 'checkpoint_recovery', timestamp: 2500, params: { question: '系统恢复后正在等待确认' }, view: { kind: 'human_reply', message_id: 'answer-1' } }],
        replies,
    ).get('answer-1').id,
    'recovery-confirm',
);

// 显式 message.run_id 归属（运行中补充与恢复回答即使晚于 updatedAt，也直接归入消费它的 run）
const claimRuns = [
    { id: 'run-crashed', goal: '任务A', summary: '', startedAt: 1000, completedAt: 1200, steps: [] },
    { id: 'run-next', goal: '任务B', summary: '', startedAt: 2000, completedAt: 3000, steps: [] },
];
const claimMessages = [
    { id: 'goal-a', role: 'user', content: '任务A', created_at: new Date(1000).toISOString(), run_id: 'run-crashed' },
    { id: 'late-supp-claimed-by-next', role: 'user', content: '顺便修复测试', created_at: new Date(1500).toISOString(), run_id: 'run-next' },
    { id: 'recovery-after-updated', role: 'user', content: '确认继续', created_at: new Date(1400).toISOString(), run_id: 'run-crashed' },
    { id: 'legacy-unowned', role: 'user', content: '旧闲聊', created_at: new Date(500).toISOString(), run_id: null },
];
const claimGoalIds = findGoalIds(claimMessages, claimRuns, new Map());
assert.deepEqual(
    selectMessagesForRun(claimMessages, claimRuns[0], claimRuns, claimGoalIds, new Map()).map(m => m.id),
    ['recovery-after-updated'],
);
assert.deepEqual(
    selectMessagesForRun(claimMessages, claimRuns[1], claimRuns, claimGoalIds, new Map()).map(m => m.id),
    ['late-supp-claimed-by-next'],
);
assert.deepEqual(
    selectLegacyConversationMessages(claimMessages, claimRuns, claimGoalIds, new Map(), { isRunning: false, now: 4000 }).map(m => m.id),
    ['legacy-unowned'],
);

console.log('History ownership cases passed');
