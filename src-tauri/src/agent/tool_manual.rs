//! 工具说明书（Tool Manual）懒加载：从 main_loop 抽出的下一轮手册注入逻辑。
//!
//! Write-Once 按需把手册追加为历史事件并去重（保护前缀缓存）；显式
//! next_tool_hint 未命中时注入可用插件名纠错反馈；命中插件但目录缓存为
//! 空/过期时现场刷新。行为与原 main_loop 内联实现完全一致。

use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::mcp::PluginRegistry;
use crate::agent::triage::TriageVerdict;
use crate::agent::types::AgentInstruction;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

pub async fn load_tool_manual_for_next_step(
    app: &AppHandle,
    registry_state: &Arc<Mutex<PluginRegistry>>,
    context: &mut SandwichContext,
    instruction: &AgentInstruction,
    workspace_root: &std::path::Path,
    db: &crate::db::DbState,
) {
    let targets = instruction.get_next_tool_hints();
    if targets.is_empty() {
        return;
    }

    let resource_dir = app.path().resource_dir().unwrap_or_default();
    let is_explicit = instruction.next_tool_hint.is_some();

    for raw_target in targets {
        // dedup/命中键使用完整目标：支持 "插件名/工具名" 粒度——
        // 大插件手册只列工具名清单，单工具 Schema 需要按完整目标取回。
        let target = raw_target.trim().to_string();
        // 文件手册/插件存在性检查仍用首段名（Skill 与插件名都不含 /）。
        let tool_key = target
            .split('/')
            .next()
            .unwrap_or(target.as_str())
            .trim()
            .to_string();
        if tool_key.is_empty() || tool_key == "finish" || tool_key == "core" {
            continue;
        }
        if context.has_tool_manual(&target) {
            continue;
        }

        load_single_tool_manual(
            &target,
            &tool_key,
            is_explicit,
            &resource_dir,
            workspace_root,
            registry_state,
            context,
            db,
        )
        .await;
    }
}

async fn load_single_tool_manual(
    target: &str,
    tool_key: &str,
    is_explicit_hint: bool,
    resource_dir: &std::path::Path,
    workspace_root: &std::path::Path,
    registry_state: &Arc<Mutex<PluginRegistry>>,
    context: &mut SandwichContext,
    db: &crate::db::DbState,
) {
    // 含 "/" 的目标是 "插件/工具" 精确粒度，必须直通 registry：
    // 若先做本地文件手册查找，恰好同名的 <插件>.md Skill 会返回
    // 文档而不是 MCP 工具 Schema（名称碰撞）。
    let is_exact_mcp_target = target.contains('/');
    let tool_filename = format!("{}.md", tool_key);
    // 与 capability_index 的 Skill 索引口径保持一致：
    // 工作区 skills/ 与应用资源 skills/ 都可被搜索到，也必须可加载。
    let candidate_paths = [
        workspace_root.join("skills").join(&tool_filename),
        std::path::PathBuf::from("src-tauri/prompts/tools").join(&tool_filename),
        std::path::PathBuf::from("prompts/tools").join(&tool_filename),
        std::path::PathBuf::from("../src-tauri/prompts/tools").join(&tool_filename),
        resource_dir.join("prompts/tools").join(&tool_filename),
        std::path::PathBuf::from("src-tauri/prompts/skills").join(&tool_filename),
        std::path::PathBuf::from("prompts/skills").join(&tool_filename),
        std::path::PathBuf::from("../src-tauri/prompts/skills").join(&tool_filename),
        resource_dir.join("prompts/skills").join(&tool_filename),
    ];

    let mut tool_detail = String::new();
    if !is_exact_mcp_target {
        for cp in &candidate_paths {
            if let Ok(content) = std::fs::read_to_string(cp) {
                tool_detail = content;
                break;
            }
        }
        // 文件手册未命中 → 兜底查 DB 自定义 Skill（kind='skill'）：
        // agent 经 skill_manager.add 自助沉淀的条目由此可被运行中
        // next_tool_hint 取回，与 triage 索引/懒加载同一命名空间。
        if tool_detail.is_empty() {
            if let Some(doc) =
                crate::agent::skill_context::db_skill_doc(db, tool_key, 20_000)
            {
                tool_detail = format!("【DB 自定义 Skill】\n{}", doc);
            }
        }
    }
    let mut hint_miss_feedback: Option<String> = None;
    if tool_detail.is_empty() {
        let is_registered_plugin = {
            let registry = registry_state.lock().await;
            registry.plugin_names().contains(&tool_key)
        };
        if is_registered_plugin {
            // 命中插件但目录缓存为空/过期：现场刷新一次再生成说明书，
            // 让懒加载在长会话中依旧有效，而不是静默返回空。
            let client = {
                let registry = registry_state.lock().await;
                registry.get(tool_key)
            };
            if let Some(client) = client {
                if crate::agent::mcp_catalog::cached_tools(&client.config)
                    .map_or(true, |tools| tools.is_empty())
                {
                    match client.list_tools().await {
                        Ok(tools) => println!(
                            "🔄 [Tool Manifest] 目录缓存为空，已现场刷新 [{}]（{} 个工具）",
                            tool_key,
                            tools.len()
                        ),
                        Err(error) => {
                            eprintln!("⚠️ [Tool Manifest] 现场刷新 [{}] 失败: {}", tool_key, error)
                        }
                    }
                }
            }
            let registry = registry_state.lock().await;
            // 完整目标直通 format_tool_detail："plugin/tool" 取单工具 Schema，
            // "plugin" 取插件级清单（内部已有超大插件截断）。
            tool_detail = registry.format_tool_detail(target);
        } else if is_explicit_hint {
            // 显式 hint 未命中任何手册/Skill/插件时必须给模型可行动的纠错事实，
            // 否则它会重复猜测不存在的名字，永远拿不到工具契约。
            let plugin_names = {
                let registry = registry_state.lock().await;
                registry.plugin_names().join(", ")
            };
            if !context
                .fetched_tools
                .contains(&format!("miss:{}", tool_key))
            {
                hint_miss_feedback = Some(format!(
                    "【next_tool_hint 未命中: {}】不存在名为「{}」的内置工具手册、Skill 或 MCP 插件。当前已连接的 MCP 插件：{}。可先用 capability_search(query: \"关键词\") 检索全部内置工具/MCP 工具/Skill/角色卡拿到确切 id，再设置 next_tool_hint 或直接按「插件名/工具名」格式调用。",
                    tool_key,
                    tool_key,
                    if plugin_names.is_empty() {
                        "（无）".to_string()
                    } else {
                        plugin_names
                    }
                ));
            }
        }
    }

    if let Some(feedback) = hint_miss_feedback {
        context.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(feedback),
        });
        context.fetched_tools.insert(format!("miss:{}", tool_key));
        println!(
            "🧭 [Tool Manifest] next_tool_hint 未命中 [{}]，已注入可用插件名纠错反馈",
            tool_key
        );
    } else if !tool_detail.trim().is_empty() {
        if tool_detail.contains("未找到工具") || tool_detail.contains("离线") || tool_detail.contains("连接失败") {
            context.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【插件不可用提示】插件/工具 [{}] 当前处于离线或连接失败状态，暂无可用工具，请勿尝试调用该插件下的动作。",
                    target
                )),
            });
            context.fetched_tools.insert(target.to_string());
            println!("⚠️ [Tool Manifest] 插件 [{}] 处于离线/连接失败状态，已注入不可用简短提示", target);
        } else {
            context.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【工具/Skill 操作手册: {}】\n{}\n(已写入历史流，后续调用可直接参考此规格)",
                    target,
                    tool_detail.trim()
                )),
            });
            context.fetched_tools.insert(target.to_string());
            println!("📖 [Tool Manifest Cache] 首次引入工具手册 [{}] 并作为事件追加至历史流 (已标记 fetched)", target);
        }
    }
}

/// Triage Skill 路由执行：按判决推荐自动预载手册——与 next_tool_hint
/// 走同一 Write-Once 历史事件 + fetched_tools 去重口径，区别只在来源
/// （triage 判决 vs 模型自荐）。加载结果回填 verdict.loaded_skills /
/// skill_load_failures，让"推荐过"与"真加载了"可审计。
pub fn apply_triage_skill_recommendations(
    context: &mut SandwichContext,
    verdict: &mut TriageVerdict,
    skills_dir: &std::path::Path,
    db: &crate::db::DbState,
) {
    const SKILL_DOC_MAX_CHARS: usize = 20_000;
    for rec in &verdict.skill_recommendations {
        let name = rec.name.trim();
        if name.is_empty() || context.has_tool_manual(name) {
            continue;
        }
        match crate::agent::skill_context::load_skill_doc_with_db(
            db,
            skills_dir,
            name,
            SKILL_DOC_MAX_CHARS,
        ) {
            Some(doc) => {
                context.turns_history.push(ChatMessage {
                    role: "user".to_string(),
                    content: json!(format!(
                        "【工具/Skill 操作手册: {}】\n{}\n(分流路由预载：{}；已写入历史流，后续可直接参考此规格)",
                        name,
                        doc.trim(),
                        if rec.reason.trim().is_empty() { "任务匹配" } else { rec.reason.trim() }
                    )),
                });
                context.fetched_tools.insert(name.to_string());
                verdict.loaded_skills.push(name.to_string());
            }
            None => verdict.skill_load_failures.push(name.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::triage::SkillRecommendation;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skills_dir() -> std::path::PathBuf {
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("tool_manual_skills_{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn triage_recommendations_load_dedup_and_report_failures() {
        let dir = temp_skills_dir();
        std::fs::write(dir.join("code_diagnosis.md"), "# 诊断手册\n先取证再改").unwrap();

        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut verdict = TriageVerdict::fallback("test");
        verdict.skill_recommendations = vec![
            SkillRecommendation { name: "code_diagnosis".into(), reason: "修复被否定".into() },
            SkillRecommendation { name: "code_diagnosis".into(), reason: "重复".into() },
            SkillRecommendation { name: "ghost_skill".into(), reason: "不存在".into() },
        ];
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_custom_instructions (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('prompt','skill')),
                name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        apply_triage_skill_recommendations(&mut ctx, &mut verdict, &dir, &db);

        assert_eq!(verdict.loaded_skills, vec!["code_diagnosis".to_string()], "存在的手册应加载且去重");
        assert_eq!(verdict.skill_load_failures, vec!["ghost_skill".to_string()], "不存在的名应记失败不静默");
        assert!(ctx.has_tool_manual("code_diagnosis"));
        let manuals = ctx.turns_history.iter().filter(|m| {
            m.content.as_str().map_or(false, |s| s.contains("【工具/Skill 操作手册: code_diagnosis】"))
        }).count();
        assert_eq!(manuals, 1, "手册事件只应注入一次");

        // 二次调用不重复注入（与 next_tool_hint 同一去重口径）
        let mut verdict2 = TriageVerdict::fallback("test2");
        verdict2.skill_recommendations = vec![SkillRecommendation { name: "code_diagnosis".into(), reason: "again".into() }];
        apply_triage_skill_recommendations(&mut ctx, &mut verdict2, &dir, &db);
        assert!(verdict2.loaded_skills.is_empty(), "已加载的 Skill 不应重复注入");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
