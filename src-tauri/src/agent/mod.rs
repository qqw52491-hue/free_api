pub mod browser;
pub mod builtins;
pub mod context;
pub mod mcp;
pub mod types;
pub mod utils;

use crate::agent::browser::*;
use crate::agent::builtins::*;
use crate::agent::mcp::*;
use crate::agent::types::*;
use crate::agent::utils::*;
use crate::db::DbState;
use rusqlite::params;
use serde_json::json;
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::time::{sleep, Duration};

// 根据指令执行具体动作
pub fn run_agent_step(
    instruction: &AgentInstruction,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let action = instruction.get_action().trim().to_lowercase();
    let params = instruction.get_params();

    // 1. 优先尝试本地内置工具
    if let Some(res) = run_builtin_step(&action, &params) {
        return res;
    }

    // 2. 尝试分发到 MCP 插件
    let (plugin_name, tool_name) = if let Some(pos) = action.find('/') {
        (&action[..pos], &action[pos + 1..])
    } else {
        let tool = params
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or(&action);
        (&action as &str, tool)
    };

    if let Some(client) = registry.get_mut(plugin_name) {
        // --- 参数预处理：确保 MCP 拿到的是 Object ---
        let arguments = if let Some(s) = params.as_str() {
            // 如果是字符串，尝试解析成 JSON 对象
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
                if val.is_object() { val } else { 
                    return DispatchResult { stdout: String::new(), stderr: format!("❌ MCP 预检失败: 参数必须是对象，不能是基本类型. 收到: {}", s), success: false, route: "mcp_check".to_string() };
                }
            } else {
                return DispatchResult { stdout: String::new(), stderr: format!("❌ MCP 预检失败: AI 发送的是普通字符串 '{}', 但 MCP 工具 '{}' 需要 JSON 对象参数. 请参考说明书中的【参数规格】。", s, tool_name), success: false, route: "mcp_check".to_string() };
            }
        } else if params.is_object() {
            params.clone()
        } else {
            json!({})
        };

        println!("🛠️ 正在调用 MCP 插件 [{}], 工具 [{}], 参数: {}", plugin_name, tool_name, arguments);
        match client.call_tool(tool_name, arguments) {
            Ok(result) => {
                let stdout =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: format!("mcp:{}", plugin_name),
                }
            }
            Err(e) => DispatchResult {
                stdout: String::new(),
                stderr: e,
                success: false,
                route: format!("mcp:{}", plugin_name),
            },
        }
    } else {
        // --- 3. 实在没辙了 ---
        DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 未知 action='{}', params={:?}", action, instruction.params),
            success: false,
            route: "unknown".to_string(),
        }
    }
}

#[tauri::command]
pub async fn dispatch_agent_step(
    instruction_json: String,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction =
        serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;

    // 克隆 Arc 以满足 'static 约束
    let registry_arc = registry_state.inner().clone();

    let result = tokio::task::spawn_blocking(move || {
        // 在阻塞线程内部加锁
        let mut registry = futures::executor::block_on(registry_arc.lock());
        run_agent_step(&instruction, &mut registry)
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(
        json!({ "route": result.route, "success": result.success, "stdout": result.stdout, "stderr": result.stderr }),
    )
}

#[tauri::command]
pub async fn execute_command(tool: String, command: String) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" {
        run_osascript(&command)
    } else {
        run_shell(&command)
    };
    Ok(json!({ "success": success, "stdout": stdout.trim(), "stderr": stderr.trim() }))
}

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_id: String,
    goal: String,
    auto_pilot: bool,
) -> Result<(), String> {
    // 1. 自动加载插件（多路探测：系统配置、项目根目录、src-tauri 目录）
    {
        let mut registry = registry_state.lock().await;
        if registry.plugin_names().is_empty() {
            let mut all_clients = std::collections::HashMap::new();
            
            // 待扫描的目录列表
            let mut search_paths = vec![
                app.path().app_config_dir().unwrap_or_default().join("plugins"), // 系统路径
                std::env::current_dir().unwrap_or_default().join("plugins"),     // 当前路径/plugins
                std::env::current_dir().unwrap_or_default().join("../plugins"),  // 如果在 src-tauri 里，搜根目录/plugins
            ];

            for path in search_paths {
                if path.exists() {
                    println!("📂 正在扫描插件目录: {}", path.display());
                    let reg = PluginRegistry::load_from_dir(&path);
                    for (name, client) in reg.clients {
                        all_clients.insert(name, client);
                    }
                }
            }
            registry.clients = all_clients;
        }
    }

    // 2. 初始化三明治上下文：加载系统提示词 + 动态注入 MCP 工具
    let prompt_path = app.path().resource_dir()
        .unwrap_or_default()
        .join("prompts/agent_system_prompt.md");
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("prompts/agent_system_prompt.md"))
        .unwrap_or_else(|_| "你是一个 macOS 自动化特工。每轮只返回1个JSON动作：{\"thought\":\"...\",\"description\":\"...\",\"tool\":\"browser_dom|shell|osascript|finish\",\"command\":\"...\"}".to_string());

    // 查询所有 MCP Server 的工具列表，动态附加到提示词
    let mcp_tools_section = {
        let mut registry = registry_state.lock().await;
        registry.format_tools_for_prompt()
    };
    let system_prompt = format!("{}{}", base_prompt, mcp_tools_section);
    println!("--- 最终系统提示词 (含 MCP) ---\n{}\n------------------", system_prompt);

    let mut context = context::SandwichContext::new(system_prompt, goal);

    let mut step_id = 0;
    loop {
        if step_id >= 50 {
            app.emit("agent-progress", json!({
                "type": "error",
                "message": "达到最大步数限制"
            })).map_err(|e| e.to_string())?;
            break;
        }

        // --- A. AI 规划阶段 ---
        app.emit("agent-progress", json!({
            "type": "planning",
            "message": format!("正在规划第 {} 步...", step_id + 1)
        })).map_err(|e| e.to_string())?;

        let _messages = context.assemble_messages(); // 仅用于调试引用
        // 请求 LLM 获取 AgentInstruction JSON
        let instruction: AgentInstruction = call_llm(&context, &state, model_id.clone()).await?;

        // --- B. 更新任务规划 + 核心记忆 ---
        if !instruction.todo_update.is_empty() {
            context.todo_list = instruction.todo_update.clone();
        }
        if !instruction.memories_update.is_empty() {
            context.update_memories(instruction.memories_update.clone());
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }

        // 推送整个三明治状态（包含最新的 todo_list 和 memory）
        app.emit("agent-context", &context).map_err(|e| e.to_string())?;

        // 通知前端新步骤产生
        app.emit("agent-progress", json!({
            "type": "step_new",
            "step": {
                "id": step_id,
                "description": instruction.description.clone(),
                "thought": instruction.thought.clone(),
                "tool": instruction.get_action(),
                "command": instruction.get_params().to_string(),
                "status": "pending",
                "output": ""
            }
        })).map_err(|e| e.to_string())?;

        // --- C. 执行路由分发 ---
        app.emit("agent-progress", json!({
            "type": "step_start",
            "step_id": step_id,
            "description": &instruction.description
        })).map_err(|e| e.to_string())?;

        let result = {
            let instruction_c = instruction.clone();
            let registry_arc = registry_state.inner().clone();
            tokio::task::spawn_blocking(move || {
                // 在阻塞线程中同步锁定进行分发
                let mut registry = futures::executor::block_on(registry_arc.lock());
                run_agent_step(&instruction_c, &mut registry)
            })
            .await
            .map_err(|e| e.to_string())?
        };

        // --- D. 更新记忆与观测 ---
        context.push_memory(ShortMemory {
            step_id: step_id + 1,
            tool: instruction.get_action(),
            command: instruction.get_params().to_string(),
            output_summary: chars_preview(&result.stdout, 500),
            success: result.success,
        });
        context.update_observation(result.stdout.clone());

        // 通知前端步骤完成
        if result.success {
            app.emit("agent-progress", json!({
                "type": "step_done",
                "step_id": step_id,
                "output": result.stdout
            })).map_err(|e| e.to_string())?;
        } else {
            app.emit("agent-progress", json!({
                "type": "step_error",
                "step_id": step_id,
                "output": format!("{} \n {}", result.stderr, result.stdout)
            })).map_err(|e| e.to_string())?;
        }

        // --- D. 任务完成判定 ---
        if instruction.get_action() == "finish" {
            app.emit("agent-progress", json!({
                "type": "complete",
                "message": "任务已由 AI 标记完成",
                "success": true
            })).map_err(|e| e.to_string())?;
            break;
        }

        step_id += 1;
        sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}
