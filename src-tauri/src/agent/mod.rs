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
use std::sync::atomic::Ordering;
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::time::{sleep, Duration};

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 50;

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

/// 执行单条指令的内部路由逻辑
fn execute_command_inner(
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let mut final_action = action.to_string();

    // --- 兼容性修复：如果 action 为空但 params 里有参数，尝试从 params 里捞 action ---
    if final_action.is_empty() {
        if let Some(a) = params.get("action").and_then(|v| v.as_str()) {
            final_action = a.to_string();
        } else if let Some(a) = params.get("command").and_then(|v| v.as_str()) {
            final_action = a.to_string();
        }
    }

    // --- 容错路由：如果 action 直接就是一个 http 地址，自动补全为 goto ---
    if final_action.starts_with("http://") || final_action.starts_with("https://") {
        final_action = format!("goto {}", final_action);
    }

    // 1. 优先处理 finish 特权指令 (防止模型在 finish command 里塞 action: complete 导致报错)
    if tool == "finish" || action == "finish" {
        return DispatchResult {
            stdout: "任务已由 AI 标记完成".to_string(),
            stderr: String::new(),
            success: true,
            route: "agent".to_string(),
        };
    }

    // 2. 优先尝试本地内置工具
    if let Some(res) = run_builtin_step(session_id, &final_action, params) {
        println!("执行动作调用本地内置工具: {}", final_action);
        return res;
    }

    // 2. 尝试从注册表加载外部插件
    let (plugin_name, tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), final_action.to_string())
    };

    if let Some(plugin) = registry.get_mut(&plugin_name) {
        println!("执行动作调用外部插件: {}/{}", plugin_name, tool_name);
        match plugin.call_tool(&tool_name, params.clone()) {
            Ok(out) => {
                let is_error = out.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                let content = out
                    .get("content")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(0))
                    .and_then(|m| m.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if is_error {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: content,
                        success: false,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                } else {
                    return DispatchResult {
                        stdout: content,
                        stderr: String::new(),
                        success: true,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                }
            }
            Err(e) => {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("插件调用异常: {:?}", e),
                    success: false,
                    route: format!("plugin:{}/{}", plugin_name, tool_name),
                };
            }
        }
    }

    // 专项检测：model 幻觉成了内置函数调用格式（tool_name / tool_input）
    let is_function_calling_hallucination = params.get("tool_name").is_some()
        || params.get("tool_input").is_some()
        || params.get("function_call").is_some()
        || final_action == "none"
        || final_action.is_empty();

    if is_function_calling_hallucination {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ 【幻觉纠正】你输出了错误的函数调用格式（tool_name/tool_input）！\n\
                这不是我们的协议。正确格式必须是：\n\
                {{\"tool\": \"browser_dom\", \"command\": {{\"action\": \"具体动作\"}}}}\n\
                或者任务完成时：{{\"tool\": \"finish\", \"command\": {{}}}}\n\
                绝对禁止输出 tool_name / tool_input / function_call 等字段！"
            ),
            success: false,
            route: "hallucination_detected".to_string(),
        };
    }

    DispatchResult {
        stdout: String::new(),
        stderr: format!("❌ 未知 action='{}', params={:?}", final_action, params),
        success: false,
        route: "unknown".to_string(),
    }
}

// 根据指令执行具体动作
pub fn run_agent_step(
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let tool_name = instruction.get_tool().to_lowercase();
    
    // --- 场景 A: 组合指令流水线 Batch Execution ---
    if !instruction.commands.is_empty() {
        println!("🚀 [Pipeline: {}] 正在按顺序执行 {} 个组合动作...", session_id, instruction.commands.len());
        let mut total_stdout = String::new();
        
        for (idx, cmd_val) in instruction.commands.iter().enumerate() {
            let step_idx = idx + 1;
            // 提取单步的 action
            let step_action = cmd_val.get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            
            println!("   ➡️  Step {}: 执行动作 '{}'...", step_idx, step_action);
            
            // 执行单步
            let res = execute_command_inner(session_id, &tool_name, &step_action, cmd_val, registry);
            
            // 拼接输出结果
            if !res.stdout.is_empty() {
                total_stdout.push_str(&format!("\n[Step {} Result]: {}", step_idx, res.stdout));
            }
            
            // --- 核心修复：Fail-Fast 快速失败机制 ---
            if !res.success {
                println!("   🚫 Step {} 执行失败，流水线立即熔断！原因: {}", step_idx, res.stderr);
                return DispatchResult {
                    stdout: total_stdout,
                    stderr: format!("流水线在第 {} 步崩溃: {}", step_idx, res.stderr),
                    success: false,
                    route: format!("pipeline_fail_at_{}", step_idx),
                };
            }
        }
        
        return DispatchResult {
            stdout: total_stdout,
            stderr: String::new(),
            success: true,
            route: "pipeline_success".to_string(),
        };
    }

    // --- 场景 B: 传统单步指令 Single Execution ---
    let mut action = instruction.get_action().trim().to_lowercase();
    
    // --- 幻觉纠偏：如果 action 为空但 extra_fields 里有 tool_name，尝试打捞 ---
    if action.is_empty() {
        if let Some(name) = instruction.extra_fields.get("tool_name").and_then(|v| v.as_str()) {
            action = name.to_lowercase();
        }
    }

    let params = instruction.get_params();
    execute_command_inner(session_id, &tool_name, &action, &params, registry)
}




#[tauri::command]
pub async fn dispatch_agent_step(
    instruction_json: String,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction =
        serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;

    let registry_arc = registry_state.inner().clone();
    let sid = session_id.unwrap_or_else(|| "default".to_string());

    let result = tokio::task::spawn_blocking(move || {
        // 在阻塞线程内部加锁
        let mut registry = futures::executor::block_on(registry_arc.lock());
        run_agent_step(&sid, &instruction, &mut registry)
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
    session_id: Option<String>,
) -> Result<(), String> {
    let final_session_id = session_id.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });
    // 1. 自动加载插件（多路探测：系统配置、项目根目录、src-tauri 目录）
    {
        let mut registry = registry_state.lock().await;
        if registry.plugin_names().is_empty() {
            let mut all_clients = std::collections::HashMap::new();

            // 待扫描的目录列表
            let mut search_paths = vec![
                app.path()
                    .app_config_dir()
                    .unwrap_or_default()
                    .join("plugins"), // 系统路径
                std::env::current_dir().unwrap_or_default().join("plugins"), // 当前路径/plugins
                std::env::current_dir()
                    .unwrap_or_default()
                    .join("../plugins"), // 如果在 src-tauri 里，搜根目录/plugins
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
    let prompt_path = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/core_logic.md");
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| "你是一个全自动 Web Agent。".to_string());

    // 2. 动态扫描所有内置工具 (Built-in Tools)
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    let tools_dir = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/tools");
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

    // 3. 查询所有 MCP Server 的工具列表，追加到菜单里
    let mcp_tools_menu = {
        let mut registry = registry_state.lock().await;
        registry.format_tools_menu()
    };

    let system_prompt = format!("{}\n{}\n{}", base_prompt, local_tools_menu, mcp_tools_menu);
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    let mut context = context::SandwichContext::new(system_prompt, goal.clone());

    for step_id in 0..MAX_STEPS {
        app.emit("agent-log", format!("正在规划第 {} 步...", step_id + 1))
            .map_err(|e| e.to_string())?;

        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // ================================================================
        let mut retry_count = 0;
        let mut tool_retry_count = 0;
        let mut max_retries = 3;
        let mut pre_computed_inst: Option<crate::agent::types::AgentInstruction> = None;

        let (instruction, result) = loop {
            // ================================================================
            // 🔥 [Kimi 专家直达快线]：如果上一步已经拿到了 Kimi 的指令，直接跳过整个规划循环
            // ================================================================
            if let Some(inst) = pre_computed_inst.take() {
                app.emit(
                    "agent-log",
                    "🚀 Kimi 专家指令已就绪，跳过本地 AI 思考，直接执行！",
                )
                .map_err(|e| e.to_string())?;

                // 构造一个空的 Token 使用统计
                let token_usage = crate::agent::types::TokenUsage::new(0, 0, 0);

                // 直接跳到对应的执行逻辑中
                let dispatch_result = {
                    let inst_c = inst.clone();
                    let registry_arc = registry_state.inner().clone();
                    let sid = final_session_id.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut registry = futures::executor::block_on(registry_arc.lock());
                        println!("⚡ 专家指令直接执行: {:?}", inst_c);
                        run_agent_step(&sid, &inst_c, &mut registry)
                    })
                    .await
                    .map_err(|e| e.to_string())?
                };

                // 执行完后，把指令赋给 instruction 变量，让后面的上下文更新逻辑继续执行
                break (inst, dispatch_result);
            }

            // --- A. 请求 AI 规划 ---
            let (inst, token_usage, thinking_text) =
                match call_llm(&context, &state, model_id.clone(), Some(&app), step_id).await {
                    Ok(r) => r,
                    Err(e) => {
                        retry_count += 1;
                        if retry_count >= max_retries {
                            return Err(format!("AI 无法解析 JSON: {}", e));
                        }
                        app.emit(
                            "agent-log",
                            format!("🔄 格式错误(第{}次)，正在自我修复...", retry_count),
                        )
                        .map_err(|er| er.to_string())?;
                        context.add_error_feedback(&e);
                        continue;
                    }
                };

            // 📊 发送 Token 用量统计到前端
            app.emit(
                "agent-progress",
                json!({
                    "type": "token_usage",
                    "step_id": step_id,
                    "prompt_tokens": token_usage.prompt_tokens,
                    "completion_tokens": token_usage.completion_tokens,
                    "total_tokens": token_usage.total_tokens,
                    "context_window": token_usage.context_window,
                    "usage_percent": token_usage.usage_percent,
                    "summary": token_usage.summary()
                }),
            )
            .map_err(|e| e.to_string())?;
            println!("{}", token_usage.summary());

            // 🧠 发送最终思考完成事件到前端
            if !thinking_text.is_empty() {
                app.emit(
                    "agent-progress",
                    json!({
                        "type": "thinking",
                        "step_id": step_id,
                        "content": &thinking_text,
                        "done": true
                    }),
                )
                .map_err(|e| e.to_string())?;
                app.emit(
                    "agent-log",
                    format!("🧠 AI思考完毕 (共{}字)", thinking_text.len()),
                )
                .map_err(|e| e.to_string())?;
            }

            // 通知前端：发现了新计划（含反思）
            if !inst.reflection.is_empty() {
                app.emit(
                    "agent-log",
                    format!("🪞 AI 反思: {}", inst.reflection),
                )
                .map_err(|e| e.to_string())?;
            }
            app.emit(
                "agent-log",
                format!("🤖 AI 规划了新动作: {}", inst.description),
            )
            .map_err(|e| e.to_string())?;
            app.emit(
                "agent-progress",
                json!({
                    "type": "step_new",
                    "step": {
                        "id": step_id,
                        "description": inst.description.clone(),
                        "reflection": inst.reflection.clone(),
                        "thought": inst.thought.clone(),
                        "tool": inst.get_action(),
                        "command": inst.get_params().to_string(),
                        "status": "pending",
                        "output": ""
                    }
                }),
            )
            .map_err(|e| e.to_string())?;

            // --- B. 执行动作 ---
            app.emit(
                "agent-log",
                format!("▶ 步骤 {}: {}", step_id + 1, inst.description),
            )
            .map_err(|e| e.to_string())?;
            app.emit(
                "agent-progress",
                json!({
                    "type": "step_start",
                    "step_id": step_id,
                    "description": &inst.description
                }),
            )
            .map_err(|e| e.to_string())?;

            // 具体和本地工具交互
            let dispatch_result = {
                let inst_c = inst.clone();
                let registry_arc = registry_state.inner().clone();
                let sid = final_session_id.clone();
                tokio::task::spawn_blocking(move || {
                    let mut registry = futures::executor::block_on(registry_arc.lock());
                    println!("执行动作: {:?}", inst_c);
                    run_agent_step(&sid, &inst_c, &mut registry)
                })
                .await
                .map_err(|e| e.to_string())?
            };

            // --- C. 判断执行结果 ---
            if dispatch_result.success || inst.get_action() == "finish" {
                tool_retry_count = 0; // 成功后重置计数器
                break (inst, dispatch_result); // 成功！
            } else {
                retry_count += 1;
                tool_retry_count += 1;
                let error_detail = format!("【❌ 执行失败】: {}", dispatch_result.stderr);

                if tool_retry_count == 1 {
                    let _ = app.emit(
                        "agent-log",
                        "🚨 检测到指令执行失败，正在唤起 Kimi 网页专家护驾...",
                    );
                    println!("🚨 [Rescue] 触发 Kimi 网页专家救援模式 (Session: {})...", final_session_id);

                    // 记录报错时的场景上下文，以便 Kimi 协助分析
                    // 注意：现在不需要手动记录 original_tab_id，因为动作默认就在 "main" 执行
                    let _original_id = "main";

                    let mut recent_context = String::new();
                    for msg in context.turns_history.iter().rev().take(5).rev() {
                        let content_str = match &msg.content {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        recent_context
                            .push_str(&format!("角色: {}\n内容: {}\n---\n", msg.role, content_str));
                    }

                    // 从文件加载 rescue 提示词模板，并做变量替换
                    let rescue_template = {
                        let res_path_prod = app
                            .path()
                            .resource_dir()
                            .unwrap_or_default()
                            .join("prompts/rescue.md");
                        std::fs::read_to_string(&res_path_prod)
                            .or_else(|_| std::fs::read_to_string("prompts/rescue.md"))
                            .unwrap_or_else(|_| {
                                // 最后一道防线：内联默认模板
                                "请分析以下场景并输出一个正确的 JSON 指令。目标: {{GOAL}}。报错: {{ERROR_MSG}}".to_string()
                            })
                    };
                    let help_prompt = rescue_template
                        .replace("{{GOAL}}", &goal)
                        .replace("{{TOOL_DETAIL}}", &context.active_tool_detail)
                        .replace("{{RECENT_CONTEXT}}", &recent_context)
                        .replace("{{CURRENT_OBSERVATION}}", &context.current_observation)
                        .replace("{{FAILED_ACTION}}", &inst.get_action())
                        .replace("{{FAILED_PARAMS}}", &format!("{:?}", inst.get_params()))
                        .replace("{{ERROR_MSG}}", &dispatch_result.stderr);
                    let ask_cmd = format!("ask_web_ai kimi {}", help_prompt);
                    let (stdout, stderr, success) =
                        crate::agent::browser::run_browser_dom(&final_session_id, &ask_cmd);

                    if success {
                        println!("✅ [Rescue] Kimi 救援响应成功！");
                        // 尝试直接截获 Kimi 吐出的 JSON 指令包！
                        if let Some(json_str) = crate::agent::utils::extract_json_from_text(&stdout)
                        {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                                if let Ok(parsed_inst) = serde_json::from_value::<
                                    crate::agent::types::AgentInstruction,
                                >(val)
                                {
                                    let _ = app.emit("agent-log", "🔥 Kimi 返回了完美的 JSON 指令格式，系统已截断本地 AI，马上自动代打本回合！");

                                    // 不要用 continue 重头规划，直接把这一条完美方案覆盖给本次本该让 AI 出的代码，去跑！
                                    let dispatch_result = {
                                        let inst_c = parsed_inst.clone();
                                        let registry_arc = registry_state.inner().clone();
                                        let sid = final_session_id.clone();
                                        tokio::task::spawn_blocking(move || {
                                            let mut registry =
                                                futures::executor::block_on(registry_arc.lock());
                                            println!("⚡ 专家指令直接执行: {:?}", inst_c);
                                            run_agent_step(&sid, &inst_c, &mut registry)
                                        })
                                        .await
                                        .map_err(|e| e.to_string())?
                                    };

                                    // 代办：直接拿着刚才跑出来的 dispatch_result 跳出去正常记录，如果失败了的话由外面的判断自己去收拾！
                                    break (parsed_inst, dispatch_result);
                                }
                            }
                        }

                        let rescue_feedback = format!("【🌟 场外专家 (网页 Kimi) 的急救诊断建议】：\n{}\n请你必须仔细阅读上述专家建议，并立即改变你的策略重新规划行动！", stdout);
                        context.add_error_feedback(&error_detail);
                        context.add_error_feedback(&rescue_feedback);

                        let _ = app.emit(
                            "agent-log",
                            "✅ 场外救驾对策已就绪（但非 JSON 指令），交还本地 AI 最后尝试...",
                        );
                        max_retries = 4; // 给出最后一次机会
                        continue;
                    } else {
                        println!("⚠️ [Rescue] 场外援助由于异常失败: {}", stderr);
                        let rescue_feedback =
                            format!("【⚠️ 自动场外援助失败，只能重新靠你自己】：{}", stderr);
                        context.add_error_feedback(&error_detail);
                        context.add_error_feedback(&rescue_feedback);

                        max_retries = 4;
                        continue;
                    }
                }

                if retry_count >= max_retries {
                    return Err(error_detail);
                }

                app.emit(
                    "agent-log",
                    format!(
                        "🔄 执行失败(第{}次)，正注入错误反馈给 AI 重试...",
                        retry_count
                    ),
                )
                .map_err(|e| e.to_string())?;
                context.add_error_feedback(&error_detail);
                continue; // 带着错误信息，重新规划！
            }
        };

        // ================================================================
        // 以下是成功跳出规划+执行重试循环后的处理逻辑
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
        let next_tool = instruction
            .next_tool_hint
            .clone()
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
        } else {
            let plugin_name = next_tool.split('/').next().unwrap_or(&next_tool);
            let detail = {
                let mut registry = registry_state.lock().await;
                if registry.plugin_names().contains(&plugin_name) {
                    registry.format_tool_detail(plugin_name)
                } else {
                    String::new()
                }
            };
            if detail.is_empty() {
                context.active_tool_detail.clear();
            } else {
                context.active_tool_detail = detail;
            }
        }

        // --- 核心增强：检查是否有截图反馈需要注入多模态消息 ---
        // browser.rs 的 screenshot 动作返回格式："data:image/jpeg;base64,xxxxx"
        let mut final_stdout = result.stdout.clone();

        // 动态获取当前视口真实尺寸（用于告知模型坐标范围）
        let viewport_size = {
            let tab_result = crate::agent::browser::get_or_create_tab(&final_session_id);
            if let Ok(tab) = tab_result {
                let js = "JSON.stringify({w: window.innerWidth, h: window.innerHeight})";
                tab.evaluate(js, false).ok()
                    .and_then(|r| r.value)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| r#"{"w":1280,"h":800}"#.to_string())
            } else {
                r#"{"w":1280,"h":800}"#.to_string()
            }
        };
        // 解析 w/h
        let vp: serde_json::Value = serde_json::from_str(&viewport_size).unwrap_or(json!({"w":1280,"h":800}));
        let vp_w = vp.get("w").and_then(|v| v.as_i64()).unwrap_or(1280);
        let vp_h = vp.get("h").and_then(|v| v.as_i64()).unwrap_or(800);

        if result.success && (final_stdout.starts_with("data:image/png;base64,") || final_stdout.starts_with("data:image/jpeg;base64,")) {
            let base64_img = final_stdout
                .trim_start_matches("data:image/png;base64,")
                .trim_start_matches("data:image/jpeg;base64,");

            // === 新增：获取当前页面的轻量 DOM 坐标快照 ===
            // 只提取有文字的可交互元素，每个元素给出精确的中心点坐标
            // 这样大模型就不需要"猜"坐标，直接从表里查就行
            let dom_coord_snapshot = {
                let tab_result = crate::agent::browser::get_or_create_tab(&final_session_id);
                if let Ok(tab) = tab_result {
                    let js = r#"
                    (function() {
                        let items = [];
                        let idx = 1;
                        document.querySelectorAll('a, button, input, textarea, select, [role="button"], [role="link"], [contenteditable="true"]').forEach((el) => {
                            const rect = el.getBoundingClientRect();
                            // 过滤掉不可见元素和视口外的元素
                            if (rect.width < 2 || rect.height < 2) return;
                            if (rect.top > window.innerHeight || rect.bottom < 0) return;
                            if (rect.left > window.innerWidth || rect.right < 0) return;
                            const style = window.getComputedStyle(el);
                            if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') return;

                            // 提取元素文字
                            let text = (el.innerText || el.placeholder || el.getAttribute('aria-label') || el.getAttribute('title') || el.value || '').trim().replace(/[\r\n\s]+/g, ' ');
                            if (!text) {
                                if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
                                    text = `输入框(${el.type || 'text'})`;
                                } else {
                                    return; // 无字的 div/span 直接跳过
                                }
                            }
                            if (text.length > 30) text = text.slice(0, 30) + '..';

                            const cx = Math.round(rect.left + rect.width / 2);
                            const cy = Math.round(rect.top + rect.height / 2);
                            items.push(`[${idx}] "${text}" => cx:${cx}, cy:${cy}`);
                            idx++;
                        });
                        return items.slice(0, 40).join('\n'); // 最多40条，防止 Token 爆炸
                    })()
                    "#;
                    tab.evaluate(js, false).ok()
                        .and_then(|r| r.value)
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "（DOM坐标获取失败，请凭截图目测估算）".to_string())
                } else {
                    "（无法获取标签页，请凭截图目测估算）".to_string()
                }
            };

            // === 升级版 Vision Prompt：DOM坐标 + 截图双锚点 ===
            let vision_prompt = format!(
                "【📸 截图 + DOM坐标双锚点分析】\n\
                当前目标任务：{}\n\
                视口真实尺寸：{}×{} 像素\n\n\
                ✅ 【精确坐标表 (来自DOM实时计算，优先使用！)】\n\
                {}\n\n\
                📋 【操作规则 - 严格遵守】\n\
                1. 先看截图，理解当前页面的整体布局和状态\n\
                2. 在上方坐标表中找到与任务目标最匹配的元素\n\
                3. 直接使用表中的 cx/cy 值作为 click_xy 的坐标参数，禁止自行估算！\n\
                4. 如果坐标表中确实没有目标元素（如元素被遮挡或在视口外），才允许凭截图目视估算坐标\n\n\
                ✅ 正确输出示例（坐标直接来自坐标表）：\n\
                {{\n\
                  \"reflection\": \"截图显示评论输入框可见，坐标表[3]\\\"下面我简单唠两句\\\" => cx:648, cy:329，直接使用\",\n\
                  \"thought\": \"使用 DOM 精确坐标 click_xy 点击评论框\",\n\
                  \"tool\": \"browser_dom\",\n\
                  \"command\": {{\"action\": \"click_xy\", \"x\": 648, \"y\": 329}}\n\
                }}",
                goal,
                vp_w, vp_h,
                dom_coord_snapshot
            );

            context.add_image_feedback(&vision_prompt, base64_img);
            final_stdout = format!(
                "(截图+DOM坐标双锚点已发送给视觉模型，共提取 {} 个元素坐标，等待点击指令)",
                dom_coord_snapshot.lines().count()
            );
        } else if result.success && final_stdout.contains("[Screenshot Saved as Base64]: ") {
            // 兼容旧格式
            if let Some(pos) = final_stdout.find("[Screenshot Saved as Base64]: ") {
                let base64_img = &final_stdout[pos + 30..].trim();
                let log_text = &final_stdout[..pos].trim();
                let vision_prompt = format!(
                    "【截图回退】DOM 提取发现异常，截图如下。\n{}\n\n请分析截图，识别目标元素坐标，下一步输出 click_xy 指令。",
                    log_text
                );
                context.add_image_feedback(&vision_prompt, base64_img);
                final_stdout = format!("{}\n(已作为图片附件发送给视觉模型)", log_text);
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
        app.emit("agent-context", &context)
            .map_err(|e| e.to_string())?;
        app.emit(
            "agent-progress",
            json!({
                "type": if result.success { "step_done" } else { "step_error" },
                "step_id": step_id,
                "description": instruction.description,
                "output": output_text
            }),
        )
        .map_err(|e| e.to_string())?;

        // --- 任务完成判定 ---
        if instruction.get_action() == "finish" {
            app.emit(
                "agent-progress",
                json!({
                    "type": "complete",
                    "message": "任务已由 AI 标记完成",
                    "success": true
                }),
            )
            .map_err(|e| e.to_string())?;
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}
