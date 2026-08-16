use crate::agent::context::types::{ChatMessage, SandwichContext};
use serde_json::json;

impl SandwichContext {
    /// 水位线批量压缩老历史记录为不可变的定型单行摘要
    pub fn compact_old_history(&mut self, compact_count: usize) {
        if self.turns_history.len() <= compact_count {
            return;
        }
        let to_compact: Vec<ChatMessage> = self.turns_history.drain(0..compact_count).collect();
        let mut compacted_lines = Vec::new();
        for msg in to_compact {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let first_line = content_str.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                let line_preview: String = first_line.chars().take(120).collect();
                compacted_lines.push(format!("- [{}] {}", msg.role, line_preview));
            }
        }

        if !compacted_lines.is_empty() {
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档 (已定型压缩)】\n{}\n(早期执行摘要已归档，当前窗口专注于近期步骤与终极目标)",
                    compacted_lines.join("\n")
                )),
            };
            self.turns_history.insert(0, compact_card);
        }
    }
}
