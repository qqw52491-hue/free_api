import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import vm from "node:vm";
import { readRun } from "./runtime-state.js";
import { splitWechatText, createMessageDelivery, MAX_MESSAGE_LENGTH } from "./message-delivery.js";

const unwrap = (parts) => parts.map((part) => part.replace(/^（\d+\/\d+）\n/, "")).join("");

test("short notifications preserve full text beyond the former 300/1200 limits", () => {
  const text = "🏁 任务已完成：\n\n" + "正文🙂".repeat(500);
  assert.deepEqual(splitWechatText(text), [text]);
  assert.deepEqual(splitWechatText(""), []);
});

test("long text round-trips including whitespace, markdown, Chinese and emoji", () => {
  const text = ("### 标题\n\n" + "结果🙂".repeat(1100) + "\n  - 保留缩进\n\n").repeat(8);
  const parts = splitWechatText(text);
  assert(parts.length > 2);
  assert.equal(unwrap(parts), text);
  for (const [index, part] of parts.entries()) {
    assert(part.startsWith(`（${index + 1}/${parts.length}）\n`));
    assert(part.length <= MAX_MESSAGE_LENGTH);
    assert(!/[\ud800-\udbff]$/.test(part));
    assert(!/^（\d+\/\d+）\n[\udc00-\udfff]/.test(part));
  }
  assert(unwrap(parts).length > 24_000);
});

test("paragraph boundaries are preferred without discarding separators", () => {
  const text = "a".repeat(2000) + "\n\n" + "b".repeat(2000);
  const parts = splitWechatText(text);
  assert(parts[0].endsWith("\n\n"));
  assert.equal(unwrap(parts), text);
});

test("concurrent notifications send complete ordered groups", async () => {
  const sent = [];
  const delivery = createMessageDelivery(async (part) => {
    await new Promise((resolve) => setImmediate(resolve));
    sent.push(part);
  });
  const a = "A".repeat(9000);
  const b = "B".repeat(5000);
  await Promise.all([delivery(a), delivery(b)]);
  const aCount = splitWechatText(a).length;
  assert.equal(unwrap(sent.slice(0, aCount)), a);
  assert.equal(unwrap(sent.slice(aCount)), b);
});

test("failed segment stops remaining sends without retry or poisoning later notifications", async () => {
  const sent = [];
  const delivery = createMessageDelivery(async (part) => {
    sent.push(part);
    if (sent.length === 2) throw new Error("connection lost");
  });
  await assert.rejects(delivery("a".repeat(12000)), /connection lost/);
  assert.equal(sent.length, 2);
  await delivery("next notification");
  assert.equal(sent[2], "next notification");
});

test("media is attached once, with complete remaining text; failure preserves text fallback", async () => {
  const text = "长正文".repeat(4000);
  const media = { type: "file", url: "/tmp/fixture.txt" };
  const sent = [];
  await createMessageDelivery(async (message) => { sent.push(message); })(text, media);
  assert.equal(sent[0].media, media);
  assert(sent.slice(1).every((message) => typeof message === "string"));
  assert.equal(unwrap(sent.map((message) => typeof message === "string" ? message : message.text)), text);
  const fallback = [];
  await createMessageDelivery(async (message) => {
    if (typeof message !== "string") throw new Error("upload failed");
    fallback.push(message);
  })(text, media);
  assert.equal(unwrap(fallback), text);
});

test("actual result command routes full replies through multipart delivery without an extra SDK reply", async () => {
  const source = fs.readFileSync(new URL("./index.js", import.meta.url), "utf8");
  const routeSource = source.slice(source.indexOf("const agent = {"), source.indexOf("// ---------------------------------------------------------------- 桥 HTTP 服务"));
  const body = "完整结果🙂".repeat(6000);
  const sent = [];
  const delivery = createMessageDelivery(async (part) => { sent.push(part); });
  const context = {
    state: { owner: "owner" }, config: {}, senderOf: () => ({ seen: true }),
    pendingQuestions: new Map(), lastResults: new Map([["owner", { num: 7, text: body }]]),
    botSend: async (text) => { await delivery(text); return true; }
  };
  const agent = vm.runInNewContext(`${routeSource}\nagent`, context);
  const response = await agent.chat({ conversationId: "owner", text: "结果" });
  assert.equal(response.text, "");
  assert.equal(unwrap(sent), `📄 任务 #7 的结果：\n\n${body}`);
});

function watcherHarness(api, send = async () => {}) {
  const source = fs.readFileSync(new URL("./index.js", import.meta.url), "utf8");
  const watchSource = source.slice(source.indexOf("function watchRun("), source.indexOf("async function latestArtifact("));
  const callbacks = [];
  const activeRuns = new Map();
  const lastResults = new Map();
  const sent = [];
  let drained = 0;
  const context = {
    activeRuns, lastResults, pendingQuestions: new Map(), api, readRun,
    bot: {}, taskQueues: new Map(), queuePauses: new Map(), lastSelections: new Map(), saveState() {},
    POLL_INTERVAL_MS: 10_000, NUDGE_MS: 7_200_000, HEARTBEAT_MS: 1_800_000,
    MAX_ARTIFACT_BYTES: 25 * 1024 * 1024,
    setInterval: (callback) => { callbacks.push(callback); return callbacks.length; },
    clearInterval: () => {}, console: { log() {}, error() {} },
    inQuietHours: () => false, latestArtifact: async () => null,
    botSend: async (text) => { sent.push(text); await send(text); return true; },
    drainQueue: async () => { drained++; },
  };
  const watch = vm.runInNewContext(`${watchSource}\nwatchRun`, context);
  return { watch, callbacks, activeRuns, lastResults, sent, drained: () => drained };
}

const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

test("bridge claims terminal notifications on both original and rebuilt-session submissions", async () => {
  const source = fs.readFileSync(new URL("./index.js", import.meta.url), "utf8");
  const submitSource = source.slice(source.indexOf("async function submitTask("), source.indexOf("/** 先保存派发中的队首"));
  for (const rebuild of [false, true]) {
    const submissions = [];
    const watched = [];
    const sender = { session_id: "original" };
    const submit = vm.runInNewContext(`${submitSource}\nsubmitTask`, {
      senderOf: () => sender, saveState() {}, state: {}, console: { log() {} },
      api: async (method, url, body) => {
        if (url === "/channel/session") return { session_id: "rebuilt" };
        submissions.push(body);
        if (rebuild && submissions.length === 1) throw new Error("session expired");
        return { run_id: "run", llm_retries: 3 };
      },
      watchRun: (...args) => watched.push(args),
    });
    await submit("owner", "task");
    assert.equal(submissions.length, rebuild ? 2 : 1);
    assert(submissions.every((body) => body.caller_notifies === true));
    assert.equal(watched.length, 1);
    assert.equal(submissions.at(-1).session_id, rebuild ? "rebuilt" : "original");
  }
});

test("slow polling and long sends produce one completion and one queue drain", async () => {
  const response = deferred();
  const delivery = deferred();
  let requests = 0;
  const h = watcherHarness(async () => { requests++; return response.promise; }, () => delivery.promise);
  h.watch("owner", "run", 7);
  const first = h.callbacks[0]();
  await h.callbacks[0](); // another interval while HTTP is pending
  assert.equal(requests, 1);
  response.resolve({ status: "done", result: "完整结果", session_id: "s" });
  await new Promise((resolve) => setImmediate(resolve));
  await h.callbacks[0](); // another interval while sending is pending
  assert.equal(h.sent.length, 1);
  delivery.resolve();
  await first;
  await h.callbacks[0](); // already removed terminal run
  assert.equal(requests, 1);
  assert.equal(h.sent.length, 1);
  assert.equal(h.lastResults.get("owner").text, "完整结果");
  assert.equal(h.drained(), 1);
});

test("failed and cancelled runs also produce one terminal notification", async () => {
  for (const status of ["failed", "cancelled"]) {
    const response = deferred();
    let requests = 0;
    const h = watcherHarness(async () => { requests++; return response.promise; });
    h.watch("owner", "run", 8);
    const first = h.callbacks[0]();
    await h.callbacks[0]();
    response.resolve({ status, error: "failure" });
    await first;
    assert.equal(requests, 1);
    assert.equal(h.sent.length, 1);
    assert.equal(h.drained(), 1);
  }
});

test("late success or failure from an old watcher cannot remove its replacement", async () => {
  for (const reject of [false, true]) {
    const response = deferred();
    const h = watcherHarness(() => response.promise);
    h.watch("owner", "old", 1);
    const pending = h.callbacks[0]();
    h.watch("owner", "new", 2);
    if (reject) response.reject(new Error("任务不存在"));
    else response.resolve({ status: "done", result: "old" });
    await pending;
    assert.equal(h.activeRuns.get("owner").runId, "new");
    assert.equal(h.sent.length, 0);
    assert.equal(h.drained(), 0);
  }
});

test("poll errors release the busy flag so a later poll can complete", async () => {
  let requests = 0;
  const h = watcherHarness(async () => {
    if (++requests === 1) throw new Error("temporarily offline");
    return { status: "done", result: "recovered" };
  });
  h.watch("owner", "run", 9);
  await h.callbacks[0]();
  assert.equal(h.sent.length, 0);
  await h.callbacks[0]();
  assert.equal(requests, 2);
  assert.equal(h.sent.length, 1);
  assert.equal(h.drained(), 1);
});
