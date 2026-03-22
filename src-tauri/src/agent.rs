use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};
use crate::db::DbState;
use tauri::State;
use reqwest;

// ===================== 数据结构 =====================

/// 一个任务步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub id: usize,
    pub description: String,    // 步骤描述
    pub tool: String,           // 工具类型: "shell" | "osascript" | "info"
    pub command: String,        // 实际执行的命令
    pub status: String,         // "pending" | "running" | "done" | "error"
    pub output: String,         // 执行输出
}

/// AI 规划出来的任务计划
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPlan {
    pub goal: String,
    pub steps: Vec<AgentStep>,
}

// ===================== 工具函数 =====================

/// 执行一条 shell 命令（bash），返回 (stdout, stderr, success)
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

/// 执行 osascript（AppleScript）
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

/// 执行浏览器控制操作 ( headless_chrome )
fn run_browser(url: &str) -> (String, String, bool) {
    use headless_chrome::{Browser, LaunchOptions};
    
    // 启动浏览器（这里故意设为 false 让用户能看见动画）
    let options = LaunchOptions::default_builder().headless(false).build().unwrap_or_default();
    let browser = match Browser::new(options) {
        Ok(b) => b,
        Err(e) => return (String::new(), format!("浏览器启动失败: {}", e), false),
    };

    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(e) => return (String::new(), format!("无法新建标签页: {}", e), false),
    };

    if tab.navigate_to(url).is_err() || tab.wait_until_navigated().is_err() {
         return (String::new(), "页面导航失败".to_string(), false);
    }
    
    // 粗略等待加载
    std::thread::sleep(std::time::Duration::from_secs(3));
    
    // 尝试获取页面的标题
    let title = tab.get_title().unwrap_or_else(|_| "未知标题".to_string());
    
    // 尝试获取页面的 body 文本 (简易版)
    let body_text = match tab.find_element("body") {
        Ok(elem) => elem.get_inner_text().unwrap_or_default(),
        Err(_) => "(无法获取页面内容)".to_string(),
    };

    // 截取前 500 个字符交给大模型，足够让它知道页面里大概有什么
    let preview = chars_preview(&body_text, 500);
    
    let stdout = format!("成功访问网页！\n标题: {}\n页面内容预览: {}", title, preview);
    (stdout, String::new(), true)
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

// ================= 最新黑科技：单步状态机全局浏览器与 DOM 神之手 =================

static GLOBAL_BROWSER: OnceLock<headless_chrome::Browser> = OnceLock::new();
static GLOBAL_TAB: OnceLock<Arc<headless_chrome::Tab>> = OnceLock::new();

fn get_or_create_tab() -> Result<Arc<headless_chrome::Tab>, String> {
    if let Some(tab) = GLOBAL_TAB.get() {
        return Ok(tab.clone());
    }
    
    let options = headless_chrome::LaunchOptions::default_builder()
        .headless(false)
        .idle_browser_timeout(std::time::Duration::from_secs(36000))
        .build().unwrap_or_default();
        
    let browser = headless_chrome::Browser::new(options).map_err(|e| format!("拉起物理浏览器失败: {}", e))?;
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

    // 智能解析：优先支持 "goto url", "click 1", "type 1 text", "extract" 等自然语言格式
    let (action, url, id, text) = if command_str.trim().starts_with('{') {
        // 兼容模式：JSON 格式
        let cmd: BrowserAction = match serde_json::from_str(command_str) {
            Ok(c) => c,
            Err(e) => return (String::new(), format!("JSON 格式有误: {}", e), false),
        };
        (cmd.action, cmd.url, cmd.id, cmd.text)
    } else {
        // 极简模式：空格分隔字符串
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
            if tab.navigate_to(&target_url).is_err() || tab.wait_until_navigated().is_err() {
                 return (String::new(), "页面导航失败".to_string(), false);
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
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
                    
                    // 获取更丰富的无障碍文本或内部文本
                    let text = el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText || el.value || el.placeholder;
                    if (!text || text.trim() === '') {
                        if (el.isContentEditable || el.tagName === 'TEXTAREA') text = "输入区";
                        else if (el.querySelector('svg')) text = "图标按钮";
                        else text = "无文本的元素";
                    }
                    
                    let tag = el.tagName.toLowerCase();
                    let role = el.getAttribute('role') ? ` role=${el.getAttribute('role')}` : '';
                    let cls_str = "";
                    if (typeof el.className === 'string' && el.className.trim() !== '') {
                        cls_str = ` class="${el.className.trim().split(/\s+/).slice(0, 2).join(' ')}"`;
                    }
                    return `[${id}] <${tag}${role}${cls_str}>: ${text.substring(0, 50).replace(/\n/g, ' ')}`;
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
            std::thread::sleep(std::time::Duration::from_secs(2));
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
            std::thread::sleep(std::time::Duration::from_secs(1));
            (format!("输入成功！"), String::new(), true)
        }
        "press" => {
            let key = text.unwrap_or_else(|| "Enter".to_string());
            if tab.press_key(&key).is_err() {
                return (String::new(), format!("按键 {} 失败", key), false);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
            (format!("已成功按下 {} 键！", key), String::new(), true)
        }
        "read" => {
            let js = "document.body.innerText;";
            match tab.evaluate(js, false) {
                Ok(remote_obj) => {
                    let t = remote_obj.value.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    (format!("【当前整个页面的正文概览】：\n{}", chars_preview(&t, 5000)), String::new(), true)
                }
                Err(e) => (String::new(), format!("提取页面文本失败: {}", e), false)
            }
        }
        _ => (String::new(), format!("未知的 browser_dom 动作: {}", command_str), false)
    }
}

// ================= 最新黑科技：直接控制 Kimi 发出提问 =================
fn run_ask_kimi(question: &str) -> (String, String, bool) {
    use headless_chrome::{Browser, LaunchOptions};
    use std::time::Duration;
    
    let options = LaunchOptions::default_builder().headless(false).build().unwrap_or_default();
    let browser = match Browser::new(options) {
        Ok(b) => b,
        Err(e) => return (String::new(), format!("浏览器启动失败: {}", e), false),
    };

    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(e) => return (String::new(), format!("无法新建标签页: {}", e), false),
    };

    if tab.navigate_to("https://kimi.moonshot.cn").is_err() || tab.wait_until_navigated().is_err() {
         return (String::new(), "页面导航失败".to_string(), false);
    }
    
    std::thread::sleep(Duration::from_secs(4));
    
    let chat_input = match tab.wait_for_element("div[contenteditable='true']") {
        Ok(el) => el,
        Err(_) => return (String::new(), "找不到富文本输入框！可能被登录挡住了！".to_string(), false),
    };

    chat_input.click().ok();
    std::thread::sleep(Duration::from_millis(500)); 

    if chat_input.type_into(question).is_err() {
        return (String::new(), "打字失败".to_string(), false);
    }

    std::thread::sleep(Duration::from_millis(500)); 

    if tab.press_key("Enter").is_err() {
         return (String::new(), "回车发送失败".to_string(), false);
    }
    
    std::thread::sleep(Duration::from_secs(12)); 
    
    ("成功打开并使唤了 Kimi 发送消息！".to_string(), String::new(), true)
}

// ===================== Tauri Commands =====================

/// 直接执行一条 shell 或 osascript 命令（供前端调用）
#[tauri::command]
pub async fn execute_command(
    tool: String,   // "shell" | "osascript"
    command: String,
) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" {
        run_osascript(&command)
    } else {
        run_shell(&command)
    };

    Ok(json!({
        "success": success,
        "stdout": stdout.trim(),
        "stderr": stderr.trim(),
    }))
}

/// AI 解析用户目标 → 生成步骤计划 → 逐步执行 → 通过事件推送进度
///
/// 事件名: "agent-progress"
/// 事件格式: { type: "plan"|"step_start"|"step_done"|"step_error"|"complete"|"error", data: ... }
#[tauri::command]
pub async fn run_agent_task(
    app: AppHandle,
    state: State<'_, DbState>,
    goal: String,
    model_id: String,
) -> Result<(), String> {
    // ── 1. 从数据库拿模型信息 ──
    let (base_url, api_key, model_name) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT p.base_url, p.api_key, m.name FROM models m \
             JOIN platforms p ON p.id = m.platform_id WHERE m.id = ?1"
        ).map_err(|e| e.to_string())?;
        stmt.query_row(rusqlite::params![model_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        }).map_err(|e| format!("模型不存在: {}", e))?
    };

    // ── 2. 让 AI 规划步骤（system prompt 约束输出 JSON）──
    let system_prompt = r#"你是一个 macOS 自动化 AI 助手。用户会给你一个目标，你需要将其拆分为具体的可执行步骤。

你只能回复一个合法的 JSON 对象，格式如下：
{
  "steps": [
    {
      "description": "步骤描述（中文）",
      "tool": "shell" 或 "osascript",
      "command": "实际要执行的命令字符串"
    }
  ]
}

工具说明：
- shell: 执行真正的终端系统命令（如 ls, mkdir）。
- browser_dom: 操作网页的唯一合法途径！【注意：如果用此工具，tool 字段必须写 "browser_dom"，绝对绝对不能错写成 "shell"！！！】
    * goto [URL] - 访问网页（如: goto https://github.com）
    * extract - 查看屏幕坐标（全自动模式下少用，建议去单步协同模式使用）
    * click [ID] - 点击对应编号
    * type [ID] [内容] - 输入内容
    * press [Key] - 直接模拟系统按键（如 press Enter，当找不到发送按钮时极其好用！）
    * read - 阅读网页纯文本内容，用于获取答案。
- osascript: 仅用于执行简单的桌面系统弹窗通知。绝对禁止写 AppleScript 操控浏览器！

重要规则：
1. 只输出 JSON，不要有任何其他一段多余的文字！
2. 只要是去访问网页，必须用 "tool": "browser_dom"！
3. 每步只做一件事。"#;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build().map_err(|e| e.to_string())?;

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model_name,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": format!("目标：{}", goal)}
        ],
        "max_tokens": 1024,
        "temperature": 0.2,
        "stream": false
    });

    app.emit("agent-progress", json!({
        "type": "planning",
        "message": "AI 正在分析目标并规划执行步骤…"
    })).ok();

    let resp = client.post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("HTTP-Referer", "https://free-api-chat.app")
        .header("X-OpenRouter-Title", "Free API Chat Agent")
        .json(&body).send().await
        .map_err(|e| {
            let msg = format!("AI 请求失败: {}", e);
            app.emit("agent-progress", json!({"type": "error", "message": msg})).ok();
            e.to_string()
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        let msg = format!("AI API 错误 {}: {}", status, &body_text[..body_text.len().min(300)]);
        app.emit("agent-progress", json!({"type": "error", "message": msg})).ok();
        return Err(msg);
    }

    let resp_json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("{\"steps\":[]}")
        .trim()
        .to_string();

    // 尝试清洗 markdown 代码块
    let cleaned = content
        .trim_start_matches("```json").trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string();

    let plan_json: serde_json::Value = serde_json::from_str(&cleaned)
        .map_err(|e| {
            let msg = format!("解析 AI 规划失败: {} \n原始内容: {}", e, &cleaned[..cleaned.len().min(500)]);
            app.emit("agent-progress", json!({"type": "error", "message": msg})).ok();
            msg
        })?;

    let raw_steps = plan_json["steps"].as_array()
        .cloned()
        .unwrap_or_default();

    let mut steps: Vec<AgentStep> = raw_steps.iter().enumerate().map(|(i, s)| AgentStep {
        id: i,
        description: s["description"].as_str().unwrap_or("").to_string(),
        tool: s["tool"].as_str().unwrap_or("shell").to_string(),
        command: s["command"].as_str().unwrap_or("").to_string(),
        status: "pending".to_string(),
        output: String::new(),
    }).collect();

    // ── 3. 推送计划给前端 ──
    app.emit("agent-progress", json!({
        "type": "plan",
        "steps": steps.iter().map(|s| json!({
            "id": s.id,
            "description": s.description,
            "tool": s.tool,
            "command": s.command,
            "status": s.status,
            "output": s.output,
        })).collect::<Vec<_>>()
    })).ok();

    // 等一小会儿让前端渲染
    tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;

    // ── 4. 逐步执行，循环轮询检查 ──
    for step in steps.iter_mut() {
        step.status = "running".to_string();
        app.emit("agent-progress", json!({
            "type": "step_start",
            "step_id": step.id,
            "description": step.description,
            "tool": step.tool,
            "command": step.command,
        })).ok();

        // 执行命令
        let tool_clone = step.tool.clone();
        let command_clone = step.command.clone();

        let (stdout, stderr, success) = tokio::task::spawn_blocking(move || {
            if tool_clone == "osascript" {
                run_osascript(&command_clone)
            } else if tool_clone == "browser" {
                run_browser(&command_clone)
            } else if tool_clone == "browser_kimi" {
                run_ask_kimi(&command_clone)
            } else if tool_clone == "browser_dom" {
                run_browser_dom(&command_clone)
            } else {
                run_shell(&command_clone)
            }
        }).await.map_err(|e| e.to_string())?;

        let output = if !stdout.is_empty() {
            stdout.trim().to_string()
        } else if !stderr.is_empty() {
            format!("[stderr] {}", stderr.trim())
        } else {
            "(无输出)".to_string()
        };

        step.output = output.clone();

        if success {
            step.status = "done".to_string();
            app.emit("agent-progress", json!({
                "type": "step_done",
                "step_id": step.id,
                "output": output,
                "success": true,
            })).ok();
        } else {
            step.status = "error".to_string();
            app.emit("agent-progress", json!({
                "type": "step_error",
                "step_id": step.id,
                "output": output,
                "success": false,
            })).ok();
            // 步骤失败时停止后续步骤
            app.emit("agent-progress", json!({
                "type": "complete",
                "success": false,
                "message": format!("步骤 {} 执行失败，任务中止", step.id + 1),
            })).ok();
            return Ok(());
        }

        // 步骤间短暂停顿，让用户看清楚
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    // ── 5. 全部完成 ──
    app.emit("agent-progress", json!({
        "type": "complete",
        "success": true,
        "message": "所有步骤执行完毕！",
    })).ok();

    Ok(())
}

// ===================== 全新：单步交互式特工 Copilot 命令 =====================

/// 每次只向大模型请求“当前状态下的一步操作”
#[tauri::command]
pub async fn plan_next_step(
    app: AppHandle,
    state: State<'_, DbState>,
    model_id: String,
    goal: String,
    req_history: String,
) -> Result<String, String> {
    
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

    let system_prompt = r#"你是一个极其谨慎、严苛的 macOS 单步执行特工（Copilot）。
用户的最终目标会提供给你。无论如何，你绝对不要瞎猜后续所有步骤！
你现在必须**阅读历史执行记录**，判断当前走到哪一步了，并**仅仅只输出紧接着的唯一 1 个步骤的 JSON**。

【最高级防幻觉规则】
你绝对不能擅自编写 AppleScript 或 Python 去控制浏览器！这会引发灾难性系统错误！
系统已经内置了一个完美的物理级跨站特工 API：`browser_dom`！如果你要操作网页，【必须】使用它！

【必须输出以下 JSON 格式】
{
  "description": "说明你要做什么",
  "tool": "shell" | "browser_dom" | "finish",
  "command": "见下方的要求"
}

【工具使用严格规范】
1. shell: 仅用于执行真正的本地终端命令（如 ls, mkdir）。
2. browser_dom: 只要是控制浏览器，【tool 字段必须严格写 "browser_dom"，绝对不能写成 "shell"！】
   它的 command 必须且只能是以下六种简单字符串之一：
   - goto [URL] (比如 `goto https://google.com`)
   - extract (重要！这会提取出所有能点的元素 ID 列表)
   - click [ID] (比如 `click 5`)
   - type [ID] [文本] (比如 `type 2 Hello`)
   - press [Key] (比如 `press Enter`。找不到发送按钮就硬派系统回车！)
   - read (当你需要阅读整篇文章的内容、回答用户的提问、或者检查当前的网页结果究竟写了什么文本时，使用这个命令获取全页面纯文本！)
3. finish: 任务结束时使用，command 写总结结论"#;

    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().unwrap();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let user_prompt = format!("最终目标：{}\n\n之前的执行记录与屏幕结果：\n{}\n\n请只给我下一步的 JSON。", goal, req_history);
    
    let mut last_error = String::new();
    for i in 0..3 {
        let body = json!({
            "model": model_name,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "max_tokens": 1024,
            "temperature": 0.1 + (i as f64 * 0.2) // 每次失败稍微提高一点温度，增加多样性
        });

        let resp = client.post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&body).send().await;

        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    last_error = format!("API 请求失败: {}", r.status());
                    continue;
                }
                let resp_json: serde_json::Value = match r.json().await {
                    Ok(j) => j,
                    Err(e) => {
                        last_error = format!("解析响应 JSON 失败: {}", e);
                        continue;
                    }
                };
                let content = resp_json["choices"][0]["message"]["content"]
                    .as_str().unwrap_or("{}").trim().to_string();

                let cleaned = content.trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
                match serde_json::from_str::<serde_json::Value>(cleaned) {
                    Ok(parsed) => return Ok(parsed.to_string()),
                    Err(e) => {
                        last_error = format!("第 {} 次尝试：大模型未返回合规的 JSON: {}", i+1, e);
                    }
                }
            }
            Err(e) => {
                last_error = format!("网络请求失败: {}", e);
            }
        }
        // 如果失败了，等一秒再试
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
    }

    Err(format!("已达到最大重试次数 (3)，任务失败。最后一次报错：{}", last_error))
}

/// 执行单步动作（由前台控制）
#[tauri::command]
pub async fn execute_single_step(tool: String, command: String) -> Result<String, String> {
    let (stdout, stderr, success) = tokio::task::spawn_blocking(move || {
        if tool == "osascript" { run_osascript(&command) }
        else if tool == "browser" { run_browser(&command) }
        else if tool == "browser_kimi" { run_ask_kimi(&command) }
        else if tool == "browser_dom" { run_browser_dom(&command) }
        else { run_shell(&command) }
    }).await.map_err(|e| e.to_string())?;

    let output = if !stdout.is_empty() { stdout.trim().to_string() } 
                 else if !stderr.is_empty() { format!("[Error] {}", stderr.trim()) } 
                 else { "(执行成功，无内容输出)".to_string() };
                 
    if !success {
        return Err(output);
    }
    Ok(output)
}
