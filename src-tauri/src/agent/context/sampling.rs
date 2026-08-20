use crate::agent::context::budget::{conservative_token_estimate, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

pub const SAMPLER_VERSION: u32 = 3;

impl SandwichContext {
    /// 写入工具执行结果（Append-only 线性历史追加，最大化 Prompt Cache 命中率）
    ///
    /// 设计原则（对齐 Claude Code / Codex 长任务架构）：
    /// - 历史在平时保持严格 Append-Only，绝不在历史中间就地随机篡改，避免打爆 KV 前缀缓存
    /// - 单步入口依据 Token 精确计量（支持中文 CJK 准确评估），超过 Cap 时采用 Head (1200) + Errors (800) + Tail (1800) 显式分账采样
    /// - 全量原文始终持久化在 SQLite，凭 output_id 随时可通过 get_step_output(offset, limit) 分段回取
    pub fn add_step(
        &mut self,
        instruction: &AgentInstruction,
        output_summary: &str,
        output_id: &str,
    ) {
        // ── 1. 写入 assistant 行动纪要 ──
        // 脱水版：只记录"做了什么"，砍掉 thought/reflection 内心戏，大幅节省 Token
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

        // ── 2. 入口硬截断 (基于悲观安全 Token 估算，支持中文/英文/密集 JSON/代码) ──
        let ingress_tokens = conservative_token_estimate(output_summary);
        let ingress_cap = ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP;

        let char_count = output_summary.chars().count();
        let token_char_ratio = if char_count > 0 {
            ingress_tokens as f64 / char_count as f64
        } else {
            1.0
        };

        let feedback_str = if ingress_tokens > ingress_cap {
            let safe_cap_tokens = ingress_cap.saturating_sub(600) as f64; // Reserve for boilerplate & errors
            let safe_cap_chars = (safe_cap_tokens / token_char_ratio).max(100.0) as usize;
            let mut head_chars = (safe_cap_chars as f64 * 0.4) as usize;
            let mut tail_chars = (safe_cap_chars as f64 * 0.6) as usize;

            let mut sampled = crate::agent::utils::sample_execution_log(output_summary, head_chars, tail_chars);
            let mut candidate = format!(
                "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n[⚠️ 入口硬截断: 原始输出约 {} Tokens (上限 {} Tokens)。完整数据已在 SQLite。\n⛔ 严格纪律警告: 结果包含 truncated: true，【绝对禁止】基于以上不完整的前缀/后缀信息直接下结论或认定任务成功！遇到此标记，你必须首先使用 get_step_output 或分页参数续读中间缺失的关键部分！\n🔍 搜索关键词: {{\"tool\": \"task_store\", \"command\": {{\"action\": \"search_step_output\", \"output_id\": \"{}\", \"pattern\": \"error\", \"context_lines\": 3}}}}\n📑 分段读取: {{\"tool\": \"task_store\", \"command\": {{\"action\": \"get_step_output\", \"output_id\": \"{}\", \"offset\": 0, \"limit\": 3000}}}}] ",
                SAMPLER_VERSION, output_id, ingress_tokens, sampled, ingress_tokens, ingress_cap, output_id, output_id
            );

            // 闭环出口复检 (Sample -> Measure -> Trim 闭环保证)：
            while conservative_token_estimate(&candidate) > ingress_cap && (head_chars > 100 || tail_chars > 150) {
                head_chars = (head_chars as f64 * 0.70) as usize;
                tail_chars = (tail_chars as f64 * 0.70) as usize;
                sampled = crate::agent::utils::sample_execution_log(output_summary, head_chars, tail_chars);
                candidate = format!(
                    "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n[⚠️ 入口硬截断: 原始输出约 {} Tokens (上限 {} Tokens)。完整数据已在 SQLite。\n⛔ 严格纪律警告: 结果包含 truncated: true，【绝对禁止】基于以上不完整的前缀/后缀信息直接下结论或认定任务成功！遇到此标记，你必须首先使用 get_step_output 或分页参数续读中间缺失的关键部分！\n🔍 搜索关键词: {{\"tool\": \"task_store\", \"command\": {{\"action\": \"search_step_output\", \"output_id\": \"{}\", \"pattern\": \"error\", \"context_lines\": 3}}}}\n📑 分段读取: {{\"tool\": \"task_store\", \"command\": {{\"action\": \"get_step_output\", \"output_id\": \"{}\", \"offset\": 0, \"limit\": 3000}}}}] ",
                    SAMPLER_VERSION, output_id, ingress_tokens, sampled, ingress_tokens, ingress_cap, output_id, output_id
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

        // ── 3. 触发自适应水位检查（仅在高水位时从头部一次性批量收纳，保护前缀缓存）──
        self.maybe_compact_history();
    }
}
