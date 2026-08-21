use crate::agent::context::budget::{conservative_token_estimate, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

pub const SAMPLER_VERSION: u32 = 4;

fn targeted_verification_guidance(
    output_id: &str,
    original_tokens: usize,
    ingress_cap: usize,
) -> String {
    format!(
        "[宿主提示：本次执行输出约 {} Tokens，超过单条上下文上限 {} Tokens，已保存为工件 output_id={} 并仅展示采样。当前片段不能证明未展示部分的细节；但无需通读全文。只有当结论确实依赖缺失内容时，才按文件路径、函数/符号、错误码、关键词或 diff hunk 定向搜索，再通过 task_store/search_step_output 或带 offset 的 get_step_output 获取最小必要片段。大范围回查会消耗工作集预算。]",
        original_tokens, ingress_cap, output_id
    )
}

impl SandwichContext {
    /// 写入工具执行结果（Append-only 线性历史追加，最大化 Prompt Cache 命中率）。
    ///
    /// 设计原则：
    /// - 历史在平时保持严格 Append-Only，避免在历史中间随机篡改而破坏前缀缓存。
    /// - 单步入口依据保守 Token 估算；超过上限时保留头尾采样，并把完整原文持久化为工件。
    /// - 截断仅表示当前证据不完整，不强迫模型通读原文；应先按符号、关键词或行范围定向核验。
    pub fn add_step(
        &mut self,
        instruction: &AgentInstruction,
        output_summary: &str,
        output_id: &str,
    ) {
        let action_brief = format!(
            "计划: {} | 工具: {} | 动作: {}",
            self.active_plan_title().unwrap_or("未命名计划"),
            instruction.get_tool(),
            instruction.description,
        );
        self.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(action_brief),
        });

        let ingress_tokens = conservative_token_estimate(output_summary);
        let ingress_cap = ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP;
        let char_count = output_summary.chars().count();
        let token_char_ratio = if char_count > 0 {
            ingress_tokens as f64 / char_count as f64
        } else {
            1.0
        };

        let feedback_str = if ingress_tokens > ingress_cap {
            let safe_cap_tokens = ingress_cap.saturating_sub(600) as f64;
            let safe_cap_chars = (safe_cap_tokens / token_char_ratio).max(100.0) as usize;
            let mut head_chars = (safe_cap_chars as f64 * 0.4) as usize;
            let mut tail_chars = (safe_cap_chars as f64 * 0.6) as usize;
            let mut sampled =
                crate::agent::utils::sample_execution_log(output_summary, head_chars, tail_chars);
            let mut candidate = format!(
                "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n{}",
                SAMPLER_VERSION,
                output_id,
                ingress_tokens,
                sampled,
                targeted_verification_guidance(output_id, ingress_tokens, ingress_cap),
            );

            while conservative_token_estimate(&candidate) > ingress_cap
                && (head_chars > 100 || tail_chars > 150)
            {
                head_chars = (head_chars as f64 * 0.70) as usize;
                tail_chars = (tail_chars as f64 * 0.70) as usize;
                sampled = crate::agent::utils::sample_execution_log(
                    output_summary,
                    head_chars,
                    tail_chars,
                );
                candidate = format!(
                    "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n{}",
                    SAMPLER_VERSION,
                    output_id,
                    ingress_tokens,
                    sampled,
                    targeted_verification_guidance(output_id, ingress_tokens, ingress_cap),
                );
            }
            candidate
        } else {
            format!(
                "【执行输出 (v{})】\noutput_id: {}\n{}",
                SAMPLER_VERSION, output_id, output_summary
            )
        };

        self.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(feedback_str),
        });
        self.maybe_compact_history();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_guidance_requires_targeted_evidence_not_full_replay() {
        let guidance = targeted_verification_guidance("out-1", 10_000, 4_000);
        assert!(guidance.contains("无需通读全文"));
        assert!(guidance.contains("定向搜索"));
        assert!(guidance.contains("output_id=out-1"));
        assert!(!guidance.contains("必须首先"));
        assert!(!guidance.contains("绝对禁止"));
    }
}
