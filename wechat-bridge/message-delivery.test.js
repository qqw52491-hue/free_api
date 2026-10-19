import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import vm from "node:vm";
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
