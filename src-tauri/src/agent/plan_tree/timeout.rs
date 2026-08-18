/// 默认各类工具/动作的超时时间 (秒)
pub fn default_timeout_for_tool(tool_name: &str) -> u64 {
    let t = tool_name.to_lowercase();
    if t.contains("search") {
        15
    } else if t.contains("fetch") || t.contains("download") || t.contains("http") {
        25
    } else if t.contains("bash") || t.contains("exec") || t.contains("shell") || t.contains("cmd") {
        60
    } else if t.contains("llm") || t.contains("summarize") || t.contains("ai") {
        90
    } else {
        30 // 通用默认 30s
    }
}
