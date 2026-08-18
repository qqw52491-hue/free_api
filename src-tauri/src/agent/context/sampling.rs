use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

pub const SAMPLER_VERSION: u32 = 1;
pub const MAX_INGRESS_CHARS: usize = 12_000; // ~4,000 tokens 入口硬截断
pub const RECENT_TOOL_WINDOW: usize = 3; // 保留最近 K=3 个工具批次完整可见
pub const FOLD_CHAR_THRESHOLD: usize = 3_500; // 超过此字符数的旧工具输出才折叠

impl SandwichContext {
    /// 写入工具执行结果（只写入，不折叠）
    ///
    /// 设计原则（对齐 Anthropic Context Editing / OpenAI Compaction / LangChain Deep Agents）：
    /// - 当前步输出**完整写入**，模型需要完整阅读刚请求的数据
    /// - 折叠不在写入时发生，而是由 compaction 引擎在 Token 高水位触发时批量处理
    /// - 最近 K=3 个工具批次保持完整，更老的才有资格被折叠
    /// - 全文始终持久化在 SQLite，凭 output_id 随时可取回
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

        // ── 2. 入口硬截断 (12,000 字符绝对上限，防止单条输出撑爆上下文) ──
        let ingress_trimmed = if output_summary.chars().count() > MAX_INGRESS_CHARS {
            let truncated_head: String = output_summary.chars().take(MAX_INGRESS_CHARS).collect();
            format!("{}\n\n[⚠️ 入口硬截断: 单步输出超过 {} 字符上限，完整原始数据可调用: {{\"tool\": \"get_step_output\", \"command\": {{\"output_id\": \"{}\"}}}} 精准获取]",
                truncated_head, MAX_INGRESS_CHARS, output_id)
        } else {
            output_summary.to_string()
        };

        // ── 3. 当前步完整写入（不折叠）──
        let feedback_str = format!(
            "【执行输出 (v{})】\noutput_id: {}\n{}",
            SAMPLER_VERSION, output_id, ingress_trimmed
        );

        self.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(feedback_str),
        });

        // ── 4. 触发自适应水位检查（折叠和压缩都在这里按需批量执行）──
        self.maybe_compact_history();
    }

    /// Phase 1 轻量收纳：批量折叠旧工具输出
    ///
    /// 保留最近 keep_recent_k 个工具批次完整可见，更老的超长输出折叠为预览+句柄。
    /// 这是低成本操作（纯字符串处理，无 LLM 调用），在 Phase 2 全量 LLM 摘要之前优先执行。
    pub fn fold_old_tool_results(&mut self, keep_recent_k: usize) -> usize {
        // 从新到旧收集所有未折叠的完整工具输出索引
        let unfolded_indices: Vec<usize> = self
            .turns_history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, m)| {
                if m.role != "user" {
                    return false;
                }
                let s = match &m.content {
                    serde_json::Value::String(s) => s.as_str(),
                    _ => return false,
                };
                s.contains("【执行输出 (v") && !s.contains("【执行输出采样")
            })
            .map(|(i, _)| i)
            .collect();

        // 近期窗口内的批次不足 K 个，全部保留
        if unfolded_indices.len() <= keep_recent_k {
            return 0;
        }

        let mut folded_count = 0;

        // 对近期窗口之外的逐个检查并折叠
        for &idx in &unfolded_indices[keep_recent_k..] {
            let content_str = match &self.turns_history[idx].content {
                serde_json::Value::String(s) => s.clone(),
                _ => continue,
            };

            // 浏览器 DOM 元素清单：特殊语义折叠
            if let Some(fold_pos) = content_str.find("【可用元素清单】") {
                let mut folded = content_str.clone();
                folded.replace_range(
                    fold_pos..,
                    "【可用元素清单】: (已折叠，详见底部当前最新观测)",
                );
                self.turns_history[idx].content = json!(folded);
                folded_count += 1;
                continue;
            }

            // 小结果不折叠
            let char_count = content_str.chars().count();
            if char_count <= FOLD_CHAR_THRESHOLD {
                continue;
            }

            // 提取 output_id
            let output_id = content_str
                .lines()
                .find(|l| l.starts_with("output_id: "))
                .map(|l| l.trim_start_matches("output_id: ").trim().to_string())
                .unwrap_or_default();

            // Head 1200 + Tail 1500 采样
            let sampled =
                crate::agent::utils::sample_execution_log(&content_str, 1200, 1500);

            if sampled.len() < content_str.len() {
                let folded = format!(
                    "【执行输出采样 (v{})】\noutput_id: {}\noriginal_chars: {}\ntruncated: true\n{}\n\n[提示: 历史工具输出已收纳。需要完整原始数据可调用: {{\"tool\": \"get_step_output\", \"command\": {{\"output_id\": \"{}\"}}}} 精准获取]",
                    SAMPLER_VERSION, output_id, char_count, sampled, output_id
                );
                self.turns_history[idx].content = json!(folded);
                folded_count += 1;
            }
        }

        folded_count
    }
}
