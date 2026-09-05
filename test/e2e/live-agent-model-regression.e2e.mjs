import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const evaluationDir = path.join(rootDir, "agent_evaluations");
const artifactDir = path.join(rootDir, "test-artifacts");
const modelId = process.env.FREE_API_LIVE_MODEL_ID?.trim();
const proModelId = process.env.FREE_API_LIVE_MODEL_ID_PRO?.trim() || modelId;
const flashModelId = process.env.FREE_API_LIVE_MODEL_ID_FLASH?.trim() || modelId;
const visionModelId = process.env.FREE_API_LIVE_MODEL_ID_VISION?.trim() || modelId;
const routingMode = process.env.FREE_API_ROUTING_MODE?.trim() || "";
const roleModelsRaw = process.env.FREE_API_ROLE_MODELS?.trim() || "";
const roleModels = roleModelsRaw ? JSON.parse(roleModelsRaw) : null;
const evaluationTimeoutMs = Number(process.env.FREE_API_LIVE_EVALUATION_TIMEOUT_MS ?? 1_200_000);
const writeFixtureEnabled = process.env.FREE_API_LIVE_WRITE_FIXTURE === "1";
const planMode = process.env.FREE_API_EVALUATION_PLAN_MODE?.trim() || null;
const fixtureDir = path.join(artifactDir, `agent-write-fixture-${Date.now()}-${process.pid}`);
const protectedFixtureFiles = new Map();
const describeLive = modelId ? describe : describe.skip;

function createWriteFixture() {
  const files = new Map([
    ["package.json", `${JSON.stringify({ type: "module", scripts: { test: "node --test" } }, null, 2)}\n`],
    ["README.md", "# Reading fixture\n\nRepair only src/summarize-readings.mjs so npm test passes. Do not modify package.json, README.md, or test/. Valid readings have non-empty string ids and finite numeric or trimmed numeric-string values. Normalize values to 0–100; retain the highest per id; return retained-id count, retained-value average, and top three sorted by value descending then id ascending; do not mutate inputs.\n"],
    ["src/summarize-readings.mjs", "export function summarizeReadings(readings) {\n  const rows = readings.filter((row) => row && typeof row.id === 'string' && typeof row.value === 'number');\n  const top = rows.sort((left, right) => left.value - right.value).slice(0, 3);\n  const average = rows.length === 0 ? 0 : rows.reduce((sum, row) => sum + row.value, 0) / rows.length;\n  return { acceptedCount: rows.length, average, top };\n}\n"],
    ["test/readings.test.mjs", "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { summarizeReadings } from '../src/summarize-readings.mjs';\n\ntest('keeps highest normalized reading per id', () => {\n  assert.deepEqual(\n    summarizeReadings([{ id: 'a', value: 40 }, { id: 'b', value: 90 }, { id: 'a', value: '80' }, { id: 'bad', value: 'x' }]),\n    { acceptedCount: 2, average: 85, top: [{ id: 'b', value: 90 }, { id: 'a', value: 80 }] },\n  );\n});\n"],
  ]);
  for (const [relativePath, content] of files) {
    const pathname = path.join(fixtureDir, relativePath);
    mkdirSync(path.dirname(pathname), { recursive: true });
    writeFileSync(pathname, content);
    if (!relativePath.startsWith("src/")) protectedFixtureFiles.set(pathname, content);
  }
}

describeLive("可选真实模型长流程回归", function () {
  // 真实评测可持续二十分钟；外层测试必须覆盖等待时间与启动/清理余量，
  // 不能被 Wdio 的默认三分钟 Mocha 时限抢先中断。
  this.timeout(evaluationTimeoutMs + 60_000);

  let evaluationId;

  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    if (writeFixtureEnabled) createWriteFixture();
    await browser.tauri.switchWindow("main");
    await browser.url("tauri://localhost/agent");
  });

  it("以调用方指定 UUID 运行真实只读工程回归并落盘可审计报告", async () => {
    const started = await browser.executeAsync(async (proModelId, flashModelId, visionModelId, routingMode, roleModels, selectedScenario, selectedWorkspace, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: {
            pro: proModelId,
            flash: flashModelId,
            vision: visionModelId,
            reasoning_effort: "high",
            ...(routingMode ? { routing_mode: routingMode } : {}),
            ...(roleModels ? { role_models: roleModels } : {}),
          },
          maxDurationMinutes: 20,
          scenario: selectedScenario,
          routingPreset: null,
          workspaceOverride: selectedWorkspace,
          ...(planMode ? { planMode } : {}),
        });
        done({ ok: true, result });
      } catch (error) {
        done({ ok: false, error: String(error) });
      }
    }, proModelId, flashModelId, visionModelId, routingMode, roleModels, writeFixtureEnabled ? "isolated_write_fixture" : null, writeFixtureEnabled ? fixtureDir : null);

    assert.equal(started.ok, true, started.error);
    evaluationId = started.result.evaluation_id;
    assert.match(evaluationId, writeFixtureEnabled ? /^agent-write-fixture-/ : /^agent-readonly-/);

    const reportPath = path.join(evaluationDir, `${evaluationId}.json`);
    await browser.waitUntil(() => existsSync(reportPath), {
      timeout: evaluationTimeoutMs,
      interval: 2_000,
      timeoutMsg: `评测 ${evaluationId} 未在时限内生成报告`,
    });

    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    if (!process.env.FREE_API_LIVE_MODEL_ID_PRO) {
      assert.equal(report.routing?.reasoning_model_id, modelId);
      assert.equal(report.routing?.execution_model_id, modelId);
    }
    assert.equal(report.routing?.preset, "requested");
    assert.ok(report.metrics?.llm_call_count >= 1, "真实评测至少应产生一次模型调用");
    if (writeFixtureEnabled) {
      assert.equal(report.trace_status, "succeeded");
      for (const [pathname, original] of protectedFixtureFiles) {
        assert.equal(readFileSync(pathname, "utf8"), original, `${path.basename(pathname)} 不应被 Agent 修改`);
      }
      const moduleUrl = `${pathToFileURL(path.join(fixtureDir, "src/summarize-readings.mjs")).href}?run=${evaluationId}`;
      const { summarizeReadings } = await import(moduleUrl);
      const input = [{ id: "beta", value: "110" }, { id: "alpha", value: "42.5" }, { id: "alpha", value: 42.5 }, { id: "gamma", value: -4 }, { id: "skip", value: Infinity }];
      const before = structuredClone(input);
      assert.deepEqual(
        summarizeReadings(input),
        { acceptedCount: 3, average: 47.5, top: [{ id: "beta", value: 100 }, { id: "alpha", value: 42.5 }, { id: "gamma", value: 0 }] },
      );
      assert.deepEqual(input, before, "生产实现不得修改调用方输入");
    }
    writeFileSync(
      path.join(artifactDir, `live-agent-evaluation-${modelId}-${evaluationId}.json`),
      JSON.stringify(report, null, 2),
    );
  });
});
