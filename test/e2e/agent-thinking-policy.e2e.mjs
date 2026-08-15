import assert from "node:assert/strict";
import { mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const artifactDir = path.join(rootDir, "test-artifacts");
const routingStorageKey = "free-api.agent.model-routing.v1";
// 所有 E2E 模型路由统一使用已配置的 DeepSeek 模型，禁止回退到其他平台。
const deepSeekTestRouting = Object.freeze({
  reasoning_effort_version: 2,
  pro: "44a7d8e6-ca6e-4768-ad2b-5049adcccc58", // deepseek-v4-pro
  flash: "190c3bdf-91fc-4de3-8174-8e2c52ef8a15", // deepseek-v4-flash
  vision: "190c3bdf-91fc-4de3-8174-8e2c52ef8a15", // deepseek-v4-flash
});
let originalRouting;

async function waitForPolicy(expected) {
  const policyLabel = await $("[data-testid='active-reasoning-policy']");
  const text = `档位 · 推理 ${expected.pro} · 执行 ${expected.flash} · 视觉 ${expected.vision}`;
  await browser.waitUntil(async () => (await policyLabel.getText()) === text, {
    timeout: 15_000,
    timeoutMsg: `顶部思考档位没有更新为 ${text}`,
  });
  return policyLabel;
}

async function applyPolicies(policies) {
  const result = await browser.executeAsync((items, done) => {
    const entries = Object.entries(items);
    const next = {};
    const applyNext = (index) => {
      if (index >= entries.length) { done(next); return; }
      const [testId, value] = entries[index];
      const element = document.querySelector(`[data-testid="${testId}"]`);
      if (!(element instanceof HTMLSelectElement)) { done({ error: `未找到策略选择器：${testId}` }); return; }
      element.value = value;
      element.dispatchEvent(new Event("change", { bubbles: true }));
      next[testId] = element.value;
      window.setTimeout(() => applyNext(index + 1), 0);
    };
    applyNext(0);
  }, policies);
  if (result?.error) throw new Error(result.error);
  assert.deepEqual(result, policies);
}

async function readPolicies() {
  return await browser.execute(() => {
    const get = (testId) => {
      const element = document.querySelector(`[data-testid="${testId}"]`);
      return element instanceof HTMLSelectElement ? element.value : null;
    };
    return {
      "pro-reasoning-policy-select": get("pro-reasoning-policy-select"),
      "flash-reasoning-policy-select": get("flash-reasoning-policy-select"),
      "vision-reasoning-policy-select": get("vision-reasoning-policy-select"),
    };
  });
}

describe("Agent 独立思考档位原生窗口交互", () => {
  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    await browser.tauri.switchWindow("main");
    await browser.url("tauri://localhost/agent");
    await (await $("[data-testid='agent-model-routing-trigger']")).waitForDisplayed({ timeout: 15_000 });
    originalRouting = await browser.execute((key) => window.localStorage.getItem(key), routingStorageKey);
    await browser.execute((key, routing) => {
      let current = {};
      try {
        const raw = window.localStorage.getItem(key);
        current = raw ? JSON.parse(raw) : {};
      } catch {
        current = {};
      }
      window.localStorage.setItem(key, JSON.stringify({ ...current, ...routing }));
    }, routingStorageKey, deepSeekTestRouting);
    await browser.refresh();
    await (await $("[data-testid=agent-model-routing-trigger]")).waitForDisplayed({ timeout: 15_000 });
  });

  after(async () => {
    if (originalRouting === undefined) return;
    await browser.execute((key, value) => {
      if (value === null) window.localStorage.removeItem(key);
      else window.localStorage.setItem(key, value);
    }, routingStorageKey, originalRouting);
  });

  it("可分别配置三类模型的档位，并在刷新后完整恢复", async () => {
    const policies = {
      "pro-reasoning-policy-select": "high",
      "flash-reasoning-policy-select": "medium",
      "vision-reasoning-policy-select": "low",
    };

    await (await $("[data-testid='agent-model-routing-trigger']")).click();
    await (await $("[data-testid='agent-settings-drawer']")).waitForDisplayed();
    await (await $("[data-testid='pro-reasoning-policy-select']")).waitForDisplayed();
    const selectedModels = await browser.execute(() => Object.fromEntries([
      ["pro", "pro-model-select"],
      ["flash", "flash-model-select"],
      ["vision", "vision-model-select"],
    ].map(([route, testId]) => {
      const element = document.querySelector(`[data-testid=${testId}]`);
      return [route, element instanceof HTMLSelectElement ? element.value : null];
    })));
    assert.deepEqual(selectedModels, {
      pro: deepSeekTestRouting.pro,
      flash: deepSeekTestRouting.flash,
      vision: deepSeekTestRouting.vision,
    }, "模型路由必须全部固定为指定 DeepSeek 模型");
    const effortOptions = await browser.execute(() => {
      const ids = ["pro-reasoning-policy-select", "flash-reasoning-policy-select", "vision-reasoning-policy-select"];
      return Object.fromEntries(ids.map((id) => {
        const select = document.querySelector(`[data-testid="${id}"]`);
        return [id, select instanceof HTMLSelectElement ? Array.from(select.options, (option) => option.value) : []];
      }));
    });
    for (const values of Object.values(effortOptions)) {
      assert.ok(values.includes("high"), "缺少高档位");
      assert.ok(values.includes("medium"), "缺少中档位");
      assert.ok(values.includes("low"), "缺少低档位");
    }

    // macOS WKWebView 将 select 菜单交给系统控件渲染；批量派发真实 change 事件，
    // 覆盖 Vue 更新、localStorage 持久化和顶部摘要刷新，同时减少驱动往返延迟。
    await applyPolicies(policies);
    await waitForPolicy({ pro: "高", flash: "中", vision: "低" });

    const stored = await browser.execute((key) => window.localStorage.getItem(key), routingStorageKey);
    const routing = JSON.parse(stored);
    assert.equal(routing.pro_reasoning_effort, "high");
    assert.equal(routing.flash_reasoning_effort, "medium");
    assert.equal(routing.vision_reasoning_effort, "low");

    await browser.saveScreenshot(path.join(artifactDir, "agent-independent-thinking-policies.png"));
    await browser.refresh();
    await waitForPolicy({ pro: "高", flash: "中", vision: "低" });

    await (await $("[data-testid='agent-model-routing-trigger']")).click();
    await (await $("[data-testid='pro-reasoning-policy-select']")).waitForDisplayed();
    assert.deepEqual(await readPolicies(), policies);
  });
});
