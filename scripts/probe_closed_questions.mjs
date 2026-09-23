#!/usr/bin/env node
// 封闭问题离线探针：测 flash 档模型对"枚举代码事实"的准确率。
// 用途：决定"flash 子 agent 扇出取证"这条路是否值得建——它先要学会数数。
// 用法: node scripts/probe_closed_questions.mjs <model_db_id> [db_path]
// API key 从模型表 JOIN platform 提取，只走环境，不进输出。

import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const modelId = process.argv[2];
const dbPath = process.argv[3] || resolve(process.cwd(), "src-tauri/app_data.db");
const filePath = process.argv[4] || "/tmp/composer_baseline.vue";

const row = execSync(
  `sqlite3 "${dbPath}" "SELECT m.name, p.base_url, p.api_key FROM models m JOIN platforms p ON m.platform_id=p.id WHERE m.id='${modelId}'"`,
  { encoding: "utf8" },
).trim();
const [model, baseUrl, apiKey] = row.split("|");
if (!model || !baseUrl) {
  console.error("模型/platform 查询失败:", row);
  process.exit(1);
}

const source = readFileSync(filePath, "utf8");
const numbered = source
  .split("\n")
  .map((l, i) => `${i + 1}\t${l}`)
  .join("\n");

const questions = [
  `Q1（枚举）: 列出文件中所有 attachments.value.push(...) 的调用点。对每个调用点给出：行号、该对象字面量里 file_path 字段的赋值原文。`,
  `Q2（分支）: 在 handleFileDrop 中，file.type.startsWith("image/") 这个分支内部有没有调用 save_and_parse_uploaded_attachment？该 invoke 出现在哪个分支？sessionId 参数传的是什么值？`,
  `Q3（对照）: 图片入队的三个路径（拖拽 image/* 分支、粘贴 imageFiles 分支、原生剪贴板 data_urls 回退分支）各自给 file_path 赋的是什么值？`,
];

const prompt = `下面是文件 ${filePath} 的完整内容（带行号）。请只回答事实，不给建议、不改代码、不做推测。

${questions.join("\n\n")}

===== 文件内容 =====
${numbered}`;

const res = await fetch(`${baseUrl.replace(/\/$/, "")}/chat/completions`, {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    Authorization: `Bearer ${apiKey}`,
  },
  body: JSON.stringify({
    model,
    messages: [{ role: "user", content: prompt }],
    temperature: 0,
  }),
});

const text = await res.text();
if (!res.ok) {
  console.error(`HTTP ${res.status}:`, text.slice(0, 500));
  process.exit(1);
}
const data = JSON.parse(text);
const answer = data.choices?.[0]?.message?.content ?? "(empty)";
console.log(`===== ${model} 回答 =====`);
console.log(answer);
console.log(`===== usage: prompt=${data.usage?.prompt_tokens} completion=${data.usage?.completion_tokens} cached=${data.usage?.prompt_tokens_details?.cached_tokens ?? "n/a"} =====`);
