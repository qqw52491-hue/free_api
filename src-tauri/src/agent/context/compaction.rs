use crate::agent::context::types::{ChatMessage, SandwichContext};
use serde_json::json;

/// 中英文混合内容的字符数 → Token 估算系数
/// 纯代码: char/4；纯中文: char*1.5；混合取中间值 3.5（稍微高估，保证安全）
const CHARS_PER_TOKEN: f64 = 3.5;

/// 历史消息占整个上下文窗口的比例上限
/// 剩余留给系统提示词、状态卡、输出余量
const HISTORY_BUDGET_RATIO: f64 = 0.60;
/// 高水位：历史占用超过分配预算的 80% 时触发压缩
const HIGH_WATERMARK_RATIO: f64 = 0.80;
/// 低水位：压缩目标，降至分配预算的 50%
const LOW_WATERMARK_RATIO: f64 = 0.50;
/// 未配置 context_window 时的保守默认小窗口（32K tokens）
const DEFAULT_CONTEXT_WINDOW: usize = 32_768;

fn history_char_count(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|m| match &m.content {
            serde_json::Value::String(s) => s.len(),
            other => other.to_string().len(),
        })
        .sum()
}

impl SandwichContext {
    /// 计算当前属于历史的有效 Token 估算量
    pub fn history_estimated_tokens(&self) -> usize {
        let chars = history_char_count(&self.turns_history);
        (chars as f64 / CHARS_PER_TOKEN).ceil() as usize
    }

    /// 根据模型上下文窗口动态计算压缩阈值（单位：Token）
    fn history_budget_tokens(&self) -> usize {
        let window = if self.context_window_tokens > 0 {
            self.context_window_tokens
        } else {
            DEFAULT_CONTEXT_WINDOW
        };
        (window as f64 * HISTORY_BUDGET_RATIO) as usize
    }

    /// 滞后满载检查 + 请求外层在 add_step 后调用。
    /// 当历史 Token 超过高水位时，自动将最早的消息批量定型压缩为不可变的单行摘要。
    pub fn maybe_compact_history(&mut self) {
        let budget = self.history_budget_tokens();
        let high_mark = (budget as f64 * HIGH_WATERMARK_RATIO) as usize;
        let low_mark = (budget as f64 * LOW_WATERMARK_RATIO) as usize;

        let current = self.history_estimated_tokens();
        if current <= high_mark {
            return; // 在安全范围内，无需压缩
        }

        println!(
            "📦 [Hysteresis Compact] 历史 ~{} tokens 超过高水位 {} tokens，开始定型压缩...",
            current, high_mark
        );

        let mut to_compact: Vec<ChatMessage> = Vec::new();
        // 自前往后移除最早的消息，直到降至低水位
        while !self.turns_history.is_empty() {
            if self.history_estimated_tokens() <= low_mark {
                break;
            }
            to_compact.push(self.turns_history.remove(0));
        }

        if to_compact.is_empty() {
            return;
        }

        // 生成定型归档卡：每条消息取第一行预览，截到 120 字符
        let mut lines = Vec::new();
        for msg in &to_compact {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let first_line = content_str.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                let preview: String = first_line.chars().take(120).collect();
                lines.push(format!("- [{}] {}", msg.role, preview));
            }
        }

        if !lines.is_empty() {
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档（已定型压缩，完整原文可通过 output_id + get_step_output 查询）】\n{}\n（归档 {} 条，当前窗口周期: {} tokens）",
                    lines.join("\n"),
                    to_compact.len(),
                    self.context_window_tokens
                )),
            };
            self.turns_history.insert(0, compact_card);
        }

        println!(
            "✅ [Compact 完成] {} 条 → 1 归档卡，压缩后历史约 {} tokens（预算 {} tokens）",
            to_compact.len(),
            self.history_estimated_tokens(),
            budget
        );
    }

    /// 已废弃：保留签名实现兼容旧调用者（目前未使用）
    pub fn compact_old_history(&mut self, _compact_count: usize) {
        self.maybe_compact_history();
    }
}
