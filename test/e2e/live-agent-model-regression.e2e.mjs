import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const evaluationDir = path.join(rootDir, "agent_evaluations");
const artifactDir = path.join(rootDir, "test-artifacts");
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const evaluationTimeoutMs = Number(process.env.FREE_API_LIVE_EVALUATION_TIMEOUT_MS ?? 1_200_000);
const describeLive = modelId ? describe : describe.skip;

describeLive("可选真实模型长流程回归", () => {
  let evaluationId;

  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    await browser.tauri.switchWindow("main");
    await browser.url("tauri://localhost/agent");
  });

  it("以调用方指定 UUID 运行真实只读工程回归并落盘可审计报告", async () => {
    const started = await browser.executeAsync(async (selectedModelId, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: {
            pro: selectedModelId,
            flash: selectedModelId,
            vision: selectedModelId,
            reasoning_effort: "high",
          },
          maxDurationMinutes: 20,
          scenario: null,
          routingPreset: null,
        });
        done({ ok: true, result });
      } catch (error) {
        done({ ok: false, error: String(error) });
      }
    }, modelId);

    assert.equal(started.ok, true, started.error);
    evaluationId = started.result.evaluation_id;
    assert.match(evaluationId, /^agent-readonly-/);

    const reportPath = path.join(evaluationDir, `${evaluationId}.json`);
    await browser.waitUntil(() => existsSync(reportPath), {
      timeout: evaluationTimeoutMs,
      interval: 2_000,
      timeoutMsg: `评测 ${evaluationId} 未在时限内生成报告`,
    });

    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    assert.equal(report.routing?.reasoning_model_id, modelId);
    assert.equal(report.routing?.execution_model_id, modelId);
    assert.equal(report.routing?.preset, "requested");
    assert.ok(report.metrics?.llm_call_count >= 1, "真实评测至少应产生一次模型调用");
    writeFileSync(
      path.join(artifactDir, `live-agent-evaluation-${modelId}-${evaluationId}.json`),
      JSON.stringify(report, null, 2),
    );
  });
});
