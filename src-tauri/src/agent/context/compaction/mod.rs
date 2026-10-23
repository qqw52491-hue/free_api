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
                // 规则降级也保留有界原文首尾；不能复制完整回执使归档反而膨胀。
                let preview: String = if content_str.starts_with("【历史工具回执（节选）】") {
                    let excerpt = content_str.split_once("原文节选:\n")
                        .map(|(_, body)| body.split("\n(完整详细数据").next().unwrap_or(body))
                        .unwrap_or(first_line);
                    let head: String = excerpt.chars().take(60).collect();
                    let tail: String = excerpt.chars().rev().take(80).collect::<String>().chars().rev().collect();
                    if excerpt.chars().count() > 140 {
                        format!("[原文节选，结果可能不完整] {} … {}", head, tail)
                    } else {
                        format!("[原文节选] {}", excerpt)
                    }
                } else {
                    first_line.chars().take(140).collect()
                };
                lines.push(format!("- [{}]{} {}", msg.role, out_ref, preview));
            }
        }
        Ok(lines.join("\n"))
    }
}

/// 异步调用 LLM 生成语义级继续工作摘要（带 20s 硬超时与多格式兼容提取）
/// 摘要请求复用会话真实前缀（system+M1+历史逐字节命中 provider 前缀缓存），
/// 摘要指令追加在尾部——整段历史近零成本送达摘要模型。
/// 前缀构造必须复刻 assemble_messages 的逐字节规则（含 M1 与历史首条去重），
/// 差一个字节即从第 0 token 起全部 miss。
pub async fn generate_llm_summary(
    config: &ModelConfig,
    system_prompt: &str,
    user_goal: &str,
    history_messages: &[ChatMessage],
) -> Result<String, String> {
    if history_messages.is_empty() {
        return Err("无历史可压缩".to_string());
    }
    let client = config.build_client()?;
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let mut msgs: Vec<ChatMessage> = Vec::new();
    // M0 system + M1 任务目标锚点：与 assemble_messages 完全同构
    msgs.push(ChatMessage {
        role: "system".to_string(),
        content: json!(system_prompt),
    });
    if !user_goal.is_empty() {
        msgs.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【任务目标】\n{}", user_goal)),
        });
    }
    // 复刻 M1/历史首条去重：首条历史 == user_goal 时跳过（同 assemble_messages）
    let history_slice = if let Some(first) = history_messages.first() {
        if first.role == "user" && first.content.as_str() == Some(user_goal) {
            &history_messages[1..]
        } else {
            history_messages
        }
    } else {
        history_messages
    };
    msgs.extend(history_slice.iter().cloned());
    msgs.push(ChatMessage {
        role: "user".to_string(),
        content: json!("【上下文压缩指令】以上是本 Agent 执行长程任务的完整历史（含步骤计划、工具调用、执行输出与报错）。\n\
        请提取并生成一份高度凝练的【阶段性任务继续摘要】，严格包含以下 4 个结构化板块：\n\n\
        ## 1. 已完成事项与关键产物\n\
        - 简明列出已成功完成的阶段、生成的文件路径、创建的数据表等\n\n\
        ## 2. 关键事实与架构决策\n\
        - 列出执行中确认的关键环境信息、配置项、发现的约束条件\n\n\
        ## 3. 踩坑记录与教训\n\
        - 列出失败过的操作、错误的参数/action、服务端报错及后续如何纠正的（避免后续步骤重蹈覆辙）\n\n\
        ## 4. 未完成事项与下一步建议\n\
        - 基于前面的进展，明确指出当前尚未完成的工作点与建议方向\n\n\
        ⚠️ 注意：禁止流水账重复，只保留具有决策指导意义的硬事实与结论；如有 output_id 引用、文件路径或行号请准确保留。"),
    });

    // provider 归一化与真实请求同口径（严格交替路由合并连续同角色），
    // 保证线上前缀 = 真实请求线上前缀；不合并不止可能被 422，还会破坏命中。
    if crate::agent::utils::llm_client::provider_requires_alternating_roles(config) {
        msgs = crate::agent::context::assembler::consolidate_consecutive_messages(msgs);
    }
    let messages: Vec<serde_json::Value> = msgs
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or_else(|_| json!({"role": m.role, "content": ""})))
        .collect();

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

    // send + body 读取整体限时：仅包 send() 时，端点收下连接不吐 body 会让
    // resp.json() 无限挂起（实测整轮评测卡死在压缩路径）
    let json_resp: serde_json::Value = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        async {
            let resp = client
                .post(&url)
                .headers(headers)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("LLM 摘要网络请求失败: {}", e))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| format!("LLM 摘要响应体读取失败: {}", e))?;
            if !status.is_success() {
                return Err(format!("LLM 摘要 API 返回错误状态码 {}: {}", status, text));
            }
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| format!("解析 LLM 摘要返回 JSON 失败: {}", e))
        },
    )
    .await
    .map_err(|_| "LLM 摘要请求超时 (超过 90s，含响应体读取)".to_string())??;

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
    /// 摘要请求前缀复用所需的静态锚点（缺省时退化为无 system/M1 的普通请求）
    pub system_prompt: Option<&'a str>,
    pub user_goal: Option<&'a str>,
}

impl<'a> Summarizer for HybridSummarizer<'a> {
    fn summarize(&self, messages: &[ChatMessage]) -> Result<String, String> {
        let rule_index = RuleBasedSummarizer.summarize(messages)?;

        let mut llm_summary_opt: Option<String> = None;
        if let Some(cfg) = self.model_config {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let cfg_clone = cfg.clone();
                let sys = self.system_prompt.unwrap_or_default().to_string();
                let goal = self.user_goal.unwrap_or_default().to_string();
                let llm_res = tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        generate_llm_summary(&cfg_clone, &sys, &goal, messages).await
                    })
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
        crate::agent::attachment_vision::project_history(self)
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
        ContextBudget::derive_with_config(
            self.context_window_tokens,
            ContextBudget::DEFAULT_MAX_OUTPUT_TOKENS,
            static_prefix_tokens,
            &self.budget_config,
        )
    }

    /// 低水位批量脱水：大窗口模型下 85% 高水位（deep_coding ≈255k）日常任务
    /// 不可达，旧输出会被逐步全量携带。历史估算超过脱水水位时仅执行一次性
    /// 批量脱水：固定尾窗保护区、不清读取台账（output_id 句柄仍可回查）、
    /// 不动手册/纪元标记——前缀缓存只在事件点断裂一次，摊薄到后续全部步数。
    /// 扫无可脱对象时滞后重扫（dehydration_rearm_tokens），避免每步空转。
    fn maybe_dehydrate_low_watermark(&mut self, budget: &ContextBudget, current_tokens: usize) {
        // 与上方高水位同源的真实校准：本地保守估算会高估 CJK/JSON/代码，
        // 用最近一次服务端回传的完整 prompt 反推历史真实占用。
        let history_volume = match (
            self.last_reported_prompt_tokens.filter(|tokens| *tokens > 0),
            self.last_reported_history_estimate_tokens,
        ) {
            (Some(real_total), Some(history_at_request)) => real_total
                .saturating_sub(budget.static_prefix_tokens)
                .saturating_sub(budget.tail_state_tokens)
                .saturating_add(current_tokens.saturating_sub(history_at_request)),
            _ => current_tokens,
        };
        let watermark = ContextBudget::dehydration_watermark_tokens();
        if history_volume <= watermark || history_volume < self.dehydration_rearm_tokens {
            return;
        }
        // 固定尾窗保护区：比例的 recent_protected（deep_coding ≈75k）在 45k
        // 历史下会吞掉全部消息导致空转；低水位路径用固定小额覆盖即时消费窗口。
        let mut protected_from_back = 0usize;
        let mut back_tokens = 0usize;
        let projected_history =
            crate::agent::attachment_vision::project_messages(self, &self.turns_history);
        for msg in projected_history.iter().rev() {
            back_tokens += match &msg.content {
                serde_json::Value::String(s) => conservative_token_estimate(s),
                other => conservative_token_estimate(&other.to_string()),
            };
            protected_from_back += 1;
            if back_tokens >= ContextBudget::DEHYDRATION_PROTECTED_TOKENS
                && protected_from_back >= 2
            {
                break;
            }
        }
        let dehydrate_limit = self
            .turns_history
            .len()
            .saturating_sub(protected_from_back);
        let mut dehydrated_count = 0usize;
        for msg in self.turns_history.iter_mut().take(dehydrate_limit) {
            let before = match &msg.content {
                serde_json::Value::String(s) => s.len(),
                other => other.to_string().len(),
            };
            *msg = crate::agent::context::assembler::dehydrate_historical_turn(msg);
            let after_len = match &msg.content {
                serde_json::Value::String(s) => s.len(),
                other => other.to_string().len(),
            };
            if after_len != before {
                dehydrated_count += 1;
            }
        }
        let after = self.history_estimated_tokens();
        if dehydrated_count > 0 && after < current_tokens.saturating_sub(2_000) {
            println!(
                "🧊 [Dehydration·低水位] 历史 ~{} tokens 触线（水位 {}）：保护区外 {} 条老输出就地脱水 → ~{} tokens（缓存仅本步断裂一次，台账保留可句柄回查）",
                current_tokens, watermark, dehydrated_count, after
            );
            self.dehydration_rearm_tokens = watermark;
        } else {
            // 扫无可脱：滞后重扫，避免每步空转
            self.dehydration_rearm_tokens = history_volume
                .saturating_add(ContextBudget::DEHYDRATION_REARM_DELTA_TOKENS);
        }
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
        // 新附件先完成一次观察/失败落账，避免 base64 体积触发归档后无图可看。
        if crate::agent::attachment_vision::has_unobserved_images(self) {
            return;
        }
        let budget = self.current_budget();
        let mut current_tokens = self.history_estimated_tokens();

        // 真实校准（缓存友好）：本地保守估算会对中文、JSON 与代码高估，
        // 因而不应在低真实占用时提前脱水、打断稳定前缀缓存。服务端返回的
        // 完整 prompt 已含静态前缀、尾部和请求时历史；只叠加该请求之后
        // 新追加历史的正向保守增量。缺少任一同源快照时回退到本地安全估算。
        let calibrated_prompt_tokens = match (
            self.last_reported_prompt_tokens
                .filter(|tokens| *tokens > 0),
            self.last_reported_history_estimate_tokens,
        ) {
            (Some(real_total), Some(history_at_request)) => {
                Some(real_total.saturating_add(current_tokens.saturating_sub(history_at_request)))
            }
            _ => None,
        };
        let prompt_high_watermark = budget
            .static_prefix_tokens
            .saturating_add(budget.tail_state_tokens)
            .saturating_add(budget.high_watermark_tokens);
        let reached_high_watermark = calibrated_prompt_tokens
            .map(|tokens| tokens > prompt_high_watermark)
            .unwrap_or(current_tokens > budget.high_watermark_tokens);

        // 未达高水位，绝不轻易压缩（保护前缀缓存命中率）；
        // 但大窗口下高水位日常不可达——先检查低水位批量脱水（一次性事件，非滚动改写）。
        if !reached_high_watermark {
            self.maybe_dehydrate_low_watermark(&budget, current_tokens);
            return;
        }
        // 压缩即将改写历史：模型对旧读取内容的记忆随之失效，读取台账清零使重读重新合法
        self.read_ledger.clear();

        // ── 0. 先做一次性脱水定型：把保护区之外的历史消息就地替换为证据句柄 ──
        // 这是旧输出"变成存根"的唯一时机：只在 compaction 事件内发生，
        // 平时组装严格 append-only，前缀缓存不会因脱水边界滑动而每轮断裂。
        let recent_protected_tokens = (budget.max_allowed_history_tokens as f64
            * ContextBudget::RECENT_PROTECTED_RATIO) as usize;
        let mut protected_from_back = 0;
        let mut back_tokens = 0;
        let projected_history =
            crate::agent::attachment_vision::project_messages(self, &self.turns_history);
        for msg in projected_history.iter().rev() {
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
        let dehydrate_limit = self.turns_history.len().saturating_sub(protected_from_back);
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
        let projected_history =
            crate::agent::attachment_vision::project_messages(self, &self.turns_history);
        for (i, msg) in projected_history.iter().enumerate() {
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

        // 压缩只能归档历史，不能把历史目标重新写回当前任务。
        // 保留压缩前的锚点供摘要请求使用；仅在归档边界刷新后续 M1。
        let goal_before_promotion = self.user_goal.clone();
        self.user_goal = self.effective_goal().to_string();

        // ── 4. 使用双层混合总结器进行安全归档 ──
        let summarizer = HybridSummarizer {
            model_config: self.compaction_model_config.as_ref(),
            system_prompt: Some(&self.system_prompt),
            user_goal: Some(&goal_before_promotion),
        };
        let archive_text = summarizer
            .summarize(&crate::agent::attachment_vision::project_messages(
                self,
                &to_compact,
            ))
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

            // 摘要保留为历史材料；列表格式不证明其中条目是已确认事实。
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
                    // 摘要中可以引用多个 output_id，不能把整张归档卡当一份工具输出丢弃。
                    if text.starts_with("【历史早期执行归档") {
                        continue;
                    }
                    if let Some(pos) = text.find("output_id: ") {
                        let id = text[pos + 11..].lines().next().unwrap_or("").trim();
                        if !id.is_empty() && text.len() > 300 {
                            msg.content = json!(format!(
                                "【执行输出精简句柄】\noutput_id: {}\n[结果未保留，不能据此判断成功或失败；完整详细数据已安全持久化至 SQLite，调用 search_step_output / get_step_output(output_id={}) 获取]",
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
        // 规则卡事件同理：历史排空后若门禁仍激活，ensure_rule_events 会重新注入
        self.verification_rules_emitted = false;
        self.contract_rules_emitted = false;
        // 🌟 纪元换代：将 epoch 基线刷新为当前最新的 system_prompt，
        // 后续轮次以全新 M0 为锚点，先前在历史中追加的"规约更新事件"已被归档压缩收敛。
        self.epoch_system_prompt = Some(self.system_prompt.clone());

        println!(
            "✅ [Compaction Engine] 压缩完成：历史 Tokens 从 {} 降至 {} (低水位目标: {})",
            current_tokens, after_tokens, budget.low_watermark_tokens
        );
    }

}

#[cfg(test)]
mod tests;
