import assert from "node:assert/strict";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const artifactDir = path.join(rootDir, "test-artifacts");
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const maxDurationMinutes = Number(process.env.FREE_API_LIVE_MAX_DURATION_MINUTES ?? 60);
// 轮询期限必须严格大于运行时限（supervise_worker 取消 → 120s 安全等待 → 报告落盘），
// 否则 app 还没写完报告 spec 就先放弃并拆掉实例——2026-08-30 实测两期限同为 3600s 时报告丢失。
const evaluationTimeoutMs = Math.max(
  Number(process.env.FREE_API_LIVE_EVALUATION_TIMEOUT_MS ?? 5_400_000),
  maxDurationMinutes * 60_000 + 300_000
);
const reasoningEffort = process.env.FREE_API_LIVE_REASONING_EFFORT?.trim() || "high";
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
const describeLong = modelId && enabled ? describe : describe.skip;

const ORACLE_TEMPLATE = path.join(rootDir, "test/e2e/fixtures/progression-oracle.test.mjs");
const TOTAL_STAGES = 24;

function createFixture() {
  const files = new Map([
    ["package.json", `${JSON.stringify({ type: "module", scripts: { test: "node --test" } }, null, 2)}\n`],
    ["README.md", `# Staged convergence fixture（服务端判分版）\n\n这是一段 24 阶段的真实修复任务。题目、输入与判分全部由宿主 oracle 服务提供——本目录内不存在任何答案。\n\n运行 \`npm test\`：测试会自动获取当前阶段任务（并写入 CURRENT_STAGE.md），验证你的实现后自动解锁下一阶段；全部通过前 npm test 以非零退出。\n\n只修改 \`src/\` 中当前阶段文件；不要修改 README、package.json、CURRENT_STAGE.md、test/ 或 .verify.json。\n`],
    ["test/progression.test.mjs", readFileSync(ORACLE_TEMPLATE, "utf8")],
  ]);
  for (const [relative, content] of files) {
    const file = path.join(fixtureDir, relative);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
  // git 化 fixture：shell 批量修改探针依赖 git status 差值，真实项目均有 git
  try {
    const run = (args) => execSync(`git ${args}`, { cwd: fixtureDir, stdio: "ignore" });
    run("init -q");
    run("-c user.email=eval@local -c user.name=eval add -A");
    run("-c user.email=eval@local -c user.name=eval commit -qm init");
  } catch {
    // git 不可用时探针自动退化为按调用计数（阶段审查仍有 todo/文件数兜底）
  }
}

describeLong("可选真实模型多阶段长程收敛回归（服务端判分）", function () {
  // 长程评测可持续数十分钟；外层测试必须覆盖等待时间与启动/清理余量。
  this.timeout(evaluationTimeoutMs + 60_000);

  let evaluationId;
  let verifyPort;
  let verifyNonce;

  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    mkdirSync(fixtureDir, { recursive: true });
    createFixture();
    await browser.tauri.switchWindow("main");
    await browser.url("tauri://localhost/agent");
  });

  it("在 24 个阶段性义务中维持状态、修复、测试并通过宿主判分", async () => {
    const started = await browser.executeAsync(async (selectedModelId, routingConfig, maxDurationMinutes, selectedWorkspace, evalPlanMode, evalReasoningEffort, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: { pro: selectedModelId, flash: selectedModelId, vision: selectedModelId, reasoning_effort: evalReasoningEffort, ...(routingConfig.routing_mode ? { routing_mode: routingConfig.routing_mode } : {}), ...(routingConfig.role_models ? { role_models: routingConfig.role_models } : {}) },
          maxDurationMinutes: maxDurationMinutes,
          scenario: "isolated_write_fixture",
          routingPreset: null,
          workspaceOverride: selectedWorkspace,
          ...(evalPlanMode ? { planMode: evalPlanMode } : {}),
        });
        done({ ok: true, result });
      } catch (error) { done({ ok: false, error: String(error) }); }
    }, proModelId, routingConfig, maxDurationMinutes, fixtureDir, planMode, reasoningEffort);
    assert.equal(started?.ok, true, started?.error ?? "后台评测启动失败");
    evaluationId = started.result?.evaluation_id;
    verifyPort = started.result?.verify_port;
    verifyNonce = started.result?.verify_nonce;
    assert.ok(evaluationId, "缺少评测 ID");
    assert.ok(verifyPort && verifyNonce, "写入型评测必须返回判分 oracle 端点");
    // 鉴权凭据落盘：progression 测试据此与宿主 oracle 通信（答案表在宿主侧，落盘的不是答案）
    writeFileSync(path.join(fixtureDir, ".verify.json"), JSON.stringify({ port: verifyPort, nonce: verifyNonce }));

    const reportPath = path.join(rootDir, "agent_evaluations", `${evaluationId}.json`);
    const deadline = Date.now() + evaluationTimeoutMs;
    while (!existsSync(reportPath) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 1000));
    assert.ok(existsSync(reportPath), "评测超时后未生成报告");
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    assert.equal(report.trace_status, "succeeded", JSON.stringify(report.warnings));
    assert.ok(report.metrics.llm_call_count >= 40, `长程基线模型调用不足：${report.metrics.llm_call_count}`);

    // 终验走宿主 oracle：全部 24 阶段必须逐一经服务端判分通过（正确性的唯一权威）
    const status = await (await fetch(`http://127.0.0.1:${verifyPort}/status?nonce=${verifyNonce}`)).json();
    assert.equal(status.total, TOTAL_STAGES);
    assert.ok(status.completed, `宿主判分未完成全部阶段：${status.index}/${status.total}`);
    writeFileSync(path.join(artifactDir, `live-agent-long-convergence-${modelId}-${evaluationId}.json`), JSON.stringify(report, null, 2));
  });
});
