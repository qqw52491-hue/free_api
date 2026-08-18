use crate::agent::context::budget::{estimate_tokens, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::llm::ModelConfig;
use serde_json::json;

/// 离线可单测的 Summarizer Trait
pub trait Summarizer: Send + Sync {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String>;
}

/// 默认的 0 外部依赖规则型总结器（100% 永不失败的事实索引与保底降级路径）
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
                let preview: String = first_line.chars().take(120).collect();
                lines.push(format!("- [{}]{} {}", msg.role, out_ref, preview));
            }
        }
        Ok(lines.join("\n"))
    }
}

/// 异步调用 LLM 生成语义级继续工作摘要
pub async fn generate_llm_summary(
    config: &ModelConfig,
    archived_history_text: &str,
) -> Result<String, String> {
    let client = config.build_client()?;
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let prompt = format!(
        "你是一个自主 Agent 的上下文归档与状态压缩专家。\n\
        以下是 Agent 在执行长程任务期间早期历史的流水记录（包含步骤计划、工具调用、执行输出与报错）。\n\
        请提取并生成一份高度凝练的【阶段性任务继续摘要】，严格包含以下 4 个结构化板块：\n\n\
        ## 1. 已完成事项与关键产物\n\
        - 简明列出已成功完成的阶段、生成的文件路径、创建的数据表等\n\n\
        ## 2. 关键事实与架构决策\n\
        - 列出执行中确认的关键环境信息、配置项、发现的约束条件\n\n\
        ## 3. 踩坑记录与教训\n\
        - 列出失败过的操作、错误的参数/action、服务端报错及后续如何纠正的（避免后续步骤重蹈覆辙）\n\n\
        ## 4. 未完成事项与下一步建议\n\
        - 基于前面的进展，明确指出当前尚未完成的工作点与建议方向\n\n\
        ⚠️ 注意：禁止流水账重复，只保留具有决策指导意义的硬事实与结论；如有 output_id 引用请准确保留。\n\n\
        【待压缩历史记录】\n{}",
        archived_history_text
    );

    let messages = vec![
        json!({
            "role": "system",
            "content": "你是资深的 AI Agent 上下文工程专家，擅长从复杂的执行历史中提炼高密度的状态恢复摘要。"
        }),
        json!({
            "role": "user",
            "content": prompt
        }),
    ];

    let body = json!({
        "model": config.model_name,
        "messages": messages,
        "max_tokens": 2048,
        "temperature": 0.3,
        "stream": false
    });

    let resp = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("LLM 摘要请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM 摘要 API 错误 {}: {}", status, err_text));
    }

    let json_resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 LLM 摘要返回 JSON 失败: {}", e))?;

    let content = json_resp["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "LLM 摘要返回内容为空".to_string())?
        .trim();

    if content.is_empty() {
        return Err("LLM 摘要返回为空文本".to_string());
    }

    Ok(content.to_string())
}

/// 双层混合总结器：
/// 第一层：Rust 提取确定性事实索引 (output_id、工具、路径、状态)
/// 第二层：LLM 提炼语义摘要 (目标、决策、教训、待办)；失败时优雅降级
pub struct HybridSummarizer<'a> {
    pub model_config: Option<&'a ModelConfig>,
}

impl<'a> Summarizer for HybridSummarizer<'a> {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String> {
        let rule_index = RuleBasedSummarizer.summarize(messages)?;

        // 尝试通过 LLM 生成语义摘要
        let mut llm_summary_opt: Option<String> = None;
        if let Some(cfg) = self.model_config {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let cfg_clone = cfg.clone();
                let raw_text = rule_index.clone();
                let llm_res = tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        generate_llm_summary(&cfg_clone, &raw_text).await
                    })
                });
                match llm_res {
                    Ok(summary) => {
                        println!("🧠 [Compaction Engine] LLM 语义摘要生成成功 ({} 字符)", summary.len());
                        llm_summary_opt = Some(summary);
                    }
                    Err(e) => {
                        println!("⚠️ [Compaction Engine] LLM 摘要失败，平滑降级至规则事实索引: {}", e);
                    }
                }
            }
        }

        if let Some(llm_summary) = llm_summary_opt {
            Ok(format!(
                "### 🤖 AI 任务推进与状态摘要\n{}\n\n### 📋 关键事实索引（完整原文可通过 output_id 从 SQLite 查询）\n{}",
                llm_summary.trim(),
                rule_index.trim()
            ))
        } else {
            Ok(rule_index)
        }
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

    /// 动态计算当前会话的安全预算（顶层 System 提示词 100% 静态锁死）
    pub fn current_budget(&self) -> ContextBudget {
        let static_prefix_tokens = estimate_tokens(&self.system_prompt);
        ContextBudget::derive(
            self.context_window_tokens,
            ContextBudget::DEFAULT_MAX_OUTPUT_TOKENS,
            static_prefix_tokens,
        )
    }

    /// 滞后高低水位 + 双层混合压缩引擎（AI 语义摘要 + 规则事实索引）
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

        // ── 目标提升：扫描被压缩的历史，找到最后一条目标变更事件 ──────────────────────────
        // 约定格式：mod.rs 追加目标变更事件时使用 "【任务目标更新】\n<new_goal>" 标记
        // compaction 找到最新的一条后，将其提升到 ctx.user_goal，使下个 epoch 的 M1 自动更新
        const GOAL_UPDATE_MARKER: &str = "【任务目标更新】";
        let promoted_goal: Option<String> = to_compact.iter().rev().find_map(|msg| {
            let text = match &msg.content {
                serde_json::Value::String(s) => s.as_str(),
                _ => return None,
            };
            if let Some(pos) = text.find(GOAL_UPDATE_MARKER) {
                // 提取标记后第一行的有效目标文本
                let after_marker = &text[pos + GOAL_UPDATE_MARKER.len()..];
                let goal = after_marker
                    .trim_start_matches('\n')
                    .lines()
                    .next()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string());
                goal
            } else {
                None
            }
        });
        if let Some(new_goal) = promoted_goal {
            println!(
                "🎯 [Compaction] 提升最新任务目标到 M1 静态区: {:?}",
                new_goal
            );
            self.user_goal = new_goal;
        }
        // ────────────────────────────────────────────────────────────────────────────────────

        // 使用双层混合总结器进行安全归档（AI 语义摘要 + 确定性事实索引，永不崩溃）
        let summarizer = HybridSummarizer {
            model_config: self.compaction_model_config.as_ref(),
        };
        let archive_text = summarizer
            .summarize(&to_compact)
            .unwrap_or_else(|_| "- [历史执行记录已安全落盘 SQLite]".to_string());

        if !archive_text.is_empty() {
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档（双层压缩：已定型压缩，完整原文可通过 output_id 查询）】\n{}\n（已归档 {} 条历史消息，当前上下文窗口: {} tokens）",
                    archive_text,
                    to_compact.len(),
                    budget.context_window_tokens
                )),
            };
            self.turns_history.insert(0, compact_card);
        }

        // 压缩断代：把旧工具已加载标记清空，若新周期继续使用会自动重新追加最新手册
        self.fetched_tools.clear();

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

    #[test]
    fn test_hybrid_summarizer_falls_back_gracefully() {
        let summarizer = HybridSummarizer { model_config: None };
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("计划: 依赖分析 | 工具: remote-hub | 动作: read_file"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【执行输出】\noutput_id: c8db17f5\nCargo.toml contents..."),
            },
        ];

        let summary = summarizer.summarize(&messages).unwrap();
        assert!(summary.contains("remote-hub"));
        assert!(summary.contains("output_id=c8db17f5"));
    }

    #[test]
    fn test_compaction_promotes_latest_goal_to_m1() {
        use crate::agent::context::types::{ChatMessage, SandwichContext};

        let mut ctx = SandwichContext::new("SYS".to_string(), "初始目标：审计项目依赖".to_string());
        ctx.context_window_tokens = 8000;

        // 初始大量历史
        let big = "X".repeat(400);
        for i in 0..40 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("Step {}: {}", i, big)),
            });
        }

        // 模拟用户在第 20 步后更新了任务目标（append 事件到历史流水）
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【任务目标更新】\n生成执行报告并写入 report.xlsx\n（原目标: 审计项目依赖）"),
        });

        // 更多后续历史
        for i in 40..60 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("Step {}: {}", i, big)),
            });
        }

        // 触发压缩
        ctx.maybe_compact_history();

        // 压缩后 user_goal 应被提升为最新目标
        assert_eq!(
            ctx.user_goal,
            "生成执行报告并写入 report.xlsx",
            "compaction 应从历史事件中提取并提升最新任务目标到 M1"
        );
    }
}
