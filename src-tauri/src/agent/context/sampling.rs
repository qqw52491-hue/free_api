use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

impl SandwichContext {
    pub fn add_step(
        &mut self,
        instruction: &AgentInstruction,
        output_summary: &str,
        output_id: &str,
    ) {
        // 脱水版行动纪要：只记录"做了什么"，砍掉 thought/reflection 内心戏，大幅节省 Token
        // AI 下一步只需要知道历史里发生了什么动作，完全不需要重读上一轮的心理分析
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

        // 历史对话中的执行结果采用智能 Head + Tail 采样（头 300 + 尾 500 + 关键报错行保留）
        let mut history_feedback = output_summary.to_string();
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
                "【执行输出采样】\noutput_id: {}\noriginal_chars: {}\ntruncated: true\n{}\n\n[省略了部分字符，如需查看完整输出，请使用 internal_tool: get_step_output，参数: {{\"output_id\": \"{}\"}}]",
                output_id, original_chars, history_feedback, output_id
            )
        } else {
            format!(
                "【执行输出】\noutput_id: {}\n{}",
                output_id, history_feedback
            )
        };

        let feedback = ChatMessage {
            role: "user".to_string(),
            content: json!(feedback_str),
        };
        self.turns_history.push(feedback);

        // 水位线滞后压缩（Hysteresis Auto-Compact）：
        // 基于实际字符数估算 Token 用量，超过高水位时自动批量归档最早的历史记录。
        // 压缩后的每条记录仍保留 output_id 引用，AI 可通过 get_step_output 溯源完整原文。
        self.maybe_compact_history();
    }
}
