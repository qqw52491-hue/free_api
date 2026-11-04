import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/components/agent/delegatedTaskState.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
}).outputText;
const { isSettled, mergeSnapshot, detectAutoOpen, fullCacheIsFresh, createSerialRefresher, settleWithRetry, historicalWorkerSnapshot } = await import(
  'data:text/javascript;base64,' + Buffer.from(js).toString('base64')
);

const running = (id = 'a') => ({ child_run_id: id, execution_status: 'running', execution_ended: false });
const failed = (id = 'a') => ({ child_run_id: id, execution_status: 'failed', execution_ended: true, delivery_accepted: false });
const waiting = (id = 'a') => ({ ...running(id), human_request: { message: '?' } });

test('a late running snapshot never overwrites a settled state', () => {
  assert.equal(mergeSnapshot(failed(), running()).execution_status, 'failed');
  assert.equal(mergeSnapshot(running(), failed()).execution_status, 'failed');
  assert.equal(mergeSnapshot(undefined, running()).execution_status, 'running');
  assert.equal(isSettled({ execution_ended: false, execution_status: 'running' }), false);
});

test('polling an unchanged waiting task does not reopen a manually collapsed dock', () => {
  const active = new Set(), asking = new Set();
  assert.equal(detectAutoOpen(active, asking, [waiting()]), true, 'first sight opens');
  for (let i = 0; i < 5; i++) assert.equal(detectAutoOpen(active, asking, [waiting()]), false);
  assert.equal(detectAutoOpen(active, asking, [waiting('a'), waiting('b')]), true, 'new human request opens');
  assert.equal(detectAutoOpen(active, asking, [waiting('a'), waiting('b')]), false);
  assert.equal(detectAutoOpen(active, asking, [waiting('a'), running('c')]), true, 'new task opens');
  assert.equal(detectAutoOpen(active, asking, [failed('a'), failed('c')]), false, 'settled never opens');
  assert.equal(detectAutoOpen(active, asking, [waiting('a')]), true, 'a request raised again after being answered opens');
});

test('full-content cache is replaced once the task settles', () => {
  const cache = { settled: false };
  assert.equal(fullCacheIsFresh(cache, running()), true);
  assert.equal(fullCacheIsFresh(cache, failed()), false);
  assert.equal(fullCacheIsFresh(undefined, running()), false);
});

test('a refresh requested while another is in flight (session switch) is replayed', async () => {
  const calls = [];
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const refresher = createSerialRefresher(async () => {
    calls.push(calls.length);
    if (calls.length === 1) await gate;
  });
  const first = refresher.request();
  await Promise.resolve();
  refresher.request();
  refresher.request();
  assert.equal(calls.length, 1);
  release();
  await first;
  assert.equal(calls.length, 2, 'coalesced into exactly one follow-up run');
});

test('settlement retries through unsettled results and query failures, and ignores out-of-order running replies', async () => {
  const replies = [() => running(), () => { throw new Error('db busy'); }, () => failed()];
  const applied = [];
  const sleeps = [];
  const outcome = await settleWithRetry(
    async () => replies.shift()(),
    (task) => applied.push(mergeSnapshot(applied.at(-1), task)),
    { delaysMs: [1, 2, 3, 4], sleep: async (ms) => { sleeps.push(ms); } },
  );
  assert.equal(outcome, 'settled');
  assert.deepEqual(sleeps, [1, 2, 3]);
  assert.equal(applied.at(-1).execution_status, 'failed');
  // a stale running reply arriving after the terminal result changes nothing
  assert.equal(mergeSnapshot(applied.at(-1), running()).execution_status, 'failed');
});

test('settlement gives up (unsettled) after the retry budget and stops on session change', async () => {
  assert.equal(await settleWithRetry(async () => running(), () => {}, { delaysMs: [1, 1], sleep: async () => {} }), 'unsettled');
  let calls = 0;
  assert.equal(await settleWithRetry(async () => { calls++; return running(); }, () => {}, {
    delaysMs: [1, 1, 1], sleep: async () => {}, shouldStop: () => true,
  }), 'unsettled');
  assert.equal(calls, 0);
});

test('onProgressEvent logic ignores progress events once task is settled', () => {
  const task = failed('a');
  task.latest_tool = 'finish';
  task.latest_step = '任务已结束';

  // 模拟 onProgressEvent 逻辑：如果 isSettled(task) 则不应被更新
  const event = { tool: 'local_file', description: '正在读取文件...' };
  if (!isSettled(task)) {
    if (event.tool) task.latest_tool = event.tool;
    if (event.description) task.latest_step = event.description;
  }
  assert.equal(task.latest_tool, 'finish');
  assert.equal(task.latest_step, '任务已结束');
});

test('historical rows use matching host settlement without fabricating a result', () => {
  const worker = { id: 'a', status: 'pending', currentStep: 'waiting' };
  assert.equal(historicalWorkerSnapshot(worker, undefined), worker);
  assert.equal(historicalWorkerSnapshot(worker, { child_run_id: 'b', execution_status: 'succeeded', handoff_accepted: true }), worker);
  const handoff = historicalWorkerSnapshot(worker, { child_run_id: 'a', execution_status: 'succeeded', execution_ended: true, handoff_accepted: true });
  assert.equal(handoff.status, 'succeeded');
  assert.equal(handoff.extra.hostTaskState, '已交接');
  assert.equal(historicalWorkerSnapshot(worker, { child_run_id: 'a', execution_status: 'succeeded', execution_ended: true, delivery_accepted: true }).extra.hostTaskState, '已交付');
  assert.equal(historicalWorkerSnapshot(worker, { child_run_id: 'a', execution_status: 'cancelled' }).extra.hostTaskState, '已停止');
  assert.equal(historicalWorkerSnapshot(worker, { child_run_id: 'a', execution_status: 'failed' }).extra.hostTaskState, '未完成');
  assert.equal(historicalWorkerSnapshot(worker, { child_run_id: 'a', execution_status: 'running' }).extra.hostTaskState, '执行中');
});
