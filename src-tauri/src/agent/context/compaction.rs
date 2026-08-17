use crate::agent::context::budget::{estimate_tokens, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use serde_json::json;

/// 离线可单测的 Summarizer Trait
pub trait Summarizer: Send + Sync {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String>;
}

/// 默认的 0 外部依赖规则型总结器（100% 永不失败的硬保底降级路径）
pub struct RuleBasedSummarizer;

impl Summarizer for RuleBasedSummarizer {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String> {
        let mut lines = Vec::new();
        for msg in messages {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let mut out_ref = String::new();
            if let Some(pos) = content_str.find("output_id: ") {
                let after = &content_str[pos + 11..];
                let id = after.lines().next().unwrap_or("").trim();
                if !id.is_empty() {
                    out_ref = format!(" (output_id={})", id);
                }
            }
            let first_line = content_str.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                let preview: String = first_line.chars().take(100).collect();
                lines.push(format!("- [{}]{} {}", msg.role, out_ref, preview));
            }
        }
        Ok(lines.join("\n"))
    }
}

impl SandwichContext {
    /// 计算当前属于历史的有效 Token 估算量
    pub fn history_estimated_tokens(&self) -> usize {
        self.turns_history
            .iter()
            .map(|m| match &m.content {
                serde_json::Value::String(s) => estimate_tokens(s),
                other => estimate_tokens(&other.to_string()),
            })
            .sum()
    }

    /// 动态计算当前会话的安全预算（全量考虑系统提示词、活跃工具手册及终极目标）
    pub fn current_budget(&self) -> ContextBudget {
        let static_prefix_tokens = estimate_tokens(&self.system_prompt)
            + estimate_tokens(&self.active_tool_detail)
            + estimate_tokens(&self.user_goal);
        ContextBudget::derive(
            self.context_window_tokens,
            ContextBudget::DEFAULT_MAX_OUTPUT_TOKENS,
            static_prefix_tokens,
        )
    }

    /// 滞后高低水位 + 规则降级压缩引擎
    pub fn maybe_compact_history(&mut self) {
        let budget = self.current_budget();
        let current_tokens = self.history_estimated_tokens();

        // 未达高水位，绝不轻易压缩（保护前缀缓存）
        if current_tokens <= budget.high_watermark_tokens {
            return;
        }

        println!(
            "📦 [Compaction Engine] 历史 ~{} tokens 触碰高水位线 ({} tokens)，启动自适应归档压缩...",
            current_tokens, budget.high_watermark_tokens
        );

        // 从前往后算出需要压缩的条数：一次性确定 compact_count，
        // 然后用 drain(0..compact_count) 单次批量移除，避免 remove(0) 反复 O(n) 移动
        let mut running_tokens: usize = 0;
        let mut compact_count = 0;
        for msg in &self.turns_history {
            let msg_tokens = match &msg.content {
                serde_json::Value::String(s) => estimate_tokens(s),
                other => estimate_tokens(&other.to_string()),
            };
            if current_tokens.saturating_sub(running_tokens) <= budget.low_watermark_tokens {
                break;
            }
            running_tokens += msg_tokens;
            compact_count += 1;
        }

        if compact_count == 0 {
            return;
        }

        // 单次 drain：O(n) 一次完成，无论删多少条都只移动一次内存
        let to_compact: Vec<ChatMessage> = self.turns_history.drain(0..compact_count).collect();

        // 使用零依赖规则型生成器进行安全归档（永不崩溃）
        let summarizer = RuleBasedSummarizer;
        let archive_text = summarizer
            .summarize(&to_compact)
            .unwrap_or_else(|_| "- [历史执行记录已安全落盘 SQLite]".to_string());

        if !archive_text.is_empty() {
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档（已定型压缩，完整原文可通过 output_id 查询）】\n{}\n（已归档 {} 条步骤，当前上下文窗口: {} tokens）",
                    archive_text,
                    to_compact.len(),
                    budget.context_window_tokens
                )),
            };
            self.turns_history.insert(0, compact_card);
        }

        let after_tokens = self.history_estimated_tokens();
        println!(
            "✅ [Compaction Engine] 压缩完成：历史 Tokens 从 {} 降至 {} (低水位目标: {})",
            current_tokens, after_tokens, budget.low_watermark_tokens
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_based_summarizer_never_fails() {
        let summarizer = RuleBasedSummarizer;
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("计划: 测试 | 工具: shell_exec | 动作: cargo test"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【执行输出】\noutput_id: 89a7\nFinished 0 errors"),
            },
        ];

        let summary = summarizer.summarize(&messages).unwrap();
        assert!(summary.contains("shell_exec"));
        assert!(summary.contains("output_id=89a7"));
    }
}
