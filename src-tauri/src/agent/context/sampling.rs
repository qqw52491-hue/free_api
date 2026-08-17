use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

pub const SAMPLER_VERSION: u32 = 1;
pub const MAX_INGRESS_CHARS: usize = 12_000; // ~4,000 tokens 硬上限防爆门

impl SandwichContext {
    /// 写入即定型 (Immutable on Write)
    /// 工具执行输出在产生的瞬间完成采样与结构化，生成不可变记录追加进历史流水，
    /// 绝对禁止在后续组装 Prompt 时重复动态采样（防止破坏前缀缓存）。
    pub fn add_step(
        &mut self,
        instruction: &AgentInstruction,
        output_summary: &str,
        output_id: &str,
    ) {
        // 脱水版行动纪要：只记录"做了什么"，砍掉 thought/reflection 内心戏，大幅节省 Token
        let action_brief = format!(
            "计划: {} | 工具: {} | 动作: {}",
            self.active_plan_title.as_deref().unwrap_or("未命名计划"),
            instruction.get_tool(),
            instruction.description,
        );
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(action_brief),
        };
        self.turns_history.push(msg);

        // 1. 入口硬截断 (Ingress Hard-Cap: 单步超过 12,000 字符立刻硬截断，原文已在 SQLite)
        let ingress_trimmed = if output_summary.chars().count() > MAX_INGRESS_CHARS {
            let truncated_head: String = output_summary.chars().take(MAX_INGRESS_CHARS).collect();
            format!("{}\n\n[⚠️ 入口硬截断: 单步输出超过 4,000 Tokens 上限，完整原始数据请使用 get_step_output(output_id=\"{}\")]", truncated_head, output_id)
        } else {
            output_summary.to_string()
        };

        // 2. 智能 Head + Tail 采样（头 300 + 尾 500 + 关键报错行保留）
        let mut history_feedback = ingress_trimmed;
        let original_chars = history_feedback.chars().count();
        let mut is_truncated = false;

        if let Some(idx) = history_feedback.find("【可用元素清单】") {
            history_feedback
                .replace_range(idx.., "【可用元素清单】: (已折叠，详见底部当前最新观测)");
            is_truncated = true;
        } else {
            let sampled = crate::agent::utils::sample_execution_log(&history_feedback, 300, 500);
            if sampled.len() < history_feedback.len() {
                history_feedback = sampled;
                is_truncated = true;
            }
        }

        let feedback_str = if is_truncated {
            format!(
                "【执行输出采样 (v{})】\noutput_id: {}\noriginal_chars: {}\ntruncated: true\n{}\n\n[省略了部分字符。完整原始输出已落盘至本地文本文件 (outputs/{}.log)，可使用 grep/read 或 internal_tool: get_step_output(output_id=\"{}\") 精准查看]",
                SAMPLER_VERSION, output_id, original_chars, history_feedback, output_id, output_id
            )
        } else {
            format!(
                "【执行输出 (v{})】\noutput_id: {}\n{}",
                SAMPLER_VERSION, output_id, history_feedback
            )
        };

        let feedback = ChatMessage {
            role: "user".to_string(),
            content: json!(feedback_str),
        };
        self.turns_history.push(feedback);

        // 触发自适应水位检查
        self.maybe_compact_history();
    }
}
