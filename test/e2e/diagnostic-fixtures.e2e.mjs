import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

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
const describeLive = modelId ? describe : describe.skip;

// ---------------------------------------------------------------------------
// 三个诊断型 fixture：把真实修过的 MCP/Skill 缺陷复刻为隔离迷你工程。
// 全部文件受保护（byte 不变），唯一允许写入的是根目录 DIAGNOSIS.json——
// 隐藏断言的物理落点。断言藏在本文件里，模型不可见。
// 场景对单/多 Agent 对照通用：同一问题、同一隔离工作区、同一隐藏断言。
// ---------------------------------------------------------------------------

function fixtureFiles() {
  return {
    "diagnostic_fake_action": new Map([
      ["TASK.md", "# 任务：诊断 batch_import 调用失败循环\n\n某 Agent 反复调用 skill_manager 的 batch_import，每次都失败；错误消息却声称该动作受支持。\n请阅读 src/ 下源码与 manual，找出循环根因，按任务指令输出 DIAGNOSIS.json。\n"],
      ["src/main.rs", "mod registry;\nmod manual;\n\nfn main() {\n    let request = std::env::args().nth(1).unwrap_or_default();\n    match registry::dispatch(&request) {\n        Ok(output) => println!(\"{}\", output),\n        Err(error) => {\n            eprintln!(\"{}\", error);\n            std::process::exit(1);\n        }\n    }\n}\n"],
      ["src/manual.rs", "/// 工具手册（供模型阅读的能力声明）。\n/// 支持的动作：list、add、delete、batch_import、toggle。\npub fn documented_actions() -> &'static [&'static str] {\n    &[\"list\", \"add\", \"delete\", \"batch_import\", \"toggle\"]\n}\n"],
      ["src/registry.rs", "use crate::manual;\n\npub fn dispatch(action: &str) -> Result<String, String> {\n    match action {\n        \"list\" => Ok(\"[skill a, skill b]\".to_string()),\n        \"add\" => Ok(\"skill added\".to_string()),\n        \"delete\" => Ok(\"skill deleted\".to_string()),\n        _ => Err(format!(\n            \"未知动作：{}。支持的动作：list、add、delete、batch_import、toggle\",\n            action\n        )),\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn manual_covers_more_than_implementation() {\n        let implemented = [\"list\", \"add\", \"delete\"];\n        let documented = crate::manual::documented_actions();\n        assert!(documented.len() > implemented.len());\n    }\n}\n"],
    ]),
    "diagnostic_plugin_removal": new Map([
      ["TASK.md", "# 任务：诊断已删除插件的工具为何仍可见\n\n管理员删除了数据库管理的 MCP 插件 github_tools：DB 行已删除、目录缓存已失效，\n但其工具仍出现在运行中的工具菜单里，调用仍能成功，重启后才消失。\n请追踪 src/ 中 \"删除 → DB → 缓存 → 注册表 → 菜单\" 的调用链，找出仍持有可见性的组件，\n按任务指令输出 DIAGNOSIS.json。\n"],
      ["src/manager.rs", "pub mod catalog;\npub mod registry;\npub mod session_sync;\n\n/// 管理工具的删除入口。\npub fn delete_plugin(db: &mut Vec<String>, name: &str) -> Result<String, String> {\n    db.retain(|existing| existing != name);\n    catalog::invalidate_plugin(name);\n    Ok(format!(\"已删除插件 {}\", name))\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn delete_removes_db_row_and_invalidates_cache_but_registry_keeps_client() {\n        let mut db = vec![\"github_tools\".to_string()];\n        let message = super::delete_plugin(&mut db, \"github_tools\").unwrap();\n        assert!(message.contains(\"已删除\"));\n        assert!(!db.contains(&\"github_tools\".to_string()));\n        assert!(registry::clients().contains_key(\"github_tools\"),\n            \"删除后注册表仍持有 client——这正是要诊断的现象\");\n    }\n}\n"],
      ["src/catalog.rs", "use std::cell::RefCell;\n\nthread_local! {\n    static CACHE: RefCell<std::collections::HashMap<String, Vec<String>>> =\n        RefCell::new(std::collections::HashMap::new());\n}\n\npub fn prime(name: &str, tools: Vec<String>) {\n    CACHE.with(|cache| cache.borrow_mut().insert(name.to_string(), tools));\n}\n\n/// 删除时使目录缓存失效：只清缓存条目，不动任何运行时组件。\npub fn invalidate_plugin(name: &str) {\n    CACHE.with(|cache| {\n        cache.borrow_mut().remove(name);\n    });\n}\n\npub fn list_tools(name: &str) -> Option<Vec<String>> {\n    CACHE.with(|cache| cache.borrow().get(name).cloned())\n}\n"],
      ["src/registry.rs", "use std::collections::HashMap;\nuse std::sync::Mutex;\n\nstatic CLIENTS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);\n\npub fn register(name: &str, last_known_tools: Vec<String>) {\n    let mut guard = CLIENTS.lock().unwrap();\n    guard\n        .get_or_insert_with(HashMap::new)\n        .insert(name.to_string(), last_known_tools);\n}\n\npub fn clients() -> HashMap<String, Vec<String>> {\n    CLIENTS.lock().unwrap().clone().unwrap_or_default()\n}\n\n/// 工具菜单：目录缓存缺失时回退到 client 的最近已知工具。\n/// 对 TTL 过期的长会话有价值；但配置已撤销时它让删除失效。\npub fn format_tools_menu() -> Vec<String> {\n    let mut menu = Vec::new();\n    for (name, tools) in clients() {\n        let entry = crate::catalog::list_tools(&name).unwrap_or(tools);\n        for tool in entry {\n            menu.push(format!(\"{}::{}\", name, tool));\n        }\n    }\n    menu\n}\n"],
      ["src/session_sync.rs", "/// 会话同步：遍历 DB 现存插件并建立连接。\n/// 已删除的插件不在 load_managed_plugins 的结果里，\n/// 因此这里没有任何分支会移除运行中的 client。\npub fn load_managed_plugins(db: &[String]) -> Vec<String> {\n    db.to_vec()\n}\n\npub fn sync(db: &[String]) {\n    for name in load_managed_plugins(db) {\n        crate::registry::register(&name, vec![\"search\".to_string()]);\n    }\n}\n"],
    ]),
    "diagnostic_false_leak": new Map([
      ["TASK.md", "# 任务：诊断删除后短暂可见的插件工具\n\n管理员删除数据库管理的 MCP 插件 github_tools：DB 行、目录缓存和运行时 client 都已移除；但当前 UI 帧的工具菜单仍短暂可见，完全重启后消失。请阅读 src/，追踪删除 → DB → 缓存 → registry/client → 菜单快照/刷新调用链。不要把泄漏、zombie 或跨会话残留当作事实，除非源码直接证明；按任务指令输出 DIAGNOSIS.json。\n"],
      ["src/manager.rs", "pub mod catalog;\npub mod menu;\npub mod registry;\n\n/// 删除入口同时删除持久化行、缓存与运行时 client。\npub fn delete_plugin(db: &mut Vec<String>, name: &str) {\n    db.retain(|item| item != name);\n    catalog::invalidate_plugin(name);\n    registry::remove_client(name);\n    menu::schedule_refresh();\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn deletion_releases_client_before_menu_snapshot_refreshes() {\n        let mut db = vec![\"github_tools\".to_string()];\n        super::delete_plugin(&mut db, \"github_tools\");\n        assert!(!db.contains(&\"github_tools\".to_string()));\n        assert!(!super::registry::has_client(\"github_tools\"));\n        assert!(super::menu::visible_tools().contains(&\"github_tools.search\".to_string()));\n        super::menu::flush_scheduled_refresh();\n        assert!(!super::menu::visible_tools().contains(&\"github_tools.search\".to_string()));\n    }\n}\n"],
      ["src/catalog.rs", "use std::collections::HashMap;\nuse std::sync::Mutex;\n\nstatic CACHE: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);\n\npub fn invalidate_plugin(name: &str) {\n    if let Some(cache) = CACHE.lock().unwrap().as_mut() {\n        cache.remove(name);\n    }\n}\n"],
      ["src/registry.rs", "use std::collections::HashMap;\nuse std::sync::Mutex;\n\nstatic CLIENTS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);\n\npub fn remove_client(name: &str) {\n    if let Some(clients) = CLIENTS.lock().unwrap().as_mut() {\n        clients.remove(name);\n    }\n}\n\npub fn has_client(name: &str) -> bool {\n    CLIENTS.lock().unwrap().as_ref().is_some_and(|clients| clients.contains_key(name))\n}\n"],
      ["src/menu.rs", "use std::sync::Mutex;\n\nstatic MENU_SNAPSHOT: Mutex<Vec<String>> = Mutex::new(vec![\"github_tools.search\".to_string()]);\nstatic REFRESH_PENDING: Mutex<bool> = Mutex::new(false);\n\n/// 删除事件只安排下一 UI 帧刷新，不会同步改写当前渲染快照。\npub fn schedule_refresh() { *REFRESH_PENDING.lock().unwrap() = true; }\n\npub fn visible_tools() -> Vec<String> { MENU_SNAPSHOT.lock().unwrap().clone() }\n\npub fn flush_scheduled_refresh() {\n    if *REFRESH_PENDING.lock().unwrap() {\n        *MENU_SNAPSHOT.lock().unwrap() = Vec::new();\n    }\n}\n"],
    ]),
    "diagnostic_name_collision": new Map([
      ["TASK.md", "# 任务：诊断同名插件配置被静默忽略\n\nYAML 目录与管理数据库中存在同名插件 github_tools 时，管理员通过数据库添加返回成功、\nDB 行存在，但运行时生效的是 YAML 来源的配置，没有任何错误或日志提示冲突。\n请找到 src/ 中注册路径的具体分支，说明冲突在哪里被静默吞掉，按任务指令输出 DIAGNOSIS.json。\n"],
      ["src/loader.rs", "pub mod registry;\n\n/// 来源一：YAML 目录扫描加载。\npub fn load_yaml_dir(dir: &str) -> Vec<(String, String)> {\n    let _ = dir;\n    vec![(\"github_tools\".to_string(), \"yaml-config\".to_string())]\n}\n\n/// 来源二：管理数据库（mcp_plugins 表）加载。\npub fn load_managed_db(db: &[(String, String)]) -> Vec<(String, String)> {\n    db.to_vec()\n}\n\npub fn bootstrap(dir: &str, db: &[(String, String)]) {\n    for (name, source) in load_yaml_dir(dir) {\n        registry::insert(&name, &source);\n    }\n    for (name, source) in load_managed_db(db) {\n        registry::insert(&name, &source);\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn db_config_is_silently_ignored_when_yaml_registered_first() {\n        let db = vec![(\"github_tools\".to_string(), \"managed-db-config\".to_string())];\n        super::bootstrap(\"/etc/plugins\", &db);\n        let active = super::registry::active_config(\"github_tools\");\n        assert_eq!(active, Some(\"yaml-config\".to_string()),\n            \"DB 配置被静默忽略——这正是要诊断的现象\");\n    }\n}\n"],
      ["src/registry.rs", "use std::collections::HashMap;\nuse std::sync::Mutex;\n\nstatic ACTIVE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);\n\n/// 注册插件：同名已存在时直接返回成功，不报错、不记日志、不覆盖。\npub fn insert(name: &str, source: &str) {\n    let mut guard = ACTIVE.lock().unwrap();\n    let map = guard.get_or_insert_with(HashMap::new);\n    if map.contains_key(name) {\n        return;\n    }\n    map.insert(name.to_string(), source.to_string());\n}\n\npub fn active_config(name: &str) -> Option<String> {\n    ACTIVE.lock().unwrap().clone().unwrap_or_default().get(name).cloned()\n}\n"],
    ]),
  };
}

const hiddenAssertions = {
  "diagnostic_fake_action": (diagnosis) => {
    const actions = diagnosis.unimplemented_actions;
    assert.ok(Array.isArray(actions), "unimplemented_actions 必须是数组");
    assert.deepEqual(
      [...actions].sort(),
      ["batch_import", "toggle"],
      "被声明但未实现的 action 必须被精确识别",
    );
    const rootCause = String(diagnosis.root_cause ?? "");
    assert.ok(
      rootCause.length > 0,
      "root_cause 不得为空",
    );
    assert.ok(
      /不一致|不匹配|only|match|mismatch|声明/.test(rootCause) || /不一致|不匹配/.test(JSON.stringify(diagnosis.evidence ?? [])),
      "根因或证据必须指出声明与实现不一致",
    );
  },
  "diagnostic_plugin_removal": (diagnosis) => {
    const stale = diagnosis.stale_components;
    assert.ok(Array.isArray(stale) && stale.length > 0, "stale_components 必须非空");
    const joined = stale.join(" ").toLowerCase();
    assert.ok(
      joined.includes("registry") || joined.includes("last_known_tools") || joined.includes("clients"),
      `stale_components 必须指出 registry/clients/last_known_tools，实际：${joined}`,
    );
    const rootCause = String(diagnosis.root_cause ?? "");
    assert.ok(
      /对账|移除|reconcil|remove|同步/.test(rootCause) || /对账|移除/.test(JSON.stringify(diagnosis.evidence ?? [])),
      "根因或证据必须指出运行时移除/对账缺失",
    );
  },
  "diagnostic_false_leak": (diagnosis) => {
    const visible = diagnosis.visible_components;
    assert.ok(Array.isArray(visible) && visible.length > 0, "visible_components 必须非空");
    assert.ok(
      visible.join(" ").toLowerCase().includes("menu_snapshot")
        || visible.join(" ").toLowerCase().includes("refresh_pending"),
      "visible_components 必须识别 menu_snapshot 或 refresh_pending",
    );
    const rootCause = String(diagnosis.root_cause ?? "").toLowerCase();
    assert.match(rootCause, /snapshot|快照|刷新|refresh/, "根因必须落在菜单快照/刷新，而非泛化残留");
    const unsupportedRetentionAsCause = rootCause
      .split(/[。；;.!?]/)
      .some((clause) => /leak|泄漏|zombie|僵尸|跨会话/.test(clause)
        && !/而非|不是|并非|排除|无证据|缺乏证据|不成立|不存在|并无|并未|not|rather than|instead of/.test(clause));
    assert.equal(
      unsupportedRetentionAsCause,
      false,
      "不得把无直接证据的泄漏/zombie/跨会话残留写成根因；明确排除这些解释是允许的",
    );
    assert.equal(diagnosis.root_cause_confidence, "confirmed", "直接源码证据已足够时主根因应标为 confirmed");
    assert.ok(
      Array.isArray(diagnosis.root_cause_evidence) && diagnosis.root_cause_evidence.length > 0,
      "主根因必须有独立的 root_cause_evidence",
    );
  },
  "diagnostic_name_collision": (diagnosis) => {
    const sources = diagnosis.collision_sources;
    assert.ok(Array.isArray(sources), "collision_sources 必须是数组");
    const normalized = sources.map((item) => String(item).toLowerCase());
    assert.ok(
      normalized.some((item) => item.includes("yaml")),
      `collision_sources 必须识别 yaml 来源，实际：${normalized.join(", ")}`,
    );
    assert.ok(
      normalized.some((item) => item.includes("db") || item.includes("database") || item.includes("managed")),
      `collision_sources 必须识别 db/managed 来源，实际：${normalized.join(", ")}`,
    );
    const rootCause = String(diagnosis.root_cause ?? "");
    assert.ok(
      /静默|silent|跳过|skip|吞/.test(rootCause) || /静默|跳过/.test(JSON.stringify(diagnosis.evidence ?? [])),
      "根因必须指出静默跳过，不得只说同名冲突",
    );
  },
};

describeLive("诊断型隔离 fixture（真实模型）", function () {
  this.timeout(evaluationTimeoutMs + 60_000);

  const filesByScenario = fixtureFiles();

  for (const scenario of Object.keys(filesByScenario)) {
    describe(scenario, function () {
      this.timeout(evaluationTimeoutMs + 60_000);
      const fixtureDir = path.join(artifactDir, `agent-diag-${scenario}-${Date.now()}-${process.pid}`);
      const protectedFiles = new Map();

      before(async () => {
        mkdirSync(artifactDir, { recursive: true });
        for (const [relativePath, content] of filesByScenario[scenario]) {
          const pathname = path.join(fixtureDir, relativePath);
          mkdirSync(path.dirname(pathname), { recursive: true });
          writeFileSync(pathname, content);
          protectedFiles.set(pathname, content);
        }
        await browser.tauri.switchWindow("main");
        await browser.url("tauri://localhost/agent");
      });

      it("在隔离工作区完成诊断并通过隐藏断言", async () => {
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
            });
            done({ ok: true, result });
          } catch (error) {
            done({ ok: false, error: String(error) });
          }
        }, proModelId, flashModelId, visionModelId, routingMode, roleModels, scenario, fixtureDir);

        assert.equal(started.ok, true, started.error);
        const evaluationId = started.result.evaluation_id;
        assert.match(evaluationId, /^agent-diag-/);

        const reportPath = path.join(evaluationDir, `${evaluationId}.json`);
        await browser.waitUntil(() => existsSync(reportPath), {
          timeout: evaluationTimeoutMs,
          interval: 2_000,
          timeoutMsg: `评测 ${evaluationId} 未在时限内生成报告`,
        });

        const report = JSON.parse(readFileSync(reportPath, "utf8"));
        assert.equal(report.trace_status, "succeeded", "诊断任务必须正常收束");
        assert.equal(report.scenario_id, scenario, "报告必须保留评测场景，而不是按文件名前缀猜测");
        assert.ok(report.complex_delivery, "复杂诊断必须生成终态交付边界检查");
        assert.equal(
          report.complex_delivery.compliant,
          true,
          "最终交付必须明确披露已核验范围、未核验范围或关键假设、以及结论强度",
        );
        // 受保护文件 byte 级不变：诊断只允许写 DIAGNOSIS.json
        for (const [pathname, original] of protectedFiles) {
          assert.equal(
            readFileSync(pathname, "utf8"),
            original,
            `${path.basename(pathname)} 不应被 Agent 修改（唯一允许写入的是 DIAGNOSIS.json）`,
          );
        }

        const diagnosisPath = path.join(fixtureDir, "DIAGNOSIS.json");
        assert.ok(existsSync(diagnosisPath), "必须交付 DIAGNOSIS.json");
        const diagnosis = JSON.parse(readFileSync(diagnosisPath, "utf8"));
        assert.ok(Array.isArray(diagnosis.evidence) && diagnosis.evidence.length > 0, "evidence 必须非空");
        hiddenAssertions[scenario](diagnosis);

        writeFileSync(
          path.join(artifactDir, `diag-${scenario}-${evaluationId}.json`),
          JSON.stringify({ diagnosis, report }, null, 2),
        );
      });
    });
  }
});
