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

/// 前端调用：设置浏览器启动模式
/// mode: 0=临时(默认), 1=持久化(保留Cookie), 2=连接已有Chrome
#[tauri::command]
pub fn set_browser_launch_mode(mode: u8) -> Result<String, String> {
    let desc = match mode {
        0 => "临时模式（每次干净 profile）",
        1 => "持久化模式（保留 Cookie/登录态）",
        2 => "连接模式（接管已打开的 Chrome:9222）",
        _ => return Err("无效模式，请使用 0/1/2".to_string()),
    };
    crate::agent::browser::set_browser_mode(mode);
    Ok(format!("浏览器已切换为: {}", desc))
}

// 根据指令执行具体动作
    pub fn run_agent_step(
        instruction: &AgentInstruction,
        registry: &mut PluginRegistry,
    ) -> DispatchResult {
        let mut action = instruction.get_action().trim().to_lowercase();
        let params = instruction.get_params();

        // --- 容错路由：如果 action 直接就是一个 http 地址，自动补全为 goto ---
        if action.starts_with("http://") || action.starts_with("https://") {
            action = format!("goto {}", action);
        }

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
        .join("prompts/core_logic.md");
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| "你是一个全自动 Web Agent。".to_string());

    // 2. 动态扫描所有内置工具 (Built-in Tools)
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    let tools_dir = app.path().resource_dir().unwrap_or_default().join("prompts/tools");
    let tools_dir_dev = std::path::PathBuf::from("prompts/tools");
    
    let entries = std::fs::read_dir(&tools_dir)
        .or_else(|_| std::fs::read_dir(&tools_dir_dev))
        .unwrap_or_else(|_| return std::fs::read_dir(".").unwrap()); // 容错处理

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Ok(content) = std::fs::read_to_string(&path) {
                // 抓取简短的 summary 标签内容
                let summary = content.find("<summary>")
                    .and_then(|start| content.find("</summary>").map(|end| &content[start+9..end]))
                    .unwrap_or("无详细描述");
                local_tools_menu.push_str(&format!("- {}: {}\n", name, summary));
            }
        }
    }

    // 3. 查询所有 MCP Server 的工具列表，追加到菜单里
    let mcp_tools_menu = {
        let mut registry = registry_state.lock().await;
        registry.format_tools_menu()
    };
    
    let system_prompt = format!("{}\n{}\n{}", base_prompt, local_tools_menu, mcp_tools_menu);
    println!("--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------", system_prompt);
    let mut context = context::SandwichContext::new(system_prompt, goal);

    for step_id in 0..50 {
        app.emit("agent-log", format!("正在规划第 {} 步...", step_id + 1)).map_err(|e| e.to_string())?;
        
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // ================================================================
        let mut retry_count = 0;
        let max_retries = 3;
        
        let (instruction, result) = loop {
            // --- A. 请求 AI 规划 ---
            let inst = match call_llm(&context, &state, model_id.clone()).await {
                Ok(inst) => inst,
                Err(e) => {
                    retry_count += 1;
                    if retry_count >= max_retries {
                        return Err(format!("AI 无法解析 JSON: {}", e));
                    }
                    app.emit("agent-log", format!("🔄 格式错误(第{}次)，正在自我修复...", retry_count)).map_err(|er| er.to_string())?;
                    context.add_error_feedback(&e);
                    continue;
                }
            };

            // 通知前端：发现了新计划
            app.emit("agent-log", format!("🤖 AI 规划了新动作: {}", inst.description)).map_err(|e| e.to_string())?;
            app.emit("agent-progress", json!({
                "type": "step_new",
                "step": {
                    "id": step_id,
                    "description": inst.description.clone(),
                    "thought": inst.thought.clone(),
                    "tool": inst.get_action(),
                    "command": inst.get_params().to_string(),
                    "status": "pending",
                    "output": ""
                }
            })).map_err(|e| e.to_string())?;

            // --- B. 执行动作 ---
            app.emit("agent-log", format!("▶ 步骤 {}: {}", step_id+1, inst.description)).map_err(|e| e.to_string())?;
            app.emit("agent-progress", json!({
                "type": "step_start",
                "step_id": step_id,
                "description": &inst.description
            })).map_err(|e| e.to_string())?;

            let dispatch_result = {
                let inst_c = inst.clone();
                let registry_arc = registry_state.inner().clone();
                tokio::task::spawn_blocking(move || {
                    let mut registry = futures::executor::block_on(registry_arc.lock());
                    run_agent_step(&inst_c, &mut registry)
                })
                .await
                .map_err(|e| e.to_string())?
            };

            // --- C. 判断执行结果 ---
            if dispatch_result.success || inst.get_action() == "finish" {
                break (inst, dispatch_result); // 成功！
            } else {
                retry_count += 1;
                let error_detail = format!("❌ 执行失败: {}", dispatch_result.stderr);
                
                if retry_count >= max_retries {
                    app.emit("agent-log", format!("⚠️ 重试 {} 次均失败，跳过", max_retries)).map_err(|e| e.to_string())?;
                    break (inst, dispatch_result); 
                }
                
                app.emit("agent-log", format!("🔄 执行错误 (第 {} 次)，正在重新规划...", retry_count)).map_err(|e| e.to_string())?;
                context.add_error_feedback(&error_detail);
            }
        };

        // ================================================================
        // 以下是成功跳出重试循环后的正常流程
        // ================================================================

        // --- 更新任务规划 + 核心记忆 ---
        if !instruction.todo_update.is_empty() {
            context.todo_list = instruction.todo_update.clone();
        }
        if !instruction.memories_update.is_empty() {
            context.update_memories(instruction.memories_update.clone());
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }

        // --- 核心增强：Token 优化与冷存储检索 ---
        // AI 在本轮请求 "require_memory: true"，下一轮组装消息时就会塞入全部 Fact 内容
        context.carry_memories = instruction.require_memory.unwrap_or(false);

        // --- 预加载下一轮工具说明书 ---
        let next_tool = instruction.next_tool_hint.clone()
            .unwrap_or_else(|| instruction.get_action());
        context.active_tool = Some(next_tool.clone());

        let tool_filename = format!("{}.md", next_tool);
        let resource_dir = app.path().resource_dir().unwrap_or_default();
        let tool_path_production = resource_dir.join("prompts/tools").join(&tool_filename);
        let tool_path_dev = std::path::PathBuf::from("prompts/tools").join(&tool_filename);

        if let Ok(tool_md) = std::fs::read_to_string(&tool_path_production)
            .or_else(|_| std::fs::read_to_string(&tool_path_dev)) 
        {
            context.active_tool_detail = tool_md;
        } else if next_tool.contains('/') {
            let plugin_name = next_tool.split('/').next().unwrap_or("");
            let detail = {
                let mut registry = registry_state.lock().await;
                registry.format_tool_detail(plugin_name)
            };
            context.active_tool_detail = detail;
        } else {
            context.active_tool_detail.clear();
        }

        // --- 核心增强：检查是否有截图反馈需要注入多模态消息 ---
        let mut final_stdout = result.stdout.clone();
        if result.success && final_stdout.contains("[Screenshot Saved as Base64]: ") {
            if let Some(pos) = final_stdout.find("[Screenshot Saved as Base64]: ") {
                let base64_img = &final_stdout[pos + 30..].trim();
                let log_text = &final_stdout[..pos].trim();
                
                // 1. 发送图片反馈给 AI
                context.add_image_feedback(
                    &format!("【截图回退】DOM 提取发现异常，已自动生成截图供你参考分析：\n{}", log_text), 
                    base64_img
                );
                
                // 2. 清理 stdout，只保留日志文字供状态显示
                final_stdout = format!("{}\n(已作为图片附件发送给 AI)", log_text);
            }
        }

        // --- 记录历史 + 更新观测 ---
        let output_text = if result.success {
            final_stdout.clone()
        } else {
            format!("❌ {}\n{}", result.stderr, final_stdout)
        };
        
        // 如果是普通的文本，才走 add_step (常规历史记录)
        // 注意：add_image_feedback 内部已经 push 了一条 history，所以这里加个判断
        if !result.stdout.contains("[Screenshot Saved as Base64]: ") {
            context.add_step(&instruction, &output_text);
        }
        
        context.current_observation = output_text.clone();

        // --- 前端通知 ---
        app.emit("agent-context", &context).map_err(|e| e.to_string())?;
        app.emit("agent-progress", json!({
            "type": if result.success { "step_done" } else { "step_error" },
            "step_id": step_id,
            "description": instruction.description,
            "output": output_text
        })).map_err(|e| e.to_string())?;

        // --- 任务完成判定 ---
        if instruction.get_action() == "finish" {
            app.emit("agent-progress", json!({
                "type": "complete",
                "message": "任务已由 AI 标记完成",
                "success": true
            })).map_err(|e| e.to_string())?;
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}
