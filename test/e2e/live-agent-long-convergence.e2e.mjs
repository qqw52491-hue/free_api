import assert from "node:assert/strict";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const artifactDir = path.join(rootDir, "test-artifacts");
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const maxDurationMinutes = Number(process.env.FREE_API_LIVE_MAX_DURATION_MINUTES ?? 20);
// 轮询期限必须严格大于运行时限（supervise_worker 取消 → 120s 安全等待 → 报告落盘），
// 否则 app 还没写完报告 spec 就先放弃并拆掉实例——2026-08-30 实测两期限同为 3600s 时报告丢失。
const evaluationTimeoutMs = Math.max(
  Number(process.env.FREE_API_LIVE_EVALUATION_TIMEOUT_MS ?? 1_200_000),
  maxDurationMinutes * 60_000 + 300_000
);
const reasoningEffort = process.env.FREE_API_LIVE_REASONING_EFFORT?.trim() || "auto";
const proModelId = process.env.FREE_API_LIVE_MODEL_ID_PRO?.trim() || modelId;
const flashModelId = process.env.FREE_API_LIVE_MODEL_ID_FLASH?.trim() || modelId;
const routingMode = process.env.FREE_API_ROUTING_MODE?.trim() || "";
const roleModelsRaw = process.env.FREE_API_ROLE_MODELS?.trim() || "";
const roleModels = roleModelsRaw ? JSON.parse(roleModelsRaw) : null;
const routingConfig = {
  pro: proModelId,
  flash: flashModelId,
  vision: proModelId,
  reasoning_effort: reasoningEffort,
  ...(routingMode ? { routing_mode: routingMode } : {}),
  ...(roleModels ? { role_models: roleModels } : {}),
};
const enabled = process.env.FREE_API_LIVE_LONG_CONVERGENCE === "1";
const planMode = process.env.FREE_API_EVALUATION_PLAN_MODE?.trim() || null;
// 外部隔离工作区（FREE_API_LIVE_FIXTURE_DIR）：避免写入项目目录触发热重载
const fixtureDir = process.env.FREE_API_LIVE_FIXTURE_DIR?.trim() || path.join(artifactDir, `agent-long-convergence-${Date.now()}-${process.pid}`);
const protectedFiles = new Map();
const describeLong = modelId && enabled ? describe : describe.skip;

const stages = [
  ["01", "把字符串去除首尾空白并转为大写。", "  alpha  ", "ALPHA"],
  ["02", "把字符串去除首尾空白并转为小写。", "  BETA  ", "beta"],
  ["03", "仅接受去空白后非空的字符串；其他值返回 null。", "   ", null],
  ["04", "仅接受有限数值，截断为整数并限制在 0 到 10；其他值返回 null。", 17.8, 10],
  ["05", "仅接受有限数值，四舍五入到两位小数；其他值返回 null。", 3.14159, 3.14],
  ["06", "接受字符串数组：去空白、删除空值、保留首次出现并按字母升序返回。", [" beta ", "alpha", "beta", ""], ["alpha", "beta"]],
  ["07", "接受逗号分隔字符串：拆分、去空白、删除空项、去重并按字母升序返回数组。", "z, alpha, z, beta, ", ["alpha", "beta", "z"]],
  ["08", "接受对象数组：只保留具有字符串 name 和有限 score 的项，按 score 降序、name 升序返回新数组。", [{ name: "b", score: 2 }, { name: "a", score: 2 }, { name: "x", score: Infinity }], [{ name: "a", score: 2 }, { name: "b", score: 2 }]],
  ["09", "接受对象，返回新对象并补齐 enabled:false、retries:0；原对象字段优先，输入非对象时当作空对象。", { retries: 2, label: "x" }, { enabled: false, retries: 2, label: "x" }],
  ["10", "接受对象：键名去空白转小写，值保持不变；冲突时后出现的键覆盖；返回新对象。", { " Alpha ": 1, BETA: 2, alpha: 3 }, { alpha: 3, beta: 2 }],
  ["11", "接受数组，只求和有限数值；返回 0 表示无有效值。", [1, Infinity, "2", -3], -2],
  ["12", "接受数组，只平均有限且大于 0 的数值；无有效值返回 0。", [1, -2, 3, Infinity], 2],
  ["13", "接受字符串，生成 slug：去空白、小写、连续非字母数字替换为单个连字符、移除首尾连字符。", "  Hello, World! 2026  ", "hello-world-2026"],
  ["14", "接受对象数组，按非空字符串 id 去重并保留最后出现项；按 id 升序返回新数组。", [{ id: "b", v: 1 }, { id: "a", v: 2 }, { id: "b", v: 3 }], [{ id: "a", v: 2 }, { id: "b", v: 3 }]],
  ["15", "接受两个有限数值组成的数组，返回 {min,max}；任何无效输入返回 null。", [8, -2], { min: -2, max: 8 }],
  ["16", "接受布尔值或字符串 true/false（忽略大小写和空白）；无法解析时返回 null。", " TRUE ", true],
  ["17", "接受字符串，按空白分词，每个词首字母大写其余小写，并以单空格连接。", "hELLo   wORLD", "Hello World"],
  ["18", "接受数组，删除 null 和 undefined，保持其余元素顺序并返回新数组。", [0, null, "", undefined, false, 3], [0, "", false, 3]],
  ["19", "接受数值数组，只保留有限数值并按升序返回新数组。", [3, Infinity, -1, 2], [-1, 2, 3]],
  ["20", "接受字符串，规范化 email：去空白并转小写；不含单个 @ 或任一侧为空时返回 null。", " User@Example.COM ", "user@example.com"],
  ["21", "接受多行字符串：逐行去空白、删除空行、去重并保持首次出现顺序。", " a\n\n b \na\n", ["a", "b"]],
  ["22", "接受字符串数组：按每个去空白后首字符的小写分组，忽略空字符串，返回键按字母升序的对象，组内保留原去空白值。", ["Apple", " ant ", "Beta", ""], { a: ["Apple", "ant"], b: ["Beta"] }],
  ["23", "接受对象数组，只保留有限 value 和字符串 id，按 value 降序、id 升序取前三项，返回新数组。", [{ id: "b", value: 3 }, { id: "a", value: 3 }, { id: "c", value: 1 }, { id: "x", value: Infinity }], [{ id: "a", value: 3 }, { id: "b", value: 3 }, { id: "c", value: 1 }]],
  ["24", "接受对象数组，只保留非空字符串 id 与有限数值 value；按 id 取最高 value，返回 {count, average}，平均值为保留项均值，无项时为 {count:0,average:0}。", [{ id: "a", value: 2 }, { id: "a", value: 4 }, { id: "b", value: 6 }, { id: "x", value: Infinity }], { count: 2, average: 5 }],
];

function stageFile([id]) { return `src/stage-${id}.mjs`; }
function renderStage(index) {
  const [id, requirement] = stages[index];
  return `# 当前阶段 ${id}/${stages.length}\n\n仅修改 \`${stageFile(stages[index])}\` 的 \`transform\`。${requirement}\n\n运行 \`npm test\`。只有测试明确解锁下一阶段后才继续；不要修改 README、package.json、CURRENT_STAGE.md、test/ 或 .progress.json。\n`;
}
function createFixture() {
  const files = new Map([
    ["package.json", `${JSON.stringify({ type: "module", scripts: { test: "node --test" } }, null, 2)}\n`],
    ["README.md", `# Staged convergence fixture\n\n这是 24 阶段真实修复任务。当前唯一可执行义务在 CURRENT_STAGE.md。每次 npm test 成功验证当前阶段后会解锁下一阶段；在全部阶段完成前，npm test 会以非零退出并明确给出下一阶段。只修改 src/ 中当前阶段文件。\n`],
    ["CURRENT_STAGE.md", renderStage(0)],
    ["test/.progress.json", JSON.stringify({ index: 0 })],
    ["test/progression.test.mjs", `import assert from 'node:assert/strict';\nimport { readFileSync, writeFileSync } from 'node:fs';\nimport path from 'node:path';\nimport { fileURLToPath, pathToFileURL } from 'node:url';\nconst root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');\nconst stages = ${JSON.stringify(stages)};\nconst statePath = path.join(root, 'test/.progress.json');\nconst state = JSON.parse(readFileSync(statePath, 'utf8'));\nconst [id, requirement, input, expected] = stages[state.index];\nconst source = path.join(root, 'src', 'stage-' + id + '.mjs');\nconst mod = await import(pathToFileURL(source).href + '?v=' + Date.now());\nconst before = structuredClone(input);\nassert.deepEqual(mod.transform(input), expected, '当前阶段 ' + id + ' 尚未满足：' + requirement);\nassert.deepEqual(input, before, 'transform 不得修改输入');\nif (state.index < stages.length - 1) {\n  const next = state.index + 1;\n  const [nextId, nextRequirement] = stages[next];\n  writeFileSync(statePath, JSON.stringify({ index: next }));\n  writeFileSync(path.join(root, 'CURRENT_STAGE.md'), '# 当前阶段 ' + nextId + '/' + stages.length + '\\n\\n仅修改 src/stage-' + nextId + '.mjs 的 transform。' + nextRequirement + '\\n\\n运行 npm test；不要修改 README、package.json、CURRENT_STAGE.md、test/ 或 .progress.json。\\n');\n  assert.fail('阶段 ' + id + ' 已验证，已解锁阶段 ' + nextId + '；继续读取 CURRENT_STAGE.md 并完成新的义务。');\n}\n`],
  ]);
  for (const stage of stages) files.set(stageFile(stage), "export function transform(value) {\n  return value;\n}\n");
  for (const [relative, content] of files) {
    const file = path.join(fixtureDir, relative);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, content);
    if (["package.json", "README.md", "test/progression.test.mjs"].includes(relative)) {
      protectedFiles.set(file, content);
    }
  }
  // git 化 fixture：shell 批量修改探针依赖 git status 差值，真实项目均有 git
  try {
    const run = (args) => execSync(`git ${args}`, { cwd: fixtureDir, stdio: "ignore" });
    run("init -q");
    run("-c user.email=eval@local -c user.name=eval add -A");
    run('-c user.email=eval@local -c user.name=eval commit -qm init');
  } catch {
    // git 不可用时探针自动退化为按调用计数（阶段审查仍有 todo/文件数兜底）
  }

}
async function assertHiddenAssertions() {
  for (const [file, original] of protectedFiles) assert.equal(readFileSync(file, 'utf8'), original, `受保护文件被改写：${path.relative(fixtureDir, file)}`);
  for (const [id, , input, expected] of stages) {
    const mod = await import(pathToFileURL(path.join(fixtureDir, 'src', `stage-${id}.mjs`)).href + `?hidden=${Date.now()}-${id}`);
    const before = structuredClone(input);
    assert.deepEqual(mod.transform(input), expected, `hidden assertion: stage ${id}`);
    assert.deepEqual(input, before, `hidden assertion: stage ${id} mutated input`);
  }
}

describeLong('可选真实模型多阶段长程收敛回归', function () {
  this.timeout(evaluationTimeoutMs + 60_000);
  let evaluationId;
  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    createFixture();
    await browser.tauri.switchWindow('main');
    await browser.url('tauri://localhost/agent');
  });
  it('在 24 个阶段性义务中维持状态、修复、测试并通过外部断言', async () => {
    const started = await browser.executeAsync(async (selectedModelId, routingConfig, maxDurationMinutes, selectedWorkspace, evalPlanMode, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke('start_background_agent_evaluation', {
          modelRouting: { pro: selectedModelId, flash: selectedModelId, vision: selectedModelId, reasoning_effort: 'high', ...(routingConfig.routing_mode ? { routing_mode: routingConfig.routing_mode } : {}), ...(routingConfig.role_models ? { role_models: routingConfig.role_models } : {}) },
          maxDurationMinutes: maxDurationMinutes,
          scenario: 'isolated_write_fixture',
          routingPreset: null,
          workspaceOverride: selectedWorkspace,
          ...(evalPlanMode ? { planMode: evalPlanMode } : {}),
        });
        done({ ok: true, result });
      } catch (error) { done({ ok: false, error: String(error) }); }
    }, proModelId, routingConfig, maxDurationMinutes, fixtureDir, planMode);
    assert.equal(started?.ok, true, started?.error ?? '后台评测启动失败');
    evaluationId = started.result?.evaluation_id;
    assert.ok(evaluationId, '缺少评测 ID');
    const deadline = Date.now() + evaluationTimeoutMs;
    const reportPath = path.join(rootDir, 'agent_evaluations', `${evaluationId}.json`);
    while (!existsSync(reportPath) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 1000));
    assert.ok(existsSync(reportPath), '评测超时后未生成报告');
    const report = JSON.parse(readFileSync(reportPath, 'utf8'));
    assert.equal(report.trace_status, 'succeeded', JSON.stringify(report.warnings));
    assert.ok(report.metrics.llm_call_count >= 40, `长程基线模型调用不足：${report.metrics.llm_call_count}`);
    await assertHiddenAssertions();
    writeFileSync(path.join(artifactDir, `live-agent-long-convergence-${modelId}-${evaluationId}.json`), JSON.stringify(report, null, 2));
  });
});
