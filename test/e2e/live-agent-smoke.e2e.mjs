// 冒烟测试：确定性假模型跑通全门禁管道（取题→派审查→过审→写码→测试→交卷）。
// 目的：把「管道自身 bug」与「模型能力问题」彻底解耦——剧本跑不通 = 制度 bug；
// 剧本跑通后真模型的失败才可归因于模型。启用：FREE_API_SMOKE=1。
import assert from "node:assert/strict";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startMockModel } from "./fixtures/mock-model-server.mjs";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const artifactDir = path.join(rootDir, "test-artifacts");
const evaluationDir = path.join(rootDir, "agent_evaluations");
const appDb = "/Users/wx/Library/Application Support/free-api-chat/data_v2.db";
const fixtureDir =
  process.env.FREE_API_LIVE_FIXTURE_DIR?.trim() ||
  path.join(artifactDir, `agent-smoke-${Date.now()}-${process.pid}`);
const MOCK_MODEL_ID = "mock-llm-smoke-001";
const MOCK_FAILOVER_MODEL_ID = "mock-llm-smoke-002";
const SMOKE_SCENARIO = process.env.FREE_API_SMOKE_SCENARIO ?? "happy";
const REVIEWER_FALLBACK_MODEL = "34217be8-d0a3-45fe-aba5-d62a4d21c486"; // gemini-3.8-flash-high
const describeSmoke = process.env.FREE_API_SMOKE === "1" ? describe : describe.skip;

const sql = (query) =>
  execSync(`sqlite3 ${JSON.stringify(appDb)} ${JSON.stringify(query)}`, { encoding: "utf8" });

function createFixture() {
  const files = new Map([
    ["package.json", `${JSON.stringify({ type: "module", scripts: { test: "node --test" } }, null, 2)}\n`],
    ["README.md", `# Smoke fixture（服务端判分 + 假模型）\n\n管道冒烟测试专用。运行 npm test 获取当前阶段任务；判分在宿主侧。\n`],
    ["test/progression.test.mjs", readFileSync(path.join(rootDir, "test/e2e/fixtures/progression-oracle.test.mjs"), "utf8")],
  ]);
  for (const [relative, content] of files) {
    const file = path.join(fixtureDir, relative);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
}

describeSmoke("冒烟：假模型端到端跑通全门禁管道", function () {
  this.timeout(600_000);

  let mock;
  let evaluationId;
  let verifyPort;
  let verifyNonce;

  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    mkdirSync(fixtureDir, { recursive: true });
    createFixture();
    mock = await startMockModel();
    // 注册假模型平台与模型（base_url 指向本地 mock）
    sql(`INSERT OR REPLACE INTO platforms (id, name, base_url, api_key, created_at) VALUES ('mockplat','mockplat','http://127.0.0.1:${mock.port}/v1','mock',datetime('now'))`);
    sql(`INSERT OR REPLACE INTO models (id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, provider_capabilities, created_at) VALUES ('${MOCK_MODEL_ID}','mockplat','mock-llm','Mock LLM',8192,0.7,1,'unknown',0,128000,'{}',datetime('now'))`);
    // failover 备选：注册在第二个 mock 平台上（跨平台 + 名字带 claude → 异构家族）
    sql(`INSERT OR REPLACE INTO platforms (id, name, base_url, api_key, created_at) VALUES ('mockplat2','mockplat2','http://127.0.0.1:${mock.port}/v1','mock',datetime('now'))`);
    sql(`INSERT OR REPLACE INTO models (id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, provider_capabilities, created_at) VALUES ('${MOCK_FAILOVER_MODEL_ID}','mockplat2','claude-mock-002','Mock Reviewer B',8192,0.7,1,'unknown',0,128000,'{}',datetime('now'))`);
    // 审查员角色卡钉到假模型（冒烟期间）
    sql(`UPDATE agent_role_cards SET model_id='${MOCK_MODEL_ID}' WHERE name='reviewer-cross'`);
    await browser.tauri.switchWindow("main");
    await browser.url("tauri://localhost/agent");
    // 等 __TAURI__ IPC 就绪（app 冷启动时 invoke 可能 5s 内不可用）
    await browser.waitUntil(
      async () => await browser.execute(() => !!(window.__TAURI__ && window.__TAURI__.core)),
      { timeout: 30_000, interval: 1_000, timeoutMsg: "__TAURI__ 未就绪" },
    );
  });

  after(async () => {
    // 清理：角色卡恢复、假模型行删除、mock 服务关闭
    try {
      sql(`UPDATE agent_role_cards SET model_id='${REVIEWER_FALLBACK_MODEL}' WHERE name='reviewer-cross'`);
      sql(`DELETE FROM models WHERE id IN ('${MOCK_MODEL_ID}','${MOCK_FAILOVER_MODEL_ID}')`);
      sql(`DELETE FROM platforms WHERE id IN ('mockplat','mockplat2')`);
    } catch {}
    await mock?.close();
  });

  it("剧本模型走通 取题→派审查→过审→写码→测试→交卷 全门禁管道", async () => {
    const started = await browser.executeAsync(async (selectedModelId, selectedWorkspace, done) => {
      try {
        const { invoke } = window.__TAURI__.core;
        const result = await invoke("start_background_agent_evaluation", {
          modelRouting: { pro: selectedModelId, flash: selectedModelId, vision: selectedModelId, reasoning_effort: "high" },
          maxDurationMinutes: 8,
          scenario: "isolated_write_fixture",
          routingPreset: null,
          workspaceOverride: selectedWorkspace,
          planMode: "cross_review",
        });
        done({ ok: true, result });
      } catch (error) { done({ ok: false, error: String(error) }); }
    }, MOCK_MODEL_ID, fixtureDir);
    assert.equal(started?.ok, true, started?.error ?? "冒烟评测启动失败");
    evaluationId = started.result?.evaluation_id;
    verifyPort = started.result?.verify_port;
    verifyNonce = started.result?.verify_nonce;
    assert.ok(evaluationId, "缺少评测 ID");
    assert.ok(verifyPort && verifyNonce, "缺少判分 oracle 端点");
    writeFileSync(path.join(fixtureDir, ".verify.json"), JSON.stringify({ port: verifyPort, nonce: verifyNonce }));

    const reportPath = path.join(rootDir, "agent_evaluations", `${evaluationId}.json`);
    const deadline = Date.now() + 480_000;
    while (!existsSync(reportPath) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 1000));
    assert.ok(existsSync(reportPath), "冒烟评测超时未生成报告");
    const report = JSON.parse(readFileSync(reportPath, "utf8"));

    // 冒烟核心断言：管道机制逐项验证（与模型能力完全无关）
    const actions = report.action_counts ?? {};
    const toolNames = Object.keys(actions).join("|");
    assert.ok(
      Object.keys(actions).some((k) => k.includes("child_reviewer")),
      "剧本第一步（派发审查）未发生",
    );
    assert.ok(Object.keys(actions).some((k) => k.includes("write_file")), "剧本写码步骤未发生");
    assert.ok(Object.keys(actions).some((k) => k.includes("shell_exec")), "npm test 未执行");
    assert.ok(!Object.keys(actions).some((k) => k.includes("ask_human")), "冒烟中不应升级人工（管道应自服务）");
    assert.ok(report.metrics.llm_call_count >= 6, `调用数异常：${report.metrics.llm_call_count}`);

    // 宿主判分进度：至少阶段 01 被送验通过并解锁
    const status = await (await fetch(`http://127.0.0.1:${verifyPort}/status?nonce=${verifyNonce}`)).json();
    assert.ok(status.index >= 1, `oracle 进度应 ≥ 1（实际 ${status.index}）`);
    if (SMOKE_SCENARIO !== "happy") {
      const sid = report.session_id;
      const fo = Number(execSync(
        `sqlite3 ${JSON.stringify(appDb)} "SELECT json_extract(snapshot_json,'$.plan_reviewer_failover_count') FROM agent_checkpoints WHERE run_id='${sid}'"`,
        { encoding: "utf8" },
      ).trim());
      assert.ok(fo >= 1, `故障注入场景下 failover 应至少执行 1 次（实际 ${fo}）`);
    }

    writeFileSync(path.join(artifactDir, `smoke-${evaluationId}.json`), JSON.stringify(report, null, 2));
  });
});
