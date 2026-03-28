use crate::agent::types::DispatchResult;
use crate::agent::browser::run_browser_dom;
use crate::agent::utils::{run_shell, run_osascript};

const BROWSER_VERBS: &[&str] = &[
    "goto", "navigate", "extract", "look", "click", "type", "press", "read", "scroll", "hover", "select", "wait", "wait_for", "back", "forward", "refresh", "url", "tab_url", "eval", "js", "screenshot"
];

pub fn run_builtin_step(action: &str, params: &serde_json::Value) -> Option<DispatchResult> {
    let action_low = action.to_lowercase();
    
    // 1. 尝试还原指令字符串
    let cmd_str = if let Some(s) = params.as_str() {
        s.to_string()
    } else if params.is_object() {
        // 如果 AI 给了个对象 {"id": 12, "text": "xxx"}, 尝试拼成字符串
        let id = params.get("id").or(params.get("element_id")).and_then(|v| v.as_u64().or(v.as_str().and_then(|s| s.parse().ok()))).map(|n| n.to_string()).unwrap_or_default();
        let text = params.get("text").or(params.get("val")).or(params.get("command")).and_then(|v| v.as_str()).unwrap_or_default();
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or_default();
        
        format!("{} {} {}", id, text, url).trim().to_string()
    } else {
        String::new()
    };

    // 2. 路由分发
    if action_low == "browser_dom" {
        let (stdout, stderr, success) = run_browser_dom(&cmd_str);
        return Some(DispatchResult { stdout, stderr, success, route: "browser".to_string() });
    }

    // 特殊：如果 action 本身就是浏览器指令名
    if BROWSER_VERBS.contains(&action_low.as_str()) {
        let full_cmd = format!("{} {}", action_low, cmd_str).trim().to_string();
        let (stdout, stderr, success) = run_browser_dom(&full_cmd);
        return Some(DispatchResult { stdout, stderr, success, route: "browser".to_string() });
    }
    
    match action_low.as_str() {
        "osascript" => {
            let (stdout, stderr, success) = run_osascript(&cmd_str);
            Some(DispatchResult { stdout, stderr, success, route: "osascript".to_string() })
        }
        "shell" => {
            let (stdout, stderr, success) = run_shell(&cmd_str);
            Some(DispatchResult { stdout, stderr, success, route: "shell".to_string() })
        }
        "finish" => {
            Some(DispatchResult { 
                stdout: cmd_str, 
                stderr: String::new(), 
                success: true, 
                route: "agent".to_string() 
            })
        }
        _ => None,
    }
}
