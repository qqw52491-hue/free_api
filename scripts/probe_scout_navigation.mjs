#!/usr/bin/env node
// 第二层探针：测 flash 档模型"自己导航找代码"的能力（真实侦察兵形态）。
// 不给文件——只给症状描述 + 两个工具（search=rg，read=行段），
// 看它能走几步、最后报告的事实对不对、全不全。
// 用法: node scripts/probe_scout_navigation.mjs <model_db_id> <db_path> <repo_root>

import { execSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";

const [modelId, dbPath, repoRoot] = process.argv.slice(2);
const row = execSync(
  `sqlite3 "${dbPath}" "SELECT m.name, p.base_url, p.api_key FROM models m JOIN platforms p ON m.platform_id=p.id WHERE m.id='${modelId}'"`,
  { encoding: "utf8" },
).trim();
const [model, baseUrl, apiKey] = row.split("|");

const QUESTION = `用户报告：聊天输入框里"粘贴截图"或"拖拽图片"发送后，消息中的图片显示为破损占位图、无法预览，历史消息重载后同样失效。

请定位：图片附件在加入输入框附件列表时，file_path 字段是在哪些代码位置赋值的？要求：给出文件、行号区间、每处 file_path 的赋值原文，并指出哪些赋值会导致图片无法落盘（即赋的是裸文件名而非真实磁盘路径）。`;

const MAX_STEPS = 15;

const SYSTEM = `你是一个代码侦察兵。仓库根目录: ${repoRoot}（前端在 src/，后端在 src-tauri/src/）。

每一步只回复一个 JSON 对象，不要输出任何其他文字：
{"tool":"search","query":"<ripgrep 正则>","scope":"src"}           — 在目录里搜索，返回 文件:行号:行内容
{"tool":"read","path":"<相对路径>","start":<行号>,"end":<行号>}    — 读文件行段，单次最多 250 行
{"answer":"<最终报告>"}                                          — 证据足够时给结论

规则：报告必须引用 文件:行号 + 赋值原文；找不到就继续搜，最多 ${MAX_STEPS} 步。`;

function runTool(req) {
  try {
    if (req.tool === "search") {
      const scope = ["src", "src-tauri/src"].includes(req.scope) ? req.scope : "src";
      const out = execSync(
        `rg -n --no-heading -i --max-count 30 ${JSON.stringify(String(req.query ?? ""))} ${scope}/`,
        { cwd: repoRoot, encoding: "utf8", timeout: 15000 },
      );
      return out.slice(0, 6000) || "(无命中)";
    }
    if (req.tool === "read") {
      const p = resolve(repoRoot, String(req.path ?? ""));
      if (!p.startsWith(repoRoot) || !existsSync(p)) return "(路径无效)";
      const lines = readFileSync(p, "utf8").split("\n");
      const s = Math.max(1, req.start | 0);
      const e = Math.min(lines.length, Math.max(s, Math.min(s + 249, req.end | 0)));
      return lines
        .slice(s - 1, e)
        .map((l, i) => `${s + i}\t${l}`)
        .join("\n");
    }
    return "(未知工具)";
  } catch (e) {
    return `(工具失败: ${String(e.message || e).slice(0, 200)})`;
  }
}

async function chat(messages) {
  const res = await fetch(`${baseUrl.replace(/\/$/, "")}/chat/completions`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${apiKey}` },
    body: JSON.stringify({ model, messages, temperature: 0 }),
  });
  const data = await res.json();
  if (!res.ok) throw new Error(`HTTP ${res.status}: ${JSON.stringify(data).slice(0, 300)}`);
  return { content: data.choices?.[0]?.message?.content ?? "", usage: data.usage };
}

const messages = [
  { role: "system", content: SYSTEM },
  { role: "user", content: QUESTION },
];

let totalPrompt = 0;
for (let step = 0; step < MAX_STEPS; step++) {
  const { content, usage } = await chat(messages);
  totalPrompt += usage?.prompt_tokens ?? 0;
  let req;
  try {
    req = JSON.parse(content.replace(/```json|```/g, "").trim());
  } catch {
    messages.push({ role: "assistant", content });
    messages.push({ role: "user", content: "格式错误：请只回复一个 JSON 对象。" });
    continue;
  }
  if (req.answer) {
    console.log(`\n===== 最终报告（${step + 1} 步, 累计 prompt=${totalPrompt}）=====`);
    console.log(req.answer);
    process.exit(0);
  }
  const result = runTool(req);
  console.log(`[${step + 1}] ${req.tool} ${req.query ?? req.path ?? ""}${req.start ? `:${req.start}-${req.end}` : ""} → ${result.split("\n").length} 行`);
  messages.push({ role: "assistant", content });
  messages.push({ role: "user", content: result });
}
console.log(`\n===== ${MAX_STEPS} 步内未给出报告（累计 prompt=${totalPrompt}）=====`);
