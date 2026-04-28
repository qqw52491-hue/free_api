use crate::agent::browser::run_browser_dom;
use crate::agent::types::DispatchResult;
use crate::agent::utils::{run_osascript, run_shell};

const BROWSER_VERBS: &[&str] = &[
    "goto",
    "navigate",
    "extract",
    "look",
    "click",
    "click_xy",
    "type",
    "press",
    "read",
    "scroll",
    "hover",
    "select",
    "wait",
    "wait_for",
    "wait_idle",
    "back",
    "forward",
    "refresh",
    "url",
    "tab_url",
    "eval",
    "js",
    "screenshot",
    "ask_web_ai",
    "new_tab",
    "switch_tab",
    "list_tabs",
    "close_tab",
];

pub fn run_builtin_step(session_id: &str, action: &str, params: &serde_json::Value) -> Option<DispatchResult> {
    let action_low = action.to_lowercase();

    // 1. 尝试还原指令字符串
    let cmd_str = if let Some(s) = params.as_str() {
        // 直接字符串参数（例如 command: "goto https://..."）
        s.to_string()
    } else if params.is_object() {
        // --- 从 JSON 对象中提取各字段 ---
        let verb_from_params = params
            .get("action")
            .or(params.get("verb"))
            .or(params.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // 决定最终动词：params 里的 action 字段 > 外层传入的 action
        let verb = if !verb_from_params.is_empty() { verb_from_params } else { action };
        let verb_low = verb.to_lowercase();

        // 通用辅助字段
        let id = params
            .get("id")
            .or(params.get("element_id"))
            .or(params.get("selector"))
            .and_then(|v| {
                v.as_u64().map(|n| n.to_string()).or_else(|| {
                    v.as_str().map(|s| s.trim_start_matches('[').trim_end_matches(']').to_string())
                })
            })
            .unwrap_or_default();
        let text = params
            .get("text")
            .or(params.get("val"))
            .or(params.get("value"))
            .or(params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // 新增：scroll 的 direction 字段，press 的 key 字段，wait 的 seconds 字段
        let direction = params
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let seconds = params
            .get("seconds")
            .or(params.get("duration"))
            .or(params.get("time"))
            .and_then(|v| v.as_f64().map(|n| n.to_string()).or_else(|| v.as_str().map(|s| s.to_string())))
            .unwrap_or_default();
        let js_code = params
            .get("code")
            .or(params.get("js"))
            .or(params.get("script"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let safe_id = if id.is_empty() { "0" } else { id.as_str() };

        let final_cmd = match verb_low.as_str() {
            // 导航
            "goto" | "navigate" => format!("{} {}", verb_low, url),
            // 滚动 —— 支持 direction 字段（这是之前丢失数据的主要场景！）
            "scroll" => {
                let arg = if !direction.is_empty() { direction }
                          else if !text.is_empty() { text }
                          else { safe_id };
                format!("scroll {}", arg)
            }
            // 按键 —— 支持 key 字段
            "press" => {
                let arg = if !key.is_empty() { key }
                          else if !text.is_empty() { text }
                          else { safe_id };
                format!("press {}", arg)
            }
            // 等待 —— 支持 seconds/duration 字段
            "wait" => {
                let arg = if !seconds.is_empty() { seconds.as_str() }
                          else if !safe_id.is_empty() && safe_id != "0" { safe_id }
                          else { "1" };
                format!("wait {}", arg)
            }
            // 输入 —— 需要 id + text
            "type" => format!("type {} {}", safe_id, text),
            // 点击/悬停/选择 —— 需要 id
            "click" | "hover" | "wait_for" => format!("{} {}", verb_low, safe_id),
            // 坐标点击 —— 需要 x 和 y 字段
            "click_xy" => {
                let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                format!("click_xy {} {}", x, y)
            }
            "select" => format!("select {} {}", safe_id, text),
            // JS 执行
            "eval" | "js" => {
                let code = if !js_code.is_empty() { js_code } else { text };
                format!("eval {}", code)
            }
            // ask_web_ai: ask_web_ai kimi <prompt>
            "ask_web_ai" => format!("ask_web_ai {} {}", url, text),
            // 无参数指令
            "extract" | "look" | "read" | "screenshot" | "tab_url" | "url"
            | "back" | "forward" | "refresh" | "wait_idle"
            | "list_tabs" | "new_tab" | "switch_tab" | "close_tab" => verb_low.to_string(),
            // 兜底：直接把 verb 原样传下去
            _ => verb_low.to_string(),
        };

        final_cmd.trim().to_string()
    } else {
        // params 不是字符串也不是对象，直接用 action 本身
        String::new()
    };

    // 2. 路由分发
    if action_low == "browser_dom" {
        let (stdout, stderr, success) = run_browser_dom(session_id, &cmd_str);
        return Some(DispatchResult {
            stdout,
            stderr,
            success,
            route: "browser".to_string(),
        });
    }

    // 特殊：如果 action 本身就是浏览器指令名
    if BROWSER_VERBS.contains(&action_low.as_str()) {
        let full_cmd = if cmd_str.starts_with(&action_low) {
            cmd_str.clone()
        } else {
            format!("{} {}", action_low, cmd_str).trim().to_string()
        };
        let (stdout, stderr, success) = run_browser_dom(session_id, &full_cmd);
        return Some(DispatchResult {
            stdout,
            stderr,
            success,
            route: "browser".to_string(),
        });
    }

    match action_low.as_str() {
        "osascript" | "shell" => Some(DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ [shell/osascript 已禁用] 系统安全策略不允许直接执行内置命令。\n\
                情报：你尝试使用 '{}' 工具\n\
                解决方案：使用 browser_dom 工具完成任务。",
                action_low
            ),
            success: false,
            route: "blocked_shell".to_string(),
        }),
        "finish" => Some(DispatchResult {
            stdout: cmd_str,
            stderr: String::new(),
            success: true,
            route: "agent".to_string(),
        }),
        _ => None,
    }
}
