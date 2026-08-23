//! 工具说明书（Tool Manual）懒加载：从 main_loop 抽出的下一轮手册注入逻辑。
//!
//! Write-Once 按需把手册追加为历史事件并去重（保护前缀缓存）；显式
//! next_tool_hint 未命中时注入可用插件名纠错反馈；命中插件但目录缓存为
//! 空/过期时现场刷新。行为与原 main_loop 内联实现完全一致。

use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::mcp::PluginRegistry;
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
) {
    let next_tool = instruction
        .next_tool_hint
        .clone()
        .unwrap_or_else(|| instruction.get_action());
    let tool_key = next_tool
        .split('/')
        .next()
        .unwrap_or(&next_tool)
        .to_string();
    if tool_key.is_empty() || tool_key == "finish" || tool_key == "core" {
        return;
    }
    if context.has_tool_manual(&tool_key) {
        return;
    }

    let tool_filename = format!("{}.md", tool_key);
    let resource_dir = app.path().resource_dir().unwrap_or_default();
    let candidate_paths = [
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
    for cp in &candidate_paths {
        if let Ok(content) = std::fs::read_to_string(cp) {
            tool_detail = content;
            break;
        }
    }
    let mut hint_miss_feedback: Option<String> = None;
    if tool_detail.is_empty() {
        let is_registered_plugin = {
            let registry = registry_state.lock().await;
            registry.plugin_names().contains(&tool_key.as_str())
        };
        if is_registered_plugin {
            // 命中插件但目录缓存为空/过期：现场刷新一次再生成说明书，
            // 让懒加载在长会话中依旧有效，而不是静默返回空。
            let client = {
                let registry = registry_state.lock().await;
                registry.get(&tool_key)
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
                        Err(error) => eprintln!(
                            "⚠️ [Tool Manifest] 现场刷新 [{}] 失败: {}",
                            tool_key, error
                        ),
                    }
                }
            }
            let registry = registry_state.lock().await;
            tool_detail = registry.format_tool_detail(&tool_key);
        } else if instruction.next_tool_hint.is_some() {
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
                    "【next_tool_hint 未命中: {}】不存在名为「{}」的内置工具手册、Skill 或 MCP 插件。当前已连接的 MCP 插件：{}。请改用上述确切插件名重新设置 next_tool_hint，或直接按 MCP 工具目录中的「插件名/工具名」格式调用。",
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
        context
            .fetched_tools
            .insert(format!("miss:{}", tool_key));
        println!("🧭 [Tool Manifest] next_tool_hint 未命中 [{}]，已注入可用插件名纠错反馈", tool_key);
    } else if !tool_detail.trim().is_empty() {
        context.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【工具/Skill 操作手册: {}】\n{}\n(已写入历史流，后续调用可直接参考此规格)",
                tool_key,
                tool_detail.trim()
            )),
        });
        context.fetched_tools.insert(tool_key.clone());
        println!("📖 [Tool Manifest Cache] 首次引入工具手册 [{}] 并作为事件追加至历史流 (已标记 fetched)", tool_key);
    }
}
