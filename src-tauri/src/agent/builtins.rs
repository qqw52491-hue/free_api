use crate::agent::browser::run_browser_dom;
use crate::agent::types::DispatchResult;
use crate::agent::utils::{run_osascript, run_shell};

const BROWSER_VERBS: &[&str] = &[
    "goto",
    "navigate",
    "extract",
    "look",
    "click",
    "type",
    "press",
    "read",
    "scroll",
    "hover",
    "select",
    "wait",
    "wait_for",
    "back",
    "forward",
    "refresh",
    "url",
    "tab_url",
    "eval",
    "js",
    "screenshot",
    "ask_web_ai",
];

pub fn run_builtin_step(session_id: &str, action: &str, params: &serde_json::Value) -> Option<DispatchResult> {
    let action_low = action.to_lowercase();

    // 1. 尝试还原指令字符串
    let cmd_str = if let Some(s) = params.as_str() {
        s.to_string()
    } else if params.is_object() {
        let id = params
            .get("id")
            .or(params.get("element_id"))
            .and_then(|v| {
                v.as_u64().or_else(|| {
                    v.as_str()
                        .and_then(|s| s.trim_start_matches('[').trim_end_matches(']').parse().ok())
                })
            })
            .map(|n| n.to_string())
            .unwrap_or_default();
        let text = params
            .get("text")
            .or(params.get("val"))
            .or(params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let mut verb = params
            .get("action")
            .or(params.get("verb"))
            .or(params.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if verb.is_empty() {
            verb = action;
        }

        let safe_id = if id.is_empty() { "0" } else { id.as_str() };

        let final_cmd = match verb {
            "goto" | "navigate" => format!("{} {}", verb, url),
            "ask_web_ai" => format!("{} {} {}", verb, url, text),
            "type" => format!("{} {} {}", verb, safe_id, text),
            "click" => format!("{} {}", verb, safe_id),
            "scroll" | "press" => {
                let arg = if !text.is_empty() { text } else { safe_id };
                format!("{} {}", verb, arg)
            }
            "wait" => format!("{} {}", verb, safe_id),
            _ => verb.to_string(), // extract, wait_idle, read, etc
        };

        final_cmd.trim().to_string()
    } else {
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
        "osascript" => {
            let (stdout, stderr, success) = run_osascript(&cmd_str);
            Some(DispatchResult {
                stdout,
                stderr,
                success,
                route: "osascript".to_string(),
            })
        }
        "shell" => {
            let (stdout, stderr, success) = run_shell(&cmd_str);
            Some(DispatchResult {
                stdout,
                stderr,
                success,
                route: "shell".to_string(),
            })
        }
        "finish" => Some(DispatchResult {
            stdout: cmd_str,
            stderr: String::new(),
            success: true,
            route: "agent".to_string(),
        }),
        _ => None,
    }
}
