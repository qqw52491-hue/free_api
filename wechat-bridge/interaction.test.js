import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import vm from "node:vm";
import { randomUUID } from "node:crypto";
import { restoreRuntime, snapshotRuntime, saveBindings, readRun } from "./runtime-state.js";

const source = fs.readFileSync(new URL("./index.js", import.meta.url), "utf8");
const taskSource = source.slice(source.indexOf("async function buildGoalWithMedia("), source.indexOf("// ---------------------------------------------------------------- 桥 HTTP 服务"));
const startSource = source.slice(source.indexOf("function startBot("), source.indexOf("async function stopMonitor("));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const flush = () => new Promise((resolve) => setImmediate(resolve));

function harness(api, saved = {}) {
  const runtime = restoreRuntime(saved.runtime);
  const state = { owner: "owner", task_counter: 0, senders: { owner: { seen: true, session_id: "s" } }, ...saved };
  const callbacks = [], sent = [], mediaSent = [], cleared = new Set();
  let snapshot;
  const context = {
    ...runtime, state, config: {}, lastSelections: new Map(), submitting: new Set(), answering: new Set(),
    api, readRun, randomUUID, path, bot: {}, runtimeResumed: false,
    senderOf: (id) => state.senders[id],
    saveState: () => { snapshot = JSON.parse(JSON.stringify({ ...state, runtime: snapshotRuntime(runtime) })); },
    console: { log() {}, error() {} }, MAX_QUEUE: 5, MAX_ARTIFACT_BYTES: 25 * 1024 * 1024,
    POLL_INTERVAL_MS: 10_000, HEARTBEAT_MS: 1_800_000, NUDGE_MS: 7_200_000,
    COMMAND_PANEL: "帮助", inQuietHours: () => false,
    botSend: async (text) => { sent.push(text); return true; },
    botSendWithMedia: async (text, media) => { mediaSent.push({ text, media }); return true; },
    setInterval: (callback) => { callbacks.push(callback); return callbacks.length; },
    clearInterval: (id) => cleared.add(id), AbortController,
    start: () => ({ wait: () => new Promise(() => {}) }),
  };
  const code = vm.runInNewContext(`${startSource}\n${taskSource}\n({ agent, watchRun, drainQueue, startBot })`, context);
  return {
    ...code, ...runtime, sent, mediaSent, callbacks, cleared, context, snapshot: () => snapshot,
    chat: (text, media) => code.agent.chat({ conversationId: "owner", text, media }),
    restart: () => { context.bot = null; code.startBot(); },
  };
}

function waiting(api = async () => ({ status: "waiting_human", paused_reason: "选哪个？" })) {
  const h = harness(api);
  h.activeRuns.set("owner", { runId: "r", num: 1, sessionId: "s", status: "waiting_human", lastNudge: Date.now(), waitingSince: Date.now() });
  h.pendingQuestions.set("owner", { runId: "r", num: 1 });
  return h;
}

test("status, result, help and menus remain commands while waiting for an answer", async () => {
  const calls = [];
  const h = waiting(async (method, url) => {
    calls.push([method, url]);
    if (url.endsWith("/projects")) return { projects: [{ id: "p", name: "项目" }] };
    if (url.endsWith("/models")) return { models: [{ id: "m", name: "模型" }] };
    return { status: "waiting_human", paused_reason: "选哪个？" };
  });
  h.lastResults.set("owner", { num: 0, text: "旧结果" });
  for (const text of ["状态", "结果", "帮助", "项目", "模型"]) await h.chat(text);
  assert.equal(calls.filter(([method]) => method === "POST").length, 0);
  assert(h.pendingQuestions.has("owner"));
  assert(h.sent.some((text) => text.includes("旧结果")));
});

test("answer failure keeps routing; manual retry succeeds once and explicit command words can be answers", async () => {
  let answers = 0;
  const h = waiting(async (method, url, body) => {
    assert(url.endsWith("/answer"));
    assert.equal(body.text, "状态");
    if (++answers === 1) throw new Error("offline");
    return { ok: true };
  });
  assert.match((await h.chat("回答：状态")).text, /等待记录已保留/);
  assert.equal(answers, 1);
  assert(h.pendingQuestions.has("owner"));
  assert.match((await h.chat("回答：状态")).text, /任务继续/);
  assert.equal(answers, 2);
  assert(!h.pendingQuestions.has("owner"));
});

test("empty or concurrent answers do not consume the question or send duplicates", async () => {
  const reply = deferred();
  let answers = 0;
  const h = waiting(async () => { answers++; return reply.promise; });
  await h.chat("回答：");
  assert.equal(answers, 0);
  const first = h.chat("回答：A");
  await h.chat("回答：B");
  assert.equal(answers, 1);
  reply.resolve({ ok: true });
  await first;
  assert(!h.pendingQuestions.has("owner"));
});

test("supplement targets current run and never starts or queues another task", async () => {
  const calls = [];
  const h = harness(async (...args) => { calls.push(args); return { ok: true }; });
  h.activeRuns.set("owner", { runId: "r", num: 8, sessionId: "s" });
  assert.match((await h.chat("补充：颜色改蓝色")).text, /任务 #8/);
  assert.deepEqual(calls.map(([method, url, body]) => [method, url, body.text]), [["POST", "/channel/run/r/supplement", "颜色改蓝色"]]);
  assert.equal(h.taskQueues.size, 0);
  await h.chat("补充：");
  assert.equal(calls.length, 1);
});

test("concurrent task messages dispatch once and preserve the second queue item", async () => {
  const response = deferred();
  let submissions = 0;
  const h = harness(async (method, url) => {
    assert.equal(url, "/channel/task");
    submissions++;
    return response.promise;
  });
  const first = h.chat("first");
  await flush();
  assert(h.snapshot().runtime.taskQueues[0][1][0].dispatching);
  await h.chat("second");
  response.resolve({ run_id: "r" });
  await first;
  assert.equal(submissions, 1);
  assert.equal(h.taskQueues.get("owner").length, 1);
  assert.equal(h.taskQueues.get("owner")[0].text, "second");
  assert.equal(h.snapshot().runtime.activeRuns[0][1].sessionId, "s");
});

test("in-flight conflict never rebuilds session and dispatches the same task elsewhere", async () => {
  let submissions = 0;
  const h = harness(async (method, url) => {
    assert.equal(url, "/channel/task");
    submissions++;
    throw Object.assign(new Error("该会话已有在途任务"), { status: 409 });
  });
  assert.match((await h.chat("task")).text, /派发未确认/);
  assert.equal(submissions, 1);
  assert(h.queuePauses.has("owner"));
  assert(h.taskQueues.get("owner")[0].dispatching);
});

test("restart restores a waiting run, its answer route, queue and previous result without reposting", async () => {
  const before = waiting();
  before.lastResults.set("owner", { num: 0, text: "保留的完整结果" });
  before.taskQueues.set("owner", [{ id: "q", text: "next" }]);
  before.context.saveState();
  const calls = [];
  const after = harness(async (method, url) => {
    calls.push([method, url]);
    return { status: "waiting_human", paused_reason: "选哪个？" };
  }, before.snapshot());
  after.restart();
  await after.callbacks[0]();
  assert.equal(after.pendingQuestions.get("owner").runId, "r");
  assert.equal(after.taskQueues.get("owner")[0].text, "next");
  assert.equal(after.lastResults.get("owner").text, "保留的完整结果");
  assert(calls.every(([method]) => method === "GET"));
  assert.equal(after.sent.length, 0);
});

test("restart recovers completed durable trace by exact run and caches full result once", async () => {
  const before = harness(async () => ({}));
  before.watchRun("owner", "r", 7, { sessionId: "s" });
  const calls = [];
  const after = harness(async (method, url) => {
    calls.push([method, url]);
    if (url === "/channel/run/r") throw Object.assign(new Error("missing"), { status: 404 });
    if (url === "/channel/trace/r") return { trace: { id: "r", session_id: "s", status: "succeeded", completion: { summary: "摘要", details: "正文".repeat(9000) } } };
    return { artifacts: [] };
  }, before.snapshot());
  after.restart();
  await after.callbacks[0]();
  assert.equal(after.sent.length, 1);
  assert.equal(after.lastResults.get("owner").text, "摘要\n\n" + "正文".repeat(9000));
  const again = harness(async () => { throw new Error("must not poll or repost"); }, after.snapshot());
  again.restart();
  assert.equal(again.callbacks.length, 0);
  assert.equal(again.sent.length, 0);
});

test("unknown run pauses and preserves queue until explicitly continued", async () => {
  const h = harness(async (method, url) => {
    if (url === "/channel/run/r") throw Object.assign(new Error("missing"), { status: 404 });
    if (url === "/channel/trace/r") return { trace: { id: "r", session_id: "s", status: "running" } };
    if (url === "/channel/task") return { run_id: "next" };
    throw new Error(url);
  });
  h.taskQueues.set("owner", [{ id: "q", text: "next task" }]);
  h.watchRun("owner", "r", 1, { sessionId: "s" });
  await h.callbacks[0]();
  assert(h.queuePauses.has("owner"));
  assert.equal(h.taskQueues.get("owner").length, 1);
  assert.equal(h.callbacks.length, 1);
  await h.chat("继续队列");
  assert.equal(h.activeRuns.get("owner").runId, "next");
  assert(!h.taskQueues.has("owner"));
});

test("pending dispatch on restart is not automatically repeated; later items survive skipping", async () => {
  const h = harness(async () => { throw new Error("must not dispatch"); });
  h.taskQueues.set("owner", [{ id: "a", text: "maybe started", dispatching: true }, { id: "b", text: "next" }]);
  h.context.saveState();
  const restored = harness(async () => { throw new Error("must not dispatch"); }, h.snapshot());
  restored.restart();
  await flush();
  assert(restored.queuePauses.has("owner"));
  await restored.chat("继续队列");
  assert.equal(restored.taskQueues.get("owner").length, 2);
  await restored.chat("跳过队首");
  assert.equal(restored.taskQueues.get("owner")[0].text, "next");
});

test("binding changes cannot move active or queued tasks into a different project", async () => {
  const h = waiting(async () => ({ projects: [{ id: "p", name: "other" }] }));
  await h.chat("项目");
  for (const command of ["1", "解绑", "新会话"]) assert.match((await h.chat(command)).text, /当前有/);
  assert.equal(h.context.state.senders.owner.session_id, "s");
  assert(h.pendingQuestions.has("owner"));
});

test("durable lookup never substitutes another session and never treats offline as missing", async () => {
  const entry = { runId: "r", sessionId: "s" };
  const mismatch = await readRun(async (method, url) => {
    if (url.includes("/run/")) throw Object.assign(new Error("missing"), { status: 404 });
    return { trace: { id: "r", session_id: "other", status: "succeeded" } };
  }, entry);
  assert.equal(mismatch.status, "unknown");
  let requests = 0;
  await assert.rejects(readRun(async () => { requests++; throw new Error("offline"); }, entry), /offline/);
  assert.equal(requests, 1);
});

test("atomic state file preserves bindings and all runtime maps, and loads legacy bindings", () => {
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), "wechat-state-"));
  try {
    const file = path.join(folder, "bindings.json");
    const runtime = restoreRuntime();
    runtime.lastResults.set("owner", { num: 1, text: "全文".repeat(10000) });
    runtime.taskQueues.set("owner", [{ id: "q", text: "task", media: { filePath: "/tmp/file.pdf" } }]);
    saveBindings(file, { owner: "owner", task_counter: 1 }, runtime);
    const state = JSON.parse(fs.readFileSync(file, "utf8"));
    assert.equal(state.owner, "owner");
    assert.deepEqual(snapshotRuntime(restoreRuntime(state.runtime)), snapshotRuntime(runtime));
    assert(!fs.existsSync(`${file}.tmp`));
    assert.equal(restoreRuntime().activeRuns.size, 0);
    assert.equal(restoreRuntime(null).taskQueues.size, 0);
  } finally { fs.rmSync(folder, { recursive: true, force: true }); }
});

test("restart during completion delivery preserves result without automatically replaying it", async () => {
  const delivery = deferred();
  const h = harness(async (method, url) => url.includes("artifacts")
    ? { artifacts: [] }
    : { status: "done", result: "完整正文", session_id: "s" });
  h.context.botSend = async (text) => { h.sent.push(text); await delivery.promise; return true; };
  h.watchRun("owner", "r", 1, { sessionId: "s" });
  const tick = h.callbacks[0]();
  await flush();
  assert.equal(h.sent.length, 1);
  const after = harness(async () => { throw new Error("must not poll or repost"); }, h.snapshot());
  after.restart();
  assert.equal(after.callbacks.length, 0);
  assert.equal(after.sent.length, 0);
  await after.chat("结果");
  assert(after.sent[0].includes("完整正文"));
  delivery.resolve();
  await tick;
});

test("saved running milestones are not pushed again after restart", async () => {
  const api = async () => ({ status: "running", phase: "环境就绪", event_count: 15, last_progress: "继续处理" });
  const h = harness(api);
  h.watchRun("owner", "r", 1, { sessionId: "s" });
  await h.callbacks[0]();
  assert.equal(h.sent.length, 2);
  const after = harness(api, h.snapshot());
  after.restart();
  await after.callbacks[0]();
  assert.equal(after.sent.length, 0);
});

test("a delayed waiting snapshot cannot restore a question that has already been answered", async () => {
  const snapshot = deferred();
  const h = waiting(async (method) => method === "GET" ? snapshot.promise : { ok: true });
  h.watchRun("owner", "r", 1, h.activeRuns.get("owner"));
  const tick = h.callbacks[0]();
  await h.chat("回答：A");
  snapshot.resolve({ status: "waiting_human", paused_reason: "旧问题" });
  await tick;
  assert(!h.pendingQuestions.has("owner"));
  assert.equal(h.activeRuns.get("owner").status, "running");
  assert.equal(h.sent.length, 0);
});

test("received files are registered before dispatch, and registration failure keeps the queued input", async () => {
  const calls = [];
  const h = harness(async (method, url, body) => {
    calls.push({ url, body });
    if (url.endsWith("/attachment")) return { resource_id: "resource-file" };
    return { run_id: "file-task" };
  });
  await h.chat("整理这个文件", { type: "file", filePath: "/tmp/报告.txt", fileName: "报告.txt" });
  assert.equal(calls[0].url, "/channel/session/s/attachment");
  assert.equal(calls[0].body.original_name, "报告.txt");
  assert(calls[1].body.goal.includes("resource_id=resource-file"));
  const failed = harness(async (method, url) => {
    assert(url.endsWith("/attachment"));
    throw new Error("文件已不存在");
  });
  assert.match((await failed.chat("整理文件", { filePath: "/tmp/missing", fileName: "丢失.txt" })).text, /附件.*接收失败/);
  assert.equal(failed.activeRuns.size, 0);
  assert.equal(failed.taskQueues.get("owner")[0].dispatching, undefined);
});

test("completion sends all current-run small files, excludes old files and can resend after restart", async () => {
  const files = [
    { run_id: "r", path: "/tmp/表.xlsx", display_name: "表.xlsx", size_bytes: 12 },
    { run_id: "r", path: "/tmp/图片.png", display_name: "图片.png", size_bytes: 20, mime_type: "image/png" },
    { run_id: "old", path: "/tmp/old.txt", display_name: "old.txt", size_bytes: 5 },
  ];
  const api = async (method, url) => {
    if (url.includes("/artifacts?")) {
      assert(url.endsWith("run_id=r"));
      return { artifacts: files };
    }
    return { status: "done", session_id: "s", result: "已完成" };
  };
  const h = harness(api);
  h.watchRun("owner", "r", 4, { sessionId: "s" });
  await h.callbacks[0]();
  assert.equal(h.sent.length, 1);
  assert.equal(h.mediaSent.length, 2);
  assert.deepEqual(h.mediaSent.map((file) => file.media.url), ["/tmp/表.xlsx", "/tmp/图片.png"]);
  assert.equal(h.mediaSent[1].media.type, "image");
  const after = harness(api, h.snapshot());
  assert.equal((await after.chat("文件")).text, "");
  assert.equal(after.mediaSent.length, 2);
  assert.equal(after.taskQueues.size, 0);
});

test("oversize files and truncated file listings are disclosed; file command works while waiting", async () => {
  const h = waiting(async () => ({ artifacts: [
    { run_id: "r", path: "/tmp/big.zip", display_name: "big.zip", size_bytes: 26 * 1024 * 1024 },
  ], has_more: true }));
  h.lastResults.set("owner", { num: 1, runId: "r", sessionId: "s" });
  await h.chat("文件");
  assert.equal(h.mediaSent.length, 0);
  assert(h.sent.some((text) => text.includes("超过 25MB")));
  assert(h.sent.some((text) => text.includes("前 5 个")));
  assert(h.pendingQuestions.has("owner"));
});


test("media without a filename retains the downloaded file extension during registration", async () => {
  let name;
  const h = harness(async (method, url, body) => {
    if (url.endsWith("/attachment")) { name = body.original_name; return { resource_id: "image" }; }
    return { run_id: "image-task" };
  });
  await h.chat("看看这张图", { type: "image", filePath: "/tmp/download-123.png", mimeType: "image/png" });
  assert.equal(name, "download-123.png");
});
