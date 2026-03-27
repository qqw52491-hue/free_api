pub mod types;
pub mod utils;
pub mod browser;
pub mod mcp;
pub mod context;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::{sleep, Duration};
use crate::db::DbState;
use crate::agent::types::*;
use crate::agent::utils::*;
use crate::agent::browser::*;
use crate::agent::mcp::*;

const BUILTIN_BROWSER_ACTIONS: &[&str] = &[
    "navigate", "goto", "extract", "look", "click", "type", "press", "scroll", "hover", "read",
];

pub fn run_agent_step(
    instruction: &AgentInstruction,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let action = instruction.action.trim().to_lowercase();

    if BUILTIN_BROWSER_ACTIONS.contains(&action.as_str()) {
        let cmd_str = if let Some(s) = instruction.params.get("command").and_then(|v| v.as_str()) {
            s.to_string()
        } else {
            instruction.params.to_string()
        };
        let full_cmd = if cmd_str.is_empty() { action.clone() } else { format!("{} {}", action, cmd_str) };
        let (stdout, stderr, success) = run_browser_dom(&full_cmd);
        return DispatchResult { stdout, stderr, success, route: "browser".to_string() };
    }

    let (plugin_name, tool_name) = if let Some(pos) = instruction.action.find('/') {
        (&instruction.action[..pos], &instruction.action[pos + 1..])
    } else {
        let tool = instruction.params.get("tool").and_then(|v| v.as_str()).unwrap_or(&instruction.action);
        (&instruction.action as &str, tool)
    };

    if let Some(client) = registry.get_mut(plugin_name) {
        let arguments = instruction.params.clone();
        match client.call_tool(tool_name, arguments) {
            Ok(result) => {
                let stdout = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                DispatchResult { stdout, stderr: String::new(), success: true, route: format!("mcp:{}", plugin_name) }
            }
            Err(e) => DispatchResult { stdout: String::new(), stderr: e, success: false, route: format!("mcp:{}", plugin_name) },
        }
    } else {
        DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 未知 action='{}'", instruction.action),
            success: false,
            route: "unknown".to_string(),
        }
    }
}

#[tauri::command]
pub async fn dispatch_agent_step(instruction_json: String) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction = serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;
    let mut registry = PluginRegistry::new();
    let result = tokio::task::spawn_blocking(move || run_agent_step(&instruction, &mut registry)).await.map_err(|e| e.to_string())?;
    Ok(json!({ "route": result.route, "success": result.success, "stdout": result.stdout, "stderr": result.stderr }))
}

#[tauri::command]
pub async fn execute_command(tool: String, command: String) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" { run_osascript(&command) } else { run_shell(&command) };
    Ok(json!({ "success": success, "stdout": stdout.trim(), "stderr": stderr.trim() }))
}

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    model_id: String,
    goal: String,
    auto_pilot: bool,
) -> Result<(), String> {
    // 1. 加载所有 MCP 插件
    let mut registry = PluginRegistry::load_from_dir(
        &app.path().app_config_dir().unwrap_or_default().join("plugins")
    );

    // 2. 初始化三明治上下文
    let system_prompt = "你是一个极简、彻底解耦的 Agent 核心。通过内置 Browser 或 MCP 工具解决问题。".to_string();
    let mut context = context::SandwichContext::new(system_prompt, goal);

    // 3. 构建 HTTP 客户端（对接 LLM）
    // ... 此处省略获取 API Key 的数据库读取（保持原 logic）...
    let api_key = "sk-..."; // 示意
    let client = reqwest::Client::new();

    let mut step_id = 1;
    loop {
        if step_id > 20 { break; }

        // --- A. AI 规划阶段 ---
        let messages = context.assemble_messages();
        // 请求 LLM 获取 AgentInstruction JSON (简化示意)
        // let instruction: AgentInstruction = call_llm(messages).await?;
        
        // 模拟一条指令进行演示
        let instruction = AgentInstruction {
            thought: "我需要通过浏览器查看当前页面状态".to_string(),
            action: "extract".to_string(), // 内置动作
            params: serde_json::Value::Object(Default::default()),
            todo_update: vec![]
        };

        // --- B. 执行路由分发 ---
        let result = tokio::task::spawn_blocking(move || {
             // 此处需注意：registry 不能被简单 move 进入，实际中应存入持久 State 或跨线程访问
             // 这里仅展示解耦后的调用逻辑
             run_agent_step(&instruction, &mut registry)
        }).await.map_err(|e| e.to_string())?;

        // --- C. 更新记忆与观测 ---
        context.push_memory(ShortMemory {
            step_id,
            tool: instruction.action.clone(),
            command: instruction.params.to_string(),
            output_summary: chars_preview(&result.stdout, 500),
            success: result.success,
        });
        context.update_observation(result.stdout);

        // --- D. 任务完成判定 ---
        if instruction.action == "finish" { break; }
        
        step_id += 1;
        sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}
