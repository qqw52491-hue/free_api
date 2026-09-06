// 真实任务试产 spec：把门禁体系放到非夹具的真实任务上跑（custom_goal 场景）。
// 与 oracle 驱动的长程评测分离：无判分端点断言、无阶段进度断言——产出的是
// 「真实任务 × L2 档」的三个账单数字：审查 token 占比、拦截/失败次数、人工介入次数。
// 用法：FREE_API_EVALUATION_SCENARIO=custom_goal FREE_API_EVAL_CUSTOM_GOAL="<任务目标>" \
//      FREE_API_LIVE_FIXTURE_DIR="<白名单内工作区>" FREE_API_LIVE_MAX_DURATION_MINUTES=10 \
//      FREE_API_LIVE_LONG_CONVERGENCE=1 npx wdio run wdio.conf.mjs --spec test/e2e/live-agent-real-task.e2e.mjs
import assert from "node:assert/strict";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const artifactDir = path.join(rootDir, "test-artifacts");
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const maxDurationMinutes = Number(process.env.FREE_API_LIVE_MAX_DURATION_MINUTES ?? 10);
const evalScenario = process.env.FREE_API_EVALUATION_SCENARIO?.trim() || "custom_goal";
const planMode = process.env.FREE_API_EVALUATION_PLAN_MODE?.trim() || null;
const fixtureDir = process.env.FREE_API_LIVE_FIXTURE_DIR?.trim();
const describeReal = modelId && process.env.FREE_API_LIVE_LONG_CONVERGENCE === "1" ? describe : describe.skip;

const appDb = "/Users/wx/Library/Application Support/free-api-chat/data_v2.db";
const REVIEWER_FALLBACK_MODEL = "34217be8-d0a3-45fe-aba5-d62a4d21c486";

describeReal("真实任务试产（custom_goal × L2 档）", function () {
  this.timeout(1_200_000);

  it("在真实任务上跑通 L2 门禁并产出账单", async () => {
    assert.ok(modelId, "需要 FREE_API_LIVE_MODEL_ID");
    assert.ok(fixtureDir, "需要 FREE_API_LIVE_FIXTURE_DIR（白名单内的隔离工作区）");
    mkdirSync(fixtureDir, { recursive: true });
    mkdirSync(artifactDir, { recursive: true });

    const started = await browser.executeAsync(async (selectedModelId, selectedWorkspace, evalPlanMode, evalScenario, minutes, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: { pro: selectedModelId, flash: selectedModelId, vision: selectedModelId, reasoning_effort: "high" },
          maxDurationMinutes: minutes,
          scenario: evalScenario,
          routingPreset: null,
          workspaceOverride: selectedWorkspace,
          ...(evalPlanMode ? { planMode: evalPlanMode } : {}),
        });
        done({ ok: true, result });
      } catch (error) { done({ ok: false, error: String(error) }); }
    }, modelId, fixtureDir, planMode, evalScenario, maxDurationMinutes);
    assert.equal(started?.ok, true, started?.error ?? "真实任务评测启动失败");
    const sessionId = started.result?.session_id;
    assert.ok(sessionId, "缺少 session_id");
    writeFileSync(path.join(artifactDir, "real-task-session.txt"), sessionId);

    // 等报告落盘（评测自行收敛/超时/仲裁封顶后停止）
    const reportPath = path.join(rootDir, "agent_evaluations");
    const deadline = Date.now() + (maxDurationMinutes + 5) * 60_000;
    let reportFile = "";
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 5_000));
      try {
        // custom_goal 的报告在 runs/ 与 monitor/ 子目录（evaluation_id=agent-custom-*）
        const found = execSync(`grep -rl "${sessionId}" ${reportPath} ${reportPath}/runs ${reportPath}/monitor 2>/dev/null | head -1 || true`, { encoding: "utf8" }).trim();
        if (found) { reportFile = found.split("\n")[0]; break; }
      } catch { /* continue polling */ }
    }
    assert.ok(reportFile, "评测报告未在时限内生成");
    const report = JSON.parse(require("node:fs").readFileSync(reportFile, "utf8"));

    // 账单三数
    const tokens = execSync(
      `sqlite3 ${JSON.stringify(appDb)} "SELECT coalesce(sum(prompt_tokens),0), coalesce(sum(completion_tokens),0) FROM agent_llm_calls WHERE session_id='${sessionId}'"`,
      { encoding: "utf8" },
    ).trim().split("|");
    const mainPrompt = Number(tokens[0]);
    const mainCompletion = Number(tokens[1]);
    const review = execSync(
      `sqlite3 ${JSON.stringify(appDb)} "SELECT coalesce(sum(json_extract(result_json,'$.prompt_tokens')),0), coalesce(sum(json_extract(result_json,'$.completion_tokens')),0), count(*) FROM agent_child_runs WHERE parent_session_id='${sessionId}'"`,
      { encoding: "utf8" },
    ).trim().split("|");
    const reviewPrompt = Number(review[0]);
    const reviewCompletion = Number(review[1]);
    const reviewShare = mainPrompt + mainCompletion > 0
      ? Math.round(1000 * (reviewPrompt + reviewCompletion) / (mainPrompt + mainCompletion + reviewPrompt + reviewCompletion)) / 10
      : 0;

    const bill = {
      session_id: sessionId,
      trace_status: report.trace_status,
      llm_calls: report.metrics.llm_call_count,
      tool_steps: report.metrics.tool_step_count,
      tool_failures: report.metrics.tool_failure_count,
      main_prompt_tokens: mainPrompt,
      main_completion_tokens: mainCompletion,
      review_prompt_tokens: reviewPrompt,
      review_completion_tokens: reviewCompletion,
      review_token_share_percent: reviewShare,
      review_runs: Number(review[2]),
      warnings: report.warnings,
      verdict: report.verdict,
    };
    writeFileSync(path.join(artifactDir, `real-task-bill-${sessionId}.json`), JSON.stringify(bill, null, 2));
    console.log("=== 真实任务账单 ===");
    console.log(JSON.stringify(bill, null, 2));

    // 基线断言：有真实调用、有产出，具体质量看账单人工判读
    assert.ok(bill.llm_calls >= 3, `调用数异常：${bill.llm_calls}`);
  });
});
