// 判分 oracle 驱动的 progression 测试：纯客户端管道，题目/输入/期望输出全部由宿主下发。
// 本文件不含任何答案；每次 npm test：GET /current 取题 → 执行 transform → POST /verify 送验 → 解锁。
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { port, nonce } = JSON.parse(readFileSync(path.join(root, '.verify.json'), 'utf8'));
const base = `http://127.0.0.1:${port}`;

// 哨兵编解码：JSON 无法表达 Infinity/NaN，与宿主约定字符串哨兵
const encodeReplacer = (k, x) =>
  x === Infinity ? "__Infinity__"
  : x === -Infinity ? "__-Infinity__"
  : typeof x === "number" && Number.isNaN(x) ? "__NaN__"
  : x;
const revive = (k, x) =>
  x === "__Infinity__" ? Infinity
  : x === "__-Infinity__" ? -Infinity
  : x === "__NaN__" ? NaN
  : x;
const get = async (p) => JSON.parse(await (await fetch(base + p)).text(), revive);
const post = async (p, body) => {
  const r = await fetch(base + p, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body, encodeReplacer),
  });
  return JSON.parse(await r.text(), revive);
};

const cur = await get(`/current?nonce=${nonce}&v=${Date.now()}`);
const stageFile = path.join(root, "src", `stage-${cur.id}.mjs`);
if (!existsSync(stageFile)) {
  writeFileSync(stageFile, "export function transform(value) {\n  return value;\n}\n");
}
writeFileSync(
  path.join(root, "CURRENT_STAGE.md"),
  `# 当前阶段 ${cur.id}/${cur.total}\n\n仅修改 src/stage-${cur.id}.mjs 的 transform。${cur.requirement}\n\n运行 npm test。不要修改 README、package.json、CURRENT_STAGE.md、test/ 或 .verify.json。\n`,
);

const mod = await import(pathToFileURL(stageFile).href + "?v=" + Date.now());
const before = structuredClone(cur.input);
const output = mod.transform(cur.input);
assert.deepEqual(cur.input, before, "transform 不得修改输入");

const verdict = await post("/verify", { nonce, output });
if (!verdict.pass) {
  assert.fail("阶段 " + cur.id + " 未通过宿主判分：" + (verdict.hint || ""));
}
if (verdict.done) {
  console.log("全部 " + cur.total + " 个阶段已通过宿主判分，任务收敛。");
} else {
  const nextFile = path.join(root, "src", `stage-${verdict.next.id}.mjs`);
  if (!existsSync(nextFile)) {
    writeFileSync(nextFile, "export function transform(value) {\n  return value;\n}\n");
  }
  writeFileSync(
    path.join(root, "CURRENT_STAGE.md"),
    `# 当前阶段 ${verdict.next.id}/${cur.total}\n\n仅修改 src/stage-${verdict.next.id}.mjs 的 transform。${verdict.next.requirement}\n\n运行 npm test。不要修改 README、package.json、CURRENT_STAGE.md、test/ 或 .verify.json。\n`,
  );
  console.log("阶段 " + cur.id + " 已通过宿主判分，解锁阶段 " + verdict.next.id);
}
