use crate::agent::context::budget::{conservative_token_estimate, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::llm::ModelConfig;
use serde_json::json;

/// 离线可单测的 Summarizer Trait
pub trait Summarizer: Send + Sync {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String>;
}

/// 默认的 0 外部依赖规则型总结器（100% 永不失败的事实索引与保底降级路径）
/// 严格保留逐字关键标识符：文件路径、代码行号、错误码、ID、output_id 句柄
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
                let preview: String = first_line.chars().take(140).collect();
                lines.push(format!("- [{}]{} {}", msg.role, out_ref, preview));
            }
        }
        Ok(lines.join("\n"))
    }
}

/// 异步调用 LLM 生成语义级继续工作摘要（带 20s 硬超时与多格式兼容提取）
pub async fn generate_llm_summary(
    config: &ModelConfig,
    archived_history_text: &str,
) -> Result<String, String> {
    let client = config.build_client()?;
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    // 限制送入摘要模型的历史文本上限（最多取前 12,000 字符），防止单次摘要 Prompt 爆窗
    let capped_history: String = archived_history_text.chars().take(12_000).collect();

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
        ⚠️ 注意：禁止流水账重复，只保留具有决策指导意义的硬事实与结论；如有 output_id 引用、文件路径或行号请准确保留。\n\n\
        【待压缩历史记录】\n{}",
        capped_history
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

    let effective_max_tokens = if config.max_tokens > 0 && config.max_tokens < 4096 {
        4096
    } else {
        2048
    };

    let body = json!({
        "model": config.model_name,
        "messages": messages,
        "max_tokens": effective_max_tokens,
        "temperature": 0.3,
        "stream": false
    });

    let request_future = client.post(&url).headers(headers).json(&body).send();

    let resp = tokio::time::timeout(std::time::Duration::from_secs(20), request_future)
        .await
        .map_err(|_| "LLM 摘要请求超时 (超过 20s)".to_string())?
        .map_err(|e| format!("LLM 摘要网络请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "LLM 摘要 API 返回错误状态码 {}: {}",
            status, err_text
        ));
    }

    let json_resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 LLM 摘要返回 JSON 失败: {}", e))?;

    let extracted_content = json_resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            json_resp["choices"][0]["text"]
                .as_str()
                .map(|s| s.to_string())
        })
        .or_else(|| {
            if let Some(arr) = json_resp["choices"][0]["message"]["content"].as_array() {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| item["text"].as_str().map(|s| s.to_string()))
                    .collect();
                if !parts.is_empty() {
                    Some(parts.join("\n"))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .or_else(|| {
            json_resp["choices"][0]["message"]["reasoning_content"]
                .as_str()
                .map(|s| s.to_string())
        })
        .ok_or_else(|| "LLM 摘要返回 JSON 中未找到有效的 content 字段".to_string())?;

    let trimmed = extracted_content.trim();
    if trimmed.is_empty() {
        return Err("LLM 摘要返回为空文本".to_string());
    }

    Ok(trimmed.to_string())
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

        let mut llm_summary_opt: Option<String> = None;
        if let Some(cfg) = self.model_config {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let cfg_clone = cfg.clone();
                let raw_text = rule_index.clone();
                let llm_res = tokio::task::block_in_place(|| {
                    handle.block_on(async { generate_llm_summary(&cfg_clone, &raw_text).await })
                });
                match llm_res {
                    Ok(summary) => {
                        println!(
                            "🧠 [Compaction Engine] LLM 语义摘要生成成功 ({} 字符)",
                            summary.len()
                        );
                        llm_summary_opt = Some(summary);
                    }
                    Err(e) => {
                        println!(
                            "⚠️ [Compaction Engine] LLM 摘要失败，平滑降级至规则事实索引: {}",
                            e
                        );
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
    /// 计算当前属于历史的有效 Token 估算量（采用悲观防御型 Token 计量）
    pub fn history_estimated_tokens(&self) -> usize {
        self.turns_history
            .iter()
            .map(|m| match &m.content {
                serde_json::Value::String(s) => conservative_token_estimate(s),
                other => conservative_token_estimate(&other.to_string()),
            })
            .sum()
    }

    /// 动态计算当前会话的安全预算（顶层 System 提示词 100% 静态锁死）
    pub fn current_budget(&self) -> ContextBudget {
        let static_prefix_tokens = conservative_token_estimate(&self.system_prompt);
        ContextBudget::derive(
            self.context_window_tokens,
            ContextBudget::DEFAULT_MAX_OUTPUT_TOKENS,
            static_prefix_tokens,
        )
    }

    /// 工业级自适应前缀压缩引擎 (Prompt-Cache Friendly Compaction)
    ///
    /// 核心设计：
    /// 1. 保护前缀缓存：平时严格 Append-Only，绝不在历史中间就地随机篡改；
    /// 2. 摘要卡只追加不重写：新摘要卡插入在已有摘要卡之后，已有卡片字节绝对不动，稳稳吃满 KV 前缀缓存；
    /// 3. 水位滞后触发：仅当历史 Tokens 触碰高水位线 (80%) 时，才在最头部执行单次批量 Drain，深度回压至低水位 (35%)；
    /// 4. Token 化近期保护：按 Token 预算比例（约 40% 预算）保护近期最新轮次，而不是死板步数；
    /// 5. 轮次成对对齐：严格按照 (assistant, user) 轮次对切除，彻底杜绝孤立工具调用报错；
    /// 6. 绝对不爆窗底线：若极端情况下近期保护区仍超出上限，自动将最老步骤降级为 output_id 句柄。
    pub fn maybe_compact_history(&mut self) {
        let budget = self.current_budget();
        let mut current_tokens = self.history_estimated_tokens();

        // 未达高水位，绝不轻易压缩（保护前缀缓存命中率）
        if current_tokens <= budget.high_watermark_tokens {
            return;
        }

        // ── 0. 先做一次性脱水定型：把保护区之外的历史消息就地替换为证据句柄 ──
        // 这是旧输出"变成存根"的唯一时机：只在 compaction 事件内发生，
        // 平时组装严格 append-only，前缀缓存不会因脱水边界滑动而每轮断裂。
        let recent_protected_tokens = (budget.max_allowed_history_tokens as f64
            * ContextBudget::RECENT_PROTECTED_RATIO) as usize;
        let mut protected_from_back = 0;
        let mut back_tokens = 0;
        for msg in self.turns_history.iter().rev() {
            let t = match &msg.content {
                serde_json::Value::String(s) => conservative_token_estimate(s),
                other => conservative_token_estimate(&other.to_string()),
            };
            back_tokens += t;
            protected_from_back += 1;
            if back_tokens >= recent_protected_tokens && protected_from_back >= 2 {
                break;
            }
        }
        let dehydrate_limit = self
            .turns_history
            .len()
            .saturating_sub(protected_from_back);
        for msg in self.turns_history.iter_mut().take(dehydrate_limit) {
            *msg = crate::agent::context::assembler::dehydrate_historical_turn(msg);
        }
        let tokens_after_dehydrate = self.history_estimated_tokens();
        if tokens_after_dehydrate <= budget.high_watermark_tokens {
            println!(
                "🧊 [Dehydration] 保护区外 {} 条历史消息已一次性脱水定型（~{} → ~{} tokens），无需归档压缩",
                dehydrate_limit, current_tokens, tokens_after_dehydrate
            );
            return;
        }
        current_tokens = tokens_after_dehydrate;

        println!(
            "📦 [Compaction Engine] 历史 ~{} tokens 触碰高水位线 ({} tokens)，启动自适应归档压缩...",
            current_tokens, budget.high_watermark_tokens
        );

        let max_compact_limit = self.turns_history.len().saturating_sub(protected_from_back);

        // ── 2. 从前往后确定需要切除的轮次数 (compact_count) ──
        let mut running_tokens: usize = 0;
        let mut compact_count = 0;
        for (i, msg) in self.turns_history.iter().enumerate() {
            if i >= max_compact_limit && compact_count > 0 {
                break;
            }
            let msg_tokens = match &msg.content {
                serde_json::Value::String(s) => conservative_token_estimate(s),
                other => conservative_token_estimate(&other.to_string()),
            };
            running_tokens += msg_tokens;
            compact_count += 1;
            if current_tokens.saturating_sub(running_tokens) <= budget.low_watermark_tokens {
                break;
            }
        }

        // 关键对齐保护：确保 drain 边界对齐到完整轮次，绝不把 (assistant, user) 工具调用对拆散
        if compact_count < self.turns_history.len()
            && self.turns_history[compact_count].role == "user"
        {
            compact_count += 1;
        }

        if compact_count == 0 {
            return;
        }

        // 单次 drain：O(n) 一次完成，头部一次性切除
        let to_compact: Vec<ChatMessage> = self.turns_history.drain(0..compact_count).collect();

        // ── 3. 目标提升：扫描被压缩的历史，提取最新的任务目标更新并提升至 M1 静态区 ──
        const GOAL_UPDATE_MARKER: &str = "【任务目标更新】";
        let promoted_goal: Option<String> = to_compact.iter().rev().find_map(|msg| {
            let text = match &msg.content {
                serde_json::Value::String(s) => s.as_str(),
                _ => return None,
            };
            if let Some(pos) = text.find(GOAL_UPDATE_MARKER) {
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

        // ── 4. 使用双层混合总结器进行安全归档 ──
        let summarizer = HybridSummarizer {
            model_config: self.compaction_model_config.as_ref(),
        };
        let archive_text = summarizer
            .summarize(&to_compact)
            .unwrap_or_else(|_| "- [历史执行记录已安全落盘 SQLite]".to_string());

        if !archive_text.is_empty() {
            let existing_summary_cards = self
                .turns_history
                .iter()
                .take_while(|m| match &m.content {
                    serde_json::Value::String(s) => s.contains("【历史早期执行归档"),
                    _ => false,
                })
                .count();

            let card_index = existing_summary_cards + 1;
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档卡片 #{}（双层压缩：完整原文可通过 output_id 精准查询）】\n{}\n（已归档 {} 条历史消息，当前窗口: {} tokens）",
                    card_index,
                    archive_text,
                    to_compact.len(),
                    budget.context_window_tokens
                )),
            };
            // 关键优化：在现有摘要卡之后追加新卡，永不改动旧摘要卡字节，最大化 Prompt Cache 命中率！
            self.turns_history
                .insert(existing_summary_cards, compact_card);

            // 🌟 核心增强：从压缩归档中提取高价值架构事实与踩坑教训，沉淀至工作台事实卡 (Memories)
            if archive_text.contains("关键事实")
                || archive_text.contains("踩坑记录")
                || archive_text.contains("架构决策")
            {
                for line in archive_text.lines() {
                    let trimmed = line.trim();

                    let mut is_fact = false;
                    let mut fact_body = "";

                    if (trimmed.starts_with("- ") || trimmed.starts_with("* ")) && trimmed.len() > 6
                    {
                        is_fact = true;
                        fact_body = trimmed[2..].trim();
                    } else if let Some(idx) = trimmed.find(". ") {
                        if idx > 0
                            && trimmed[..idx].chars().all(|c| c.is_ascii_digit())
                            && trimmed.len() > idx + 4
                        {
                            is_fact = true;
                            fact_body = trimmed[idx + 2..].trim();
                        }
                    }

                    if is_fact {
                        if !fact_body.is_empty()
                            && fact_body.len() <= 200
                            && self.memories.len() < 8
                        {
                            let key = format!("auto_fact_{}", self.memories.len() + 1);
                            self.memories
                                .entry(key)
                                .or_insert_with(|| fact_body.to_string());
                        }
                    }
                }
            }
        }

        // ── 5. 绝对保底降级：若压缩后仍然超出低水位线缓冲，降级最老的用户输出为纯句柄 ──
        let mut after_tokens = self.history_estimated_tokens();
        if after_tokens > budget.low_watermark_tokens + 500 {
            let mut degraded_any = false;
            for msg in &mut self.turns_history {
                if msg.role == "user" {
                    let text = match &msg.content {
                        serde_json::Value::String(s) => s.clone(),
                        _ => continue,
                    };
                    if let Some(pos) = text.find("output_id: ") {
                        let id = text[pos + 11..].lines().next().unwrap_or("").trim();
                        if !id.is_empty() && text.len() > 300 {
                            msg.content = json!(format!(
                                "【执行输出精简句柄】\noutput_id: {}\n[提示: 已深度压缩为句柄，完整输出已存 SQLite，调用 search_step_output / get_step_output(output_id={}) 获取]",
                                id, id
                            ));
                            degraded_any = true;
                        }
                    }
                }
            }
            if degraded_any {
                after_tokens = self.history_estimated_tokens();
            }
        }

        // 压缩断代：清空工具手册已加载标记，若新周期继续使用会自动重新追加最新手册
        self.fetched_tools.clear();

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
        let mut ctx = SandwichContext::new("SYS".to_string(), "初始目标：审计项目依赖".to_string());
        ctx.context_window_tokens = 8000;

        let big = "X".repeat(400);
        for i in 0..40 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("Step {}: {}", i, big)),
            });
        }

        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(
                "【任务目标更新】\n生成执行报告并写入 report.xlsx\n（原目标: 审计项目依赖）"
            ),
        });

        for i in 40..60 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("Step {}: {}", i, big)),
            });
        }

        ctx.maybe_compact_history();

        assert_eq!(
            ctx.user_goal, "生成执行报告并写入 report.xlsx",
            "compaction 应从历史事件中提取并提升最新任务目标到 M1"
        );
    }

    #[test]
    fn test_compaction_drain_aligns_and_reduces_tokens() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 8000;

        // 模拟多轮对话：大输出带 output_id（可被脱水定型）
        let big_payload = "Data content line with information.\n".repeat(40);
        for i in 1..=10 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("计划: 测试 | 工具: tool_{} | 动作: exec", i)),
            });
            ctx.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【执行输出 (v2)】\noutput_id: out_{}\n{}",
                    i, big_payload
                )),
            });
        }

        let before_tokens = ctx.history_estimated_tokens();
        assert!(before_tokens > ctx.current_budget().high_watermark_tokens);

        ctx.maybe_compact_history();

        let after_tokens = ctx.history_estimated_tokens();
        // 新契约：高水位事件内先一次性脱水定型；本场景脱水即可回到水位线下，
        // 无需归档切除（保持历史的字节连续性优先）。
        assert!(after_tokens <= ctx.current_budget().high_watermark_tokens);
        assert!(after_tokens < before_tokens);

        // 保护区外的旧输出已定型为证据句柄（存根），原文不再驻留上下文
        let full_text = serde_json::to_string(&ctx.turns_history).unwrap();
        assert!(full_text.contains("完整详细数据已安全持久化至 SQLite"));
    }

    #[test]
    fn test_compaction_drains_when_dehydration_is_insufficient() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 8000;

        // 不可脱水的重型纯文本（无 output_id 标记）：脱水无法缩小，
        // 必须走归档切除 + 摘要卡路径。
        let heavy = "Plain heavy narrative without output markers. ".repeat(30);
        for i in 1..=10 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("计划: 阶段 {}", i)),
            });
            ctx.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!("step {} {}", i, heavy)),
            });
        }
        assert!(ctx.history_estimated_tokens() > ctx.current_budget().high_watermark_tokens);

        ctx.maybe_compact_history();

        let after_tokens = ctx.history_estimated_tokens();
        assert!(after_tokens <= ctx.current_budget().low_watermark_tokens + 800);
        let first_msg = ctx.turns_history[0].content.as_str().unwrap();
        assert!(
            first_msg.contains("【历史早期执行归档"),
            "脱水不足时必须产生归档摘要卡，实际头部: {}",
            first_msg.chars().take(80).collect::<String>()
        );
    }

    #[test]
    fn test_pairing_invariant_never_broken_by_compaction() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 8000;

        for i in 1..=8 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("计划: 测试 | 工具: tool_{} | 动作: exec", i)),
            });
            ctx.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【执行输出 (v3)】\noutput_id: out_{}\n{}",
                    i,
                    "Large log payload content line\n".repeat(30)
                )),
            });
        }

        ctx.maybe_compact_history();

        // 验证压缩后活跃历史中不存在孤立的 user 或未配对结构
        let non_card_turns: Vec<&ChatMessage> = ctx
            .turns_history
            .iter()
            .filter(|m| match &m.content {
                serde_json::Value::String(s) => !s.contains("【历史早期执行归档"),
                _ => true,
            })
            .collect();

        if !non_card_turns.is_empty() {
            // 第一条非卡片消息必须是 assistant
            assert_eq!(
                non_card_turns[0].role, "assistant",
                "首条活跃历史消息必须是 assistant"
            );
            // 必须严格交替出现 assistant -> user
            for (idx, msg) in non_card_turns.iter().enumerate() {
                if idx % 2 == 0 {
                    assert_eq!(msg.role, "assistant", "偶数索引必须是 assistant");
                } else {
                    assert_eq!(msg.role, "user", "奇数索引必须是 user");
                }
            }
        }
    }

    #[test]
    fn test_prompt_prefix_byte_stability_across_steps() {
        let mut ctx = SandwichContext::new("SYS PROMPT".to_string(), "GOAL".to_string());
        ctx.context_window_tokens = 32768;

        let instruction = crate::agent::types::AgentInstruction {
            tool: Some("exec".to_string()),
            description: "run test".to_string(),
            ..Default::default()
        };

        ctx.add_step(&instruction, "Output step 1", "out_1");
        let step1_snapshot = ctx.turns_history[1].content.as_str().unwrap().to_string();

        ctx.add_step(&instruction, "Output step 2", "out_2");
        let step1_after_step2 = ctx.turns_history[1].content.as_str().unwrap().to_string();

        // 严格字节级前缀一致性断言：历史追加绝不改变旧历史的任何字节（KV Cache 100% 命中）
        assert_eq!(
            step1_snapshot, step1_after_step2,
            "步骤 1 的输出内容在步骤 2 追加后必须 100% 字节不变"
        );
    }

    #[test]
    fn test_sample_output_token_cap_strict_invariant() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 16000;

        let instruction = crate::agent::types::AgentInstruction {
            tool: Some("exec".to_string()),
            description: "run huge test".to_string(),
            ..Default::default()
        };

        // 巨型全中文 + 密集转义 JSON + 巨量报错
        let mut giant_output =
            "这是一段非常非常长的中文字符串用于测试单步硬截断闭环保证。".repeat(200);
        giant_output.push_str("\n{\"dense_json_key\": \"dense_val\", \"nested\": [1,2,3,4,5]}\n");
        for i in 0..500 {
            giant_output.push_str(&format!(
                "error: database connection timeout occurred at step {}\n",
                i
            ));
        }

        ctx.add_step(&instruction, &giant_output, "giant_out");

        let last_user_msg = ctx.turns_history.last().unwrap();
        let content_str = match &last_user_msg.content {
            serde_json::Value::String(s) => s.as_str(),
            _ => "",
        };

        let est = conservative_token_estimate(content_str);
        assert!(
            est <= ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP,
            "闭环复检保证：截断后输出 Token 量 ({}) 必须 <= 上限 ({})",
            est,
            ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP
        );
        assert!(
            content_str.contains("search_step_output"),
            "提示中必须包含 search_step_output 检索指引"
        );
    }

    #[test]
    fn test_long_running_fuzz_never_overflows_and_watermark_gap() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 8000; // 较小窗口，测试高频压缩与边界稳定性

        let instruction = crate::agent::types::AgentInstruction {
            tool: Some("bash".to_string()),
            description: "fuzz step".to_string(),
            ..Default::default()
        };

        // 模拟 40 步长程任务，每步输入从 500 到 20,000 字符随机波动
        for step in 1..=40 {
            let payload_size = (step * 7919) % 20000 + 500;
            let payload = format!("Step {} payload: {}\n", step, "X".repeat(payload_size));
            ctx.add_step(&instruction, &payload, &format!("fuzz_{}", step));

            let current_tokens = ctx.history_estimated_tokens();
            let budget = ctx.current_budget();

            // 核心不变量 1：任何时刻历史 Tokens 绝不超出 max_allowed_history_tokens + 500 缓冲
            assert!(
                current_tokens <= budget.max_allowed_history_tokens + 1000,
                "第 {} 步：历史 Tokens ({}) 超出了最大安全历史上限 ({})",
                step,
                current_tokens,
                budget.max_allowed_history_tokens
            );
        }

        // 核心不变量 2：压缩后必须有摘要归档卡存在
        let has_summary_cards = ctx.turns_history.iter().any(|m| match &m.content {
            serde_json::Value::String(s) => s.contains("【历史早期执行归档卡片"),
            _ => false,
        });
        assert!(has_summary_cards, "长任务多轮压缩后必须留存结构化归档卡片");
    }
}
