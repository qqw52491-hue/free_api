use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, Manager};
use crate::db::DbState;
use tauri::State;
use reqwest;
use tokio::time::{sleep, Duration};

// ===================== 数据结构 =====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub id: usize,
    pub description: String,
    pub thought: String,
    pub tool: String,
    pub command: String,
    pub status: String, // "pending" | "running" | "done" | "error"
    pub output: String,
}

// ===================== 工具函数 =====================

fn run_shell(cmd: &str) -> (String, String, bool) {
    let result = Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

fn run_osascript(script: &str) -> (String, String, bool) {
    let result = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

fn chars_preview(s: &str, limit: usize) -> String {
    let s = s.replace("\n", " ");
    if s.chars().count() > limit {
        let truncated: String = s.chars().take(limit).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}

/// 从 AI 回复中鲁棒地提取 JSON 对象。
/// 处理以下情况：
/// 1. 纯 JSON: `{"tool": ...}`
/// 2. Markdown 包裹: ```json { ... } ```
/// 3. 前后有废话: `我来帮你... {"tool": ...} 希望这样可以`
fn extract_json_from_text(text: &str) -> Option<String> {
    // 先剥掉 Markdown 代码块标记
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // 尝试直接解析（最快路径）
    if serde_json::from_str::<serde_json::Value>(cleaned).is_ok() {
        return Some(cleaned.to_string());
    }

    // 找第一个 '{' 和匹配的最后一个 '}'，用花括号深度匹配
    let chars: Vec<char> = cleaned.chars().collect();
    let start = chars.iter().position(|c| *c == '{')?;
    
    let mut depth = 0i32;
    let mut end = start;
    let mut in_string = false;
    let mut escape_next = false;

    for i in start..chars.len() {
        let ch = chars[i];
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            if ch == '{' { depth += 1; }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
        }
    }

    if depth == 0 && end > start {
        let json_str: String = chars[start..=end].iter().collect();
        // 验证提取的内容确实是合法 JSON
        if serde_json::from_str::<serde_json::Value>(&json_str).is_ok() {
            return Some(json_str);
        }
    }

    None
}

// ================= 浏览器与 DOM 控制 =================

static GLOBAL_BROWSER: OnceLock<headless_chrome::Browser> = OnceLock::new();
static GLOBAL_TAB: OnceLock<Arc<headless_chrome::Tab>> = OnceLock::new();

fn get_or_create_tab() -> Result<Arc<headless_chrome::Tab>, String> {
    if let Some(tab) = GLOBAL_TAB.get() {
        return Ok(tab.clone());
    }
    
    let options = headless_chrome::LaunchOptions::default_builder()
        .headless(false) // 设置为 false 方便调试
        .idle_browser_timeout(Duration::from_secs(36000))
        .args(vec![
            "--no-sandbox".as_ref(),
            "--disable-setuid-sandbox".as_ref(),
            "--disable-gpu".as_ref(), // 有时启用 GPU 会导致内核崩溃
            "--window-size=1280,800".as_ref(),
            "--disable-dev-shm-usage".as_ref(), // 解决 Linux 环境下的内存限制
        ])
        .build().unwrap_or_default();
        
    let browser = headless_chrome::Browser::new(options).map_err(|e| format!("拉起浏览器失败: {}", e))?;
    let tab = browser.new_tab().map_err(|e| format!("新建标签页失败: {:?}", e))?;
    
    let _ = GLOBAL_BROWSER.set(browser);
    let _ = GLOBAL_TAB.set(tab.clone());
    
    Ok(tab)
}

#[derive(Serialize, Deserialize)]
struct BrowserAction {
    action: String,
    url: Option<String>,
    id: Option<u32>,
    text: Option<String>,
}

fn run_browser_dom(command_str: &str) -> (String, String, bool) {
    let tab = match get_or_create_tab() {
        Ok(t) => t,
        Err(e) => return (String::new(), e, false),
    };

    let (action, url, id, text) = if command_str.trim().starts_with('{') {
        let cmd: BrowserAction = match serde_json::from_str(command_str) {
            Ok(c) => c,
            Err(e) => return (String::new(), format!("JSON 格式有误: {}", e), false),
        };
        (cmd.action, cmd.url, cmd.id, cmd.text)
    } else {
        let parts: Vec<&str> = command_str.splitn(3, ' ').collect();
        let cmd_type = parts.get(0).unwrap_or(&"").to_lowercase();
        match cmd_type.as_str() {
            "goto" | "navigate" => ("navigate".to_string(), parts.get(1).map(|s| s.to_string()), None, None),
            "extract" | "look" => ("extract".to_string(), None, None, None),
            "click" => {
                let id = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                ("click".to_string(), None, id, None)
            }
            "type" => {
                let id = parts.get(1).and_then(|s| s.parse::<u32>().ok());
                let val = parts.get(2).map(|s| s.to_string());
                ("type".to_string(), None, id, val)
            }
            "press" => {
                let key = parts.get(1).map(|s| s.to_string());
                ("press".to_string(), None, None, key)
            }
            "read" => ("read".to_string(), None, None, None),
            _ => ("unknown".to_string(), None, None, None),
        }
    };

    match action.as_str() {
        "navigate" => {
            let target_url = url.unwrap_or_default();
            if !target_url.starts_with("http") {
                return (String::new(), "URL 格式不正确，必须以 http 开头".to_string(), false);
            }
            if let Err(e) = tab.navigate_to(&target_url) {
                 return (String::new(), format!("跳转指令发送失败: {:?}", e), false);
            }
            if let Err(e) = tab.wait_until_navigated() {
                 return (String::new(), format!("页面加载超时或等待失败: {:?}", e), false);
            }
            std::thread::sleep(Duration::from_secs(3));
            let title = tab.get_title().unwrap_or_default();
            (format!("成功跳转！标题: {}", title), String::new(), true)
        }
        "extract" => {
            let js = r#"
            (function() {
                const interactables = Array.from(document.querySelectorAll('a, button, input, textarea, select, [contenteditable="true"], [role="button"], [role="link"], [tabindex]:not([tabindex="-1"]), [class*="button" i], [class*="btn" i], [class*="send" i]'));
                let textNodes = [];
                let treeWalker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null, false);
                let currentNode;
                while(currentNode = treeWalker.nextNode()) {
                    if(currentNode.textContent.trim().length > 0) {
                        let parent = currentNode.parentElement;
                        if(parent && window.getComputedStyle(parent).display !== 'none' && !parent.closest('a, button, input, textarea, select, [contenteditable="true"], [role="button"]')) {
                            textNodes.push(parent);
                        }
                    }
                }
                let allElements = Array.from(new Set([...interactables, ...textNodes]));
                let results = allElements.map((el, i) => {
                    const rect = el.getBoundingClientRect();
                    if (rect.width === 0 || rect.height === 0 || rect.y < 0) return null;
                    let id = i + 1;
                    el.setAttribute('data-tauri-agent-id', id);
                    el.style.outline = "2px dashed red";
                    let text = el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText || el.value || el.placeholder;
                    if (!text || text.trim() === '') {
                        if (el.isContentEditable || el.tagName === 'TEXTAREA') text = "输入区";
                        else if (el.querySelector('svg')) text = "图标按钮";
                        else text = "无文本的元素";
                    }
                    let tag = el.tagName.toLowerCase();
                    return `[${id}] <${tag}>: ${text.substring(0, 50).replace(/\n/g, ' ')}`;
                }).filter(r => r !== null);
                return results.join('\n');
            })();
            "#;
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let text = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (format!("【当前屏幕元素清单】：\n{}", text), String::new(), true)
                }
                Err(e) => (String::new(), format!("提取DOM失败: {}", e), false)
            }
        }
        "click" => {
            let id = id.unwrap_or(0);
            let js = format!("document.querySelector('[data-tauri-agent-id=\"{}\"]').click();", id);
            if tab.evaluate(&js, false).is_err() {
                return (String::new(), format!("找不到编号为 {} 的元素", id), false);
            }
            std::thread::sleep(Duration::from_secs(2));
            (format!("点击编号 {} 成功！", id), String::new(), true)
        }
        "type" => {
            let id = id.unwrap_or(0);
            let val = text.unwrap_or_default();
            let js_focus = format!("document.querySelector('[data-tauri-agent-id=\"{}\"]').focus();", id);
            let _ = tab.evaluate(&js_focus, false);
            if tab.type_str(&val).is_err() {
                 let fallback_js = format!("let el = document.querySelector('[data-tauri-agent-id=\"{}\"]'); el.value='{}'; el.innerText='{}';", id, val, val);
                 let _ = tab.evaluate(&fallback_js, false);
            }
            std::thread::sleep(Duration::from_secs(1));
            (format!("输入成功！"), String::new(), true)
        }
        "press" => {
            let key = text.unwrap_or_else(|| "Enter".to_string());
            if tab.press_key(&key).is_err() {
                return (String::new(), format!("按键 {} 失败", key), false);
            }
            std::thread::sleep(Duration::from_secs(1));
            (format!("已成功按下 {} 键！", key), String::new(), true)
        }
        "read" => {
            let js = "document.body.innerText;";
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let t = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (format!("【当前页正文】：\n{}", chars_preview(&t, 5000)), String::new(), true)
                }
                Err(e) => (String::new(), format!("提取页面文本失败: {}", e), false)
            }
        }
        _ => (String::new(), format!("未知的 browser_dom 动作: {}", command_str), false)
    }
}

// ===================== 核心业务逻辑 =====================

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    model_id: String,
    goal: String,
    auto_pilot: bool,
) -> Result<(), String> {
    
    // 1. 获取模型配置
    let (base_url, api_key, model_name) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT p.base_url, p.api_key, m.name FROM models m \
             JOIN platforms p ON p.id = m.platform_id WHERE m.id = ?1"
        ).map_err(|e| e.to_string())?;
        stmt.query_row(rusqlite::params![model_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        }).map_err(|e| format!("模型不存在: {}", e))?
    };

    // 2. 初始化对话历史
    let prompt_path = app.path().resource_dir().map_err(|e| e.to_string())?
        .join("prompts/agent_system_prompt.md");
    
    // 如果是开发环境，可能需要直接读取工作目录
    let system_prompt = std::fs::read_to_string(&prompt_path).unwrap_or_else(|_| {
        // Fallback to relative paths for development
        std::fs::read_to_string("src-tauri/prompts/agent_system_prompt.md")
            .or_else(|_| std::fs::read_to_string("prompts/agent_system_prompt.md"))
            .unwrap_or_else(|_| "你是一个极其谨慎的 macOS 自动化特工。任务目标：完成用户目标。返回 JSON。".to_string())
    });

    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage { role: "system".to_string(), content: system_prompt },
        ChatMessage { role: "user".to_string(), content: format!("任务目标：{}", goal) },
    ];

    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let mut step_count = 0;
    loop {
        if step_count > 20 { // 防止死循环
            app.emit("agent-progress", json!({"type": "error", "message": "已达到最大步数限制（20）"})).ok();
            break;
        }
        step_count += 1;

        // --- A. AI 规划阶段 ---
        app.emit("agent-progress", json!({"type": "planning", "message": "正在思考下一步动作..."})).ok();

        let resp = client.post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&json!({
                "model": model_name,
                "messages": messages,
                "temperature": 0.1,
            })).send().await;

        let content = match resp {
            Ok(r) if r.status().is_success() => {
                let j: serde_json::Value = r.json().await.unwrap_or_default();
                j["choices"][0]["message"]["content"].as_str().unwrap_or("{}").trim().to_string()
            }
            Ok(r) => {
                let err_text = r.text().await.unwrap_or_default();
                app.emit("agent-progress", json!({"type": "error", "message": format!("API 报错: {}", err_text)})).ok();
                break;
            }
            Err(e) => {
                app.emit("agent-progress", json!({"type": "error", "message": format!("网络错误: {}", e)})).ok();
                break;
            }
        };

        // 强力 JSON 提取：无论模型在 JSON 前后加了什么废话，都能正确提取
        let extracted_json = extract_json_from_text(&content);
        let plan: serde_json::Value = match extracted_json {
            Some(json_str) => match serde_json::from_str(&json_str) {
                Ok(p) => p,
                Err(e) => {
                    // JSON 找到了但解析失败，把错误反馈给模型让它重试
                    app.emit("agent-progress", json!({"type": "error", "message": format!("JSON 解析失败，正在要求模型重新输出: {}", e)})).ok();
                    messages.push(ChatMessage { role: "assistant".to_string(), content: content.clone() });
                    messages.push(ChatMessage { role: "user".to_string(), content: format!(
                        "你的上一次回复 JSON 格式有误（错误: {}）。请严格按照格式重新输出，只返回纯 JSON，不要包含任何其他文字。", e
                    )});
                    continue; // 让模型重试
                }
            },
            None => {
                // 完全找不到 JSON，反馈给模型让它重试
                app.emit("agent-progress", json!({"type": "error", "message": format!("AI 输出了非 JSON 内容，正在要求重新输出")})).ok();
                messages.push(ChatMessage { role: "assistant".to_string(), content: content.clone() });
                messages.push(ChatMessage { role: "user".to_string(), content:
                    "你的上一次回复不包含合法的 JSON 对象。请记住：你只能返回纯 JSON，格式为 {\"thought\":\"...\",\"description\":\"...\",\"tool\":\"...\",\"command\":\"...\"}。不要包含任何 Markdown、解释或额外文字。".to_string()
                });
                continue; // 让模型重试
            }
        };

        let description = plan["description"].as_str().unwrap_or("未知步骤").to_string();
        let tool = plan["tool"].as_str().unwrap_or("finish").to_string();
        let command = plan["command"].as_str().unwrap_or("").to_string();
        let thought = plan["thought"].as_str().unwrap_or("").to_string();

        if !thought.is_empty() {
             println!("【AI 思考】：{}", thought);
        }

        // 任务结束判断
        if tool == "finish" {
            app.emit("agent-progress", json!({"type": "complete", "success": true, "message": command})).ok();
            break;
        }

        // --- B. 推送步骤到前端 ---
        app.emit("agent-progress", json!({
            "type": "step_new", 
            "step": {
                "id": step_count,
                "description": description.clone(),
                "thought": thought.clone(),
                "tool": tool.clone(),
                "command": command.clone(),
                "status": "pending",
                "output": ""
            }
        })).ok();

        // 如果不是全自动模式，这里本应该停下来等用户点，但由于后端是 loop，我们需要和前端互动。
        // 为了演示，这里假设 auto_pilot = true
        if !auto_pilot {
             // 暂不支持手动确认，先改为只支持自动模式（或者增加一个异步等待逻辑）
        }

        // --- C. 执行阶段 ---
        app.emit("agent-progress", json!({"type": "step_start", "step_id": step_count})).ok();

        let tool_c = tool.clone();
        let cmd_c = command.clone();
        let (stdout, stderr, success) = tokio::task::spawn_blocking(move || {
            if tool_c == "osascript" { run_osascript(&cmd_c) }
            else if tool_c == "browser_dom" { run_browser_dom(&cmd_c) }
            else { run_shell(&cmd_c) }
        }).await.map_err(|e| e.to_string())?;

        let output = if success { stdout } else { format!("Error: {}", stderr) };
        
        // --- D. 更新上下文与历史 ---
        // 我们需要把 AI 的决定和执行的结果都存入 messages
        messages.push(ChatMessage { role: "assistant".to_string(), content: plan.to_string() });
        
        // 注意：历史结果如果是巨大的 DOM，我们可以在存入历史时做一点剪裁，防止 Token 撑爆
        let hist_output = if tool == "browser_dom" && command.contains("extract") {
            chars_preview(&output, 2000) // DOM 信息只给模型留 2000 字
        } else {
            chars_preview(&output, 1000)
        };
        messages.push(ChatMessage { role: "user".to_string(), content: format!("执行结果：{}", hist_output) });

        app.emit("agent-progress", json!({
            "type": "step_done",
            "step_id": step_count,
            "output": output,
            "success": success
        })).ok();

        if !success {
             app.emit("agent-progress", json!({"type": "error", "message": format!("步骤执行失败，任务中止")})).ok();
             break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    Ok(())
}

#[tauri::command]
pub async fn execute_command(tool: String, command: String) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" { run_osascript(&command) } else { run_shell(&command) };
    Ok(json!({"success": success, "stdout": stdout.trim(), "stderr": stderr.trim()}))
}
