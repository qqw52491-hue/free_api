#!/usr/bin/env node
/**
 * case_lint —— 考题腐烂静态检查：
 * 判据(verify_command/test_patch)引用的仓库文件必须在其 base_commit 存在。
 * 模块搬家后判据没跟上=恒红假失败（实测 window_read/seq_ledger 两例）。
 * 纯机械检查：不评估题面语义，只对账「引用的路径在基线上存不存在」。
 * 用法: node scripts/case_lint.mjs [--case <id>]
 */
import { execSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.cwd();
const CASE_DIR = join(ROOT, "test/eval_cases");
const ONLY = process.argv.includes("--case")
  ? process.argv[process.argv.indexOf("--case") + 1]
  : null;

// 判据文本里的仓库路径候选（按常见前缀过滤，排除 URL/变量/临时路径）
const PATH_RE = /(?<![\w$/-])((?:src-tauri|src|test|office_materials|scripts|docs)\/[\w./-]+\.(?:rs|py|mjs|js|ts|tsx|vue|json|toml|md|sh|sql|csv|xlsx|ini|yaml|yml|txt|conf))/g;
// cd X && 后的相对路径需要相对 X 解析
const CD_RE = /cd\s+([\w./-]+)/g;

function fileExistsAt(commit, p) {
  try {
    execSync(`git cat-file -e ${commit}:${p}`, { cwd: ROOT, stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}

function extractPaths(text) {
  const found = new Map(); // path -> 来源标记
  if (!text) return found;
  const cdDirs = [...text.matchAll(CD_RE)].map((m) => m[1].replace(/\/$/, ""));
  for (const m of text.matchAll(PATH_RE)) {
    found.set(m[1], "abs");
  }
  // cd 后的裸文件名引用（如 plan_gate.rs）相对 cd 目录解析
  for (const dir of cdDirs) {
    const afterCd = text.slice(text.indexOf(`cd ${dir}`));
    for (const m of afterCd.matchAll(/(?<![\w$/-])([\w-]+\.(?:rs|py|mjs|js|toml|sh))/g)) {
      found.set(`${dir}/${m[1]}`, `cd ${dir}`);
    }
  }
  return found;
}

let bad = 0;
let checked = 0;
for (const f of readdirSync(CASE_DIR).filter((f) => f.endsWith(".json"))) {
  const def = JSON.parse(readFileSync(join(CASE_DIR, f), "utf8"));
  if (ONLY && def.id !== ONLY && !f.includes(ONLY)) continue;
  checked++;
  const commit = def.base_commit;
  if (!commit) {
    console.log(`⚠️  ${def.id}: 缺 base_commit`);
    continue;
  }
  // test_patch 在判分时注入的文件——基线上不存在是正确设计
  const patchCreated = new Set();
  if (def.test_patch) {
    try {
      const patchText = readFileSync(join(ROOT, def.test_patch), "utf8");
      for (const m of patchText.matchAll(/^\+\+\+ b\/(.+)$/gm)) patchCreated.add(m[1]);
    } catch {}
  }
  const refs = new Map();
  for (const [p, src] of extractPaths(def.verify_command)) refs.set(p, `verify:${src}`);
  // expected_diff_files 是「期望模型改动」的声明——新建型任务的产物
  // 基线本就不存在，不参与存在性对账
  // verify 里带「test -f / [ -f <path>」存在性断言的路径是产物断言：
  // 任务本来就要求模型新建它，基线缺失是设计而非腐烂。
  const creationAsserted = new Set();
  for (const m of (def.verify_command || "").matchAll(/(?:test|\[)\s+-f\s+([\w./-]+)/g)) {
    creationAsserted.add(m[1]);
  }
  // ${VAR:-default}} 尾巴多花括号笔误：shell 展开在首个 } 闭合、尾部 } 成字面量，
  // DB 路径恒不存在 → 判据 SQL 全部落空恒红（实测 8 道 hd 题全中招）。
  if (/\$\{[^}]*:-[^}]*\}\}/.test(def.verify_command || "")) {
    bad++;
    console.log(`🔴 ${def.id}: verify_command 含 \${VAR:-default}} 双花括号笔误——回退路径恒被命中，判据结构性恒红`);
  }
  const missing = [];
  const artifactSuspects = [];
  for (const [p, src] of refs) {
    if (patchCreated.has(p)) continue; // 判分注入物
    if (creationAsserted.has(p)) continue; // 产物断言
    if (!fileExistsAt(commit, p)) {
      // 代码/测试路径缺失=真腐烂；docs/tmp/fixtures 等可能是产物断言（任务要求新建）降级为提示
      if (/^(src-tauri|src|scripts)\//.test(p)) missing.push(`${p} (${src})`);
      else artifactSuspects.push(`${p} (${src})`);
    }
  }
  if (missing.length) {
    bad++;
    console.log(`🔴 ${def.id} @${commit.slice(0, 8)}: ${missing.length} 个代码引用在基线不存在:`);
    for (const m of missing) console.log(`     ${m}`);
  }
  if (artifactSuspects.length) {
    console.log(`🟡 ${def.id}: ${artifactSuspects.length} 个非代码路径基线缺失（可能是产物断言，人工确认）:`);
    for (const m of artifactSuspects) console.log(`     ${m}`);
  }
}
console.log(`\n${checked} 题检查完毕，${bad} 题引用腐烂${bad ? "（判据需随代码搬家更新）" : ""}`);
process.exit(bad ? 1 : 0);
