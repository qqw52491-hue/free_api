pub mod artifact_manager;
pub mod change_verification;
pub mod channel;
pub mod child_reviewer;
pub mod child_worker;
pub mod child_writer;
pub mod checkpoint;
pub mod context;
pub mod control;
pub mod conversation;
pub mod dispatcher;
pub mod evaluation;
pub mod evaluation_analysis;
pub mod evaluation_runner;
pub mod file_parser;
pub mod job_engine;
pub mod lesson;
pub mod loop_runner;
pub mod plan_gate;
pub mod call_llm_phase;
pub mod main_loop;
pub mod mcp;
pub mod mcp_catalog;
pub mod model_filter;
pub mod plan_tree;
pub mod plan_tree_dispatch;
pub mod project_map;
pub mod prompt_assembly;
pub mod retrieval;
pub mod review_contract;
pub mod reviewer_gatekeeper;
pub mod gate_messages;
pub mod finish_guard;
pub mod workspace_lock;
pub mod stage_review;
pub mod review_tools;
pub mod eval_oracle;
pub mod host_verify;
pub mod router;
pub mod run_state;
pub mod run_trace;
pub mod scheduler;
pub mod skill_context;
pub mod target_block;
pub mod task_store;
pub mod task_summary;
pub mod telemetry;
pub mod tool_manual;
pub mod tools;
pub mod types;
pub mod utils;
pub mod watch_engine;
pub mod web_search;

pub use checkpoint::*;
pub use control::*;
pub use dispatcher::*;
pub use loop_runner::*;
pub use main_loop::*;

#[cfg(test)]
mod finish_instruction_tests {
    use super::*;
    use crate::agent::mcp::PluginRegistry;
    use crate::agent::types::AgentInstruction;
    use serde_json::json;

    #[test]
    fn finish_is_recognized_from_tool_or_action_without_tool_dispatch() {
        let by_tool: AgentInstruction = serde_json::from_value(json!({
            "tool": "finish",
            "command": { "summary": "已完成" }
        }))
        .unwrap();
        let by_action: AgentInstruction = serde_json::from_value(json!({
            "tool": "core",
            "action": "finish",
            "command": { "summary": "已完成" }
        }))
        .unwrap();

        assert!(is_finish_instruction(&by_tool));
        assert!(is_finish_instruction(&by_action));
    }

    #[test]
    fn run_agent_step_short_circuits_finish_before_unknown_dispatch() {
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "tool": "finish",
            "command": { "summary": "Excel 已创建" }
        }))
        .unwrap();
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let registry = PluginRegistry::new();

        let result = run_agent_step(&db, "finish-regression", &instruction, &registry);
        assert!(result.success);
        assert_eq!(result.route, "finish");
        assert_eq!(result.stdout, "Excel 已创建");
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn test_built_in_tools_menu_contains_model_manager() {
        let candidate_dirs = [
            std::path::PathBuf::from("src-tauri/prompts/tools"),
            std::path::PathBuf::from("prompts/tools"),
            std::path::PathBuf::from("../src-tauri/prompts/tools"),
        ];

        let mut found_dir = None;
        for d in &candidate_dirs {
            if d.exists() && d.is_dir() {
                found_dir = Some(d.clone());
                break;
            }
        }

        let dir = found_dir.expect("prompts/tools directory must exist in candidate paths");
        let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = entries.flatten().collect();
            files.sort_by_key(|e| e.file_name());
            for entry in files {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md") {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let summary = content
                            .find("<summary>")
                            .and_then(|start| {
                                content
                                    .find("</summary>")
                                    .map(|end| &content[start + 9..end])
                            })
                            .unwrap_or("无详细描述");
                        local_tools_menu.push_str(&format!("- {}: {}\n", name, summary));
                    }
                }
            }
        }

        // 必须包含 model_manager, ask_human, web_search, web_fetch
        assert!(
            local_tools_menu.contains("- model_manager:"),
            "menu must contain model_manager: {}",
            local_tools_menu
        );
        assert!(
            local_tools_menu.contains("- ask_human:"),
            "menu must contain ask_human: {}",
            local_tools_menu
        );
        assert!(
            local_tools_menu.contains("- web_search:"),
            "menu must contain web_search: {}",
            local_tools_menu
        );
        assert!(
            local_tools_menu.contains("- web_fetch:"),
            "menu must contain web_fetch: {}",
            local_tools_menu
        );

        // 绝不能包含旧的虚构/残留工具
        assert!(
            !local_tools_menu.contains("code_editor"),
            "menu must not contain ghost code_editor"
        );
        assert!(
            !local_tools_menu.contains("browser_dom"),
            "menu must not contain ghost browser_dom"
        );
        assert!(
            !local_tools_menu.contains("project_analyzer"),
            "menu must not contain ghost project_analyzer"
        );
    }
}
