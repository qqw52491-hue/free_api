use std::process::Command;

pub fn run_shell(cmd: &str) -> (String, String, bool) {
    let result = Command::new("bash").arg("-c").arg(cmd).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn run_osascript(script: &str) -> (String, String, bool) {
    let result = Command::new("osascript").arg("-e").arg(script).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn chars_preview(s: &str, limit: usize) -> String {
    let s = s.replace("\n", " ");
    if s.chars().count() > limit {
        let truncated: String = s.chars().take(limit).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}

/// Robustly extract JSON object from AI reply.
pub fn extract_json_from_text(text: &str) -> Option<String> {
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if serde_json::from_str::<serde_json::Value>(cleaned).is_ok() {
        return Some(cleaned.to_string());
    }

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
            if ch == '{' {
                depth += 1;
            }
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
        if serde_json::from_str::<serde_json::Value>(&json_str).is_ok() {
            return Some(json_str);
        }
    }

    None
}
