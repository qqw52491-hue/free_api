// 评测套具实弹 spec：被 scripts/eval_runner.mjs --live 以环境变量驱动。
// 职责：唤起应用内后台评测（custom_goal × 指定模型 × 沙盒工作区），
// 等待运行到达终态，把 session_id 与终态落盘供判分器收取。
// 不做质量断言——判分交给 eval_runner 的机械判据（FAIL_TO_PASS 测试补丁）。
import assert from "node:assert/strict";
import { execSync } from "node:child_process";
import { writeFileSync } from "node:fs";

const appDb = "/Users/wx/Library/Application Support/free-api-chat/data_v2.db";
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const reviewerModelId = process.env.FREE_API_LIVE_REVIEWER_MODEL_ID?.trim() || null;
const fixtureDir = process.env.FREE_API_LIVE_FIXTURE_DIR?.trim();
const maxDurationMinutes = Number(process.env.FREE_API_LIVE_MAX_DURATION_MINUTES ?? 20);
const sessionFile = process.env.FREE_API_EVAL_SESSION_FILE?.trim();
const planMode = process.env.FREE_API_EVALUATION_PLAN_MODE?.trim() || null;

describe("评测套具实弹运行（custom_goal × 沙盒工作区）", function () {
  this.timeout((maxDurationMinutes + 12) * 60_000);

  it("唤起后台评测并等待运行终态", async () => {
    assert.ok(modelId, "需要 FREE_API_LIVE_MODEL_ID");
    assert.ok(fixtureDir, "需要 FREE_API_LIVE_FIXTURE_DIR（沙盒工作区）");
    assert.ok(sessionFile, "需要 FREE_API_EVAL_SESSION_FILE");

    const started = await browser.executeAsync(async (model, reviewerModel, workspace, plan, minutes, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: {
            pro: model,
            flash: model,
            vision: model,
            reasoning_effort: "high",
            ...(reviewerModel ? { role_models: { reviewer: reviewerModel } } : {}),
          },
          maxDurationMinutes: minutes,
          scenario: "custom_goal",
          routingPreset: null,
          workspaceOverride: workspace,
          ...(plan ? { planMode: plan } : {}),
        });
        done({ ok: true, result });
      } catch (error) { done({ ok: false, error: String(error) }); }
    }, modelId, reviewerModelId, fixtureDir, planMode, maxDurationMinutes);
    assert.equal(started?.ok, true, started?.error ?? "后台评测启动失败");
    const sessionId = started.result?.session_id;
    assert.ok(sessionId, "缺少 session_id");
    writeFileSync(sessionFile, sessionId, "utf8");

    // 轮询 agent_run_traces 直到运行终态
    const deadline = Date.now() + (maxDurationMinutes + 8) * 60_000;
    let status = "";
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 10_000));
      try {
        status = execSync(
          `sqlite3 ${JSON.stringify(appDb)} "SELECT status FROM agent_run_traces WHERE session_id='${sessionId}' ORDER BY id DESC LIMIT 1"`,
          { encoding: "utf8" }
        ).trim();
      } catch { /* 重试 */ }
      if (["succeeded", "failed", "cancelled"].includes(status)) break;
    }
    console.log(`=== 评测终态: ${status || "超时未观测到终态"} (session: ${sessionId}) ===`);
    writeFileSync(`${sessionFile}.status`, status || "unknown", "utf8");
  });
});
