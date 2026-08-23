use crate::{
    agent::{
        context::{ChatMessage, SandwichContext},
        types::{AgentInstruction, TokenUsage},
        utils::{
            extract_json_from_text, extract_streaming_json_field, normalize_raw_instruction_value,
            repair_json,
        },
    },
    db::DbState,
    llm::{self, ModelConfig, StreamEvent},
};
use serde_json::json;
use tauri::State;
use tokio::sync::watch;

// 将格式约束放在每次模型调用的最后一条消息，避免长系统契约被模型的临时计划文本淹没。
// 它不改变工具参数、会话工作区或任务目标，仅要求最终外层输出可被宿主解析。
const FINAL_JSON_OUTPUT_ANCHOR: &str = "【本轮最终输出】现在直接输出一个合法 JSON 对象。首字符必须是 `{`；禁止先输出‘计划：…｜工具：…｜动作：…’、解释文字、Markdown 或代码围栏。";

fn append_final_json_output_anchor(api_values: &mut Vec<serde_json::Value>) {
    api_values.push(json!({
        "role": "user",
        "content": FINAL_JSON_OUTPUT_ANCHOR,
    }));
}

/// 自由文本没有可局部修复的 JSON 候选；应立刻返回外层，让路由器按失败信号选择恢复模型。
/// 这只改变模型恢复顺序，不推断工具、cwd、path 或项目参数。
fn requires_outer_protocol_recovery(final_json_str: &str) -> bool {
    !final_json_str.trim_start().starts_with('{')
}

fn replace_with_isolated_json_repair_context(
    api_values: &mut Vec<serde_json::Value>,
    raw_output: &str,
    parse_error: &str,
) {
    *api_values = vec![
        json!({
            "role": "system",
            "content": "You repair only the outer AgentInstruction JSON protocol. Preserve the intended action and arguments; do not introduce a new task, tool, path, cwd, or project parameter.",
        }),
        json!({
            "role": "user",
            "content": format!(
                "The candidate below did not satisfy the AgentInstruction JSON contract. Return only one corrected JSON object.\n\nParse error: {}\n\nCandidate:\n{}",
                parse_error, raw_output
            ),
        }),
    ];
    append_final_json_output_anchor(api_values);
}

fn append_json_regeneration_feedback(
    api_values: &mut Vec<serde_json::Value>,
    parse_error: &str,
) {
    api_values.push(json!({
        "role": "user",
        "content": format!(
            "The previous response was not a valid AgentInstruction JSON object ({}). Preserve the intended next action and return only one valid JSON object now.",
            parse_error
        ),
    }));
    append_final_json_output_anchor(api_values);
}

/// 打印发送给模型的消息概况（调试用）
pub fn print_debug_messages(api_messages: &[ChatMessage]) {
    println!("═══════════════════════════════════════════");
    println!(
        "📤 [DEBUG] 发送给模型的消息列表 (共 {} 条):",
        api_messages.len()
    );
    for (i, msg) in api_messages.iter().enumerate() {
        let content_str = match &msg.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let char_count = content_str.len();
        let preview: String = content_str.chars().take(120).collect();
        println!(
            "  [{}] role={}, chars={}, preview: {}...",
            i, msg.role, char_count, preview
        );
    }
    println!("═══════════════════════════════════════════");
}

/// 将上下文与 LLM 交互记录写入文件（包含完整 Prompt、思考过程、原始返回、Token 真实用量与报错原因，方便离线精准复盘）
pub fn log_llm_exchange_to_file(
    session_id: &str,
    step_id: usize,
    try_index: usize,
    model_config: &ModelConfig,
    api_messages: &[serde_json::Value],
    thinking_text: &str,
    raw_response: &str,
    token_usage: Option<&TokenUsage>,
    status: &str,
    error: Option<&str>,
) {
    let log_dir = std::env::current_dir()
        .map(|p| p.join("agent_logs"))
        .unwrap_or_else(|_| std::path::PathBuf::from("agent_logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.json", session_id));

    let mut logs_array = if let Ok(content) = std::fs::read_to_string(&log_path) {
        serde_json::from_str::<Vec<serde_json::Value>>(&content).unwrap_or_else(|_| vec![])
    } else {
        vec![]
    };

    let usage_val = token_usage.map(|u| {
        serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "cached_tokens": u.cached_tokens,
            "cache_hit_rate": u.cache_hit_rate,
            "context_window": u.context_window,
            "usage_percent": u.usage_percent,
            "ttft_ms": u.ttft_ms,
            "total_duration_ms": u.total_duration_ms,
            "tokens_per_second": u.tokens_per_second,
            "summary": u.summary(),
        })
    });

    let log_data = serde_json::json!({
        "step_id": step_id,
        "try_index": try_index,
        "model": &model_config.model_name,
        "model_id": &model_config.model_id,
        "platform_name": &model_config.platform_name,
        "base_url": model_config.redacted_base_url(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "usage": usage_val,
        "parse_status": status,
        "parse_error": if status == "parse_error" { error } else { None },
        "request_error": if status == "request_error" { error } else { None },
        "thinking_text": thinking_text,
        "raw_response": raw_response,
        "messages_sent_to_model": api_messages,
    });

    logs_array.push(log_data);

    if let Ok(s) = serde_json::to_string_pretty(&logs_array) {
        let _ = std::fs::write(&log_path, s);
        if let Some(err) = error {
            println!(
                "📁 [模型异常已记录] 步骤 #{} (尝试 {}) 报错已归档到 {}: {}",
                step_id,
                try_index,
                log_path.display(),
                err
            );
        } else {
            println!(
                "📁 [步骤日志已记录] 步骤 #{} 交互已归档到 {}",
                step_id,
                log_path.display()
            );
        }
    }
}

pub async fn call_llm(
    messages: &SandwichContext,
    state: &State<'_, DbState>,
    model_id: String,
    app: Option<&tauri::AppHandle>,
    step_id: usize,
    session_id: &str,
    log_context: bool,
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<(AgentInstruction, TokenUsage, String, Option<usize>), String> {
    // 1. 从数据库解析模型配置
    let config = ModelConfig::from_db(state, &model_id)?;
    config.log_platform_info();

    // 2. 组装消息（保持原始序列）+ provider 能力归一化。
    // OpenAI 兼容路由（火山/DeepSeek/中转站/Ollama 等）直接发送原序列：
    // 历史尾部字节跨轮稳定，前缀缓存不被每轮重渲染的状态卡打断。
    // 仅要求严格 user/assistant 交替的 provider（Anthropic 系）才归并。
    let api_messages =
        normalize_messages_for_provider(messages.assemble_messages(), &config);
    let history_estimate_at_request = messages.history_estimated_tokens();
    print_debug_messages(&api_messages);

    // 3. 转为 serde_json::Value 供 llm 层使用
    let mut api_values: Vec<serde_json::Value> = api_messages
        .iter()
        .map(|m| json!({"role": m.role, "content": &m.content}))
        .collect();
    append_final_json_output_anchor(&mut api_values);

    // 4. JSON 解析重试循环
    let mut current_try = 0;
    let max_retries = 3;
    // 隔离 JSON 修复的短 prompt 不能覆盖完整 Agent 上下文的校准基线。
    let mut uses_primary_prompt = true;

    loop {
        current_try += 1;

        let stream_start = std::time::Instant::now();
        let mut first_token_at: Option<std::time::Instant> = None;
        let mut total_chars: usize = 0;
        let mut last_thinking_emit_at = std::time::Instant::now();
        let last_content_emit_at = std::time::Instant::now();
        const EMIT_THROTTLE_MS: u128 = 50; // 20 FPS 平滑节流

        // 流式接收事件，实时向前端推送 Thinking/Content 以及动态速率与 TTFT
        let mut on_event = |event: StreamEvent| {
            if let Some(app_handle) = app {
                use tauri::Emitter;
                if first_token_at.is_none() {
                    first_token_at = Some(std::time::Instant::now());
                }
                let ttft_ms = first_token_at
                    .unwrap()
                    .duration_since(stream_start)
                    .as_millis() as u64;
                let chunk_len = match &event {
                    StreamEvent::Thinking { chunk, .. } => chunk.len(),
                    StreamEvent::Content { chunk, .. } => chunk.len(),
                };
                total_chars += chunk_len;
                let est_tokens = (total_chars as f64 / 3.5).ceil() as i64;
                let gen_secs = first_token_at.unwrap().elapsed().as_secs_f64();
                let live_speed = if gen_secs > 0.05 && est_tokens > 0 {
                    (est_tokens as f64) / gen_secs
                } else {
                    0.0
                };

                let now = std::time::Instant::now();
                match event {
                    StreamEvent::Thinking { accumulated, .. } => {
                        if now.duration_since(last_thinking_emit_at).as_millis() >= EMIT_THROTTLE_MS
                        {
                            last_thinking_emit_at = now;
                            app_handle
                                .emit(
                                    &format!("agent-progress-{}", session_id),
                                    json!({
                                        "type": "thinking",
                                        "step_id": step_id,
                                        "content": accumulated,
                                        "done": false,
                                        "live_tokens": est_tokens,
                                        "live_speed": live_speed,
                                        "ttft_ms": ttft_ms
                                    }),
                                )
                                .ok();
                        }
                    }
                    StreamEvent::Content {
                        chunk,
                        accumulated_content,
                        ..
                    } => {
                        if now.duration_since(last_content_emit_at).as_millis() >= EMIT_THROTTLE_MS
                        {
                            let is_intermediate_tool = accumulated_content.contains("\"tool\"")
                                && !accumulated_content.contains("\"finish\"");
                            let streaming_summary = if is_intermediate_tool {
                                None
                            } else {
                                extract_streaming_json_field(
                                    accumulated_content,
                                    &["summary", "answer", "details"],
                                )
                            };

                            app_handle
                                .emit(
                                    &format!("agent-progress-{}", session_id),
                                    json!({
                                        "type": "content_stream",
                                        "step_id": step_id,
                                        "chunk": chunk,
                                        "accumulated": accumulated_content,
                                        "streaming_summary": streaming_summary,
                                        "live_tokens": est_tokens,
                                        "live_speed": live_speed,
                                        "ttft_ms": ttft_ms
                                    }),
                                )
                                .ok();

                            if let Some(ref summary_text) = streaming_summary {
                                if !summary_text.is_empty() {
                                    app_handle
                                        .emit(
                                            &format!("agent-progress-{}", session_id),
                                            json!({
                                                "type": "summary_stream",
                                                "step_id": step_id,
                                                "content": summary_text,
                                                "live_tokens": est_tokens,
                                                "live_speed": live_speed,
                                                "ttft_ms": ttft_ms
                                            }),
                                        )
                                        .ok();
                                }
                            }
                        }
                    }
                }
            }
        };

        // 5. 调用 llm 层发送请求（含 HTTP 重试）
        let result = match llm::stream_with_retry(
            &config,
            &api_values,
            reasoning_effort,
            interrupt_rx,
            3,
            &mut on_event,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                if log_context {
                    log_llm_exchange_to_file(
                        session_id,
                        step_id,
                        current_try,
                        &config,
                        &api_values,
                        "",
                        "",
                        None,
                        "request_error",
                        Some(&error),
                    );
                }
                return Err(error);
            }
        };

        // 6. Token 用量与速度指标统计
        let token_usage = match result.usage {
            Some(raw) => TokenUsage::new(
                raw.prompt_tokens,
                raw.completion_tokens,
                raw.cached_tokens,
                config.context_window,
                result.ttft_ms,
                result.total_duration_ms,
                result.tokens_per_second,
            ),
            None => {
                let (est_p, est_c) = llm::estimate_usage(
                    &api_values,
                    result.full_response.len(),
                    config.context_window,
                );
                let usage = TokenUsage::new(
                    est_p,
                    est_c,
                    0,
                    config.context_window,
                    result.ttft_ms,
                    result.total_duration_ms,
                    result.tokens_per_second,
                );
                println!("⚠️ API 未返回 usage，使用估算值: {}", usage.summary());
                usage
            }
        };

        println!("{}", token_usage.summary());
        if !result.thinking_text.is_empty() {
            println!(
                "🧠 Agent 思考过程 ({} 字符): {}...",
                result.thinking_text.len(),
                result.thinking_text.chars().take(300).collect::<String>()
            );
        }

        // 7. 尝试从响应中提取 JSON
        let mut final_json_str_opt = extract_json_from_text(&result.full_response);
        if final_json_str_opt.is_none() && result.full_response.trim().is_empty() {
            final_json_str_opt = extract_json_from_text(&result.thinking_text);
        }
        let final_json_str = final_json_str_opt.unwrap_or_else(|| {
            if result.full_response.trim().is_empty() && !result.thinking_text.trim().is_empty() {
                result.thinking_text.clone()
            } else {
                result.full_response.clone()
            }
        });

        // 8. 解析 AgentInstruction (带智能容错与修复)
        let parse_result = (|| -> Result<AgentInstruction, String> {
            let json_to_parse =
                repair_json(&final_json_str).unwrap_or_else(|| final_json_str.clone());
            let mut val: serde_json::Value = serde_json::from_str(&json_to_parse)
                .map_err(|e| format!("基础 JSON 语法错误: {} (处理前: {})", e, final_json_str))?;
            normalize_raw_instruction_value(&mut val);
            let inst: AgentInstruction =
                serde_json::from_value(val).map_err(|e| format!("数据结构转换失败: {}", e))?;
            Ok(inst)
        })();

        match parse_result {
            Ok(inst) => {
                if log_context {
                    log_llm_exchange_to_file(
                        session_id,
                        step_id,
                        current_try,
                        &config,
                        &api_values,
                        &result.thinking_text,
                        &final_json_str,
                        Some(&token_usage),
                        "success",
                        None,
                    );
                }
                let calibration_history_estimate =
                    uses_primary_prompt.then_some(history_estimate_at_request);
                return Ok((
                    inst,
                    token_usage,
                    result.thinking_text,
                    calibration_history_estimate,
                ));
            }
            Err(e) => {
                log_llm_exchange_to_file(
                    session_id,
                    step_id,
                    current_try,
                    &config,
                    &api_values,
                    &result.thinking_text,
                    &final_json_str,
                    Some(&token_usage),
                    "parse_error",
                    Some(&e),
                );

                if let Some(app_handle) = app {
                    use tauri::Emitter;
                    let _ = app_handle.emit(
                        &format!("agent-progress-{}", session_id),
                        json!({
                            "type": "llm_parse_error",
                            "step_id": step_id,
                            "try_index": current_try,
                            "error": &e,
                            "raw_response": &final_json_str,
                            "thinking_text": &result.thinking_text,
                        }),
                    );
                    let _ = app_handle.emit(
                        &format!("agent-log-{}", session_id),
                        format!(
                            "⚠️ [模型格式异常·第{}次尝试] 报错: {}\n📄 模型原始输出:\n{}",
                            current_try, e, final_json_str
                        ),
                    );
                }

                if requires_outer_protocol_recovery(&final_json_str) {
                    return Err(format!(
                        "❌ [协议恢复] 模型未返回可局部修复的 JSON 对象；交由外层路由选择恢复模型。{}",
                        e
                    ));
                }
                if current_try >= max_retries {
                    return Err(format!("{} \n原文: {}", e, final_json_str));
                }
                println!(
                    "⚠️ 大模型输出了错误的 JSON 格式，开始自动拦截并重试 (第 {} 次 / 共 {} 次): {}",
                    current_try, max_retries, e
                );

                /* Legacy verbose retry feedback retained below for reference but disabled.
                // 将助手的错误输出加入上下文
                api_values.push(json!({"role": "assistant", "content": result.full_response}));

                // 将报错信息丢回给模型纠正
                api_values.push(json!({
                    "role": "user",
                    "content": format!(
                        "YOUR LAST OUTPUT TRIGGERED A PARSE ERROR: \n{}\n\n\
                         RULES:\n\
                         1. 遇到问题必须且只能在 <think> 标签内完成逻辑推导。\n\
                         2. <think> 结束后的最终输出，必须且只能是一个有效的 JSON 对象来调用对应的工具函数。\n\
                         3. 如果在 JSON 字符串字段（如 summary/details）中包含 HTML 或双引号，必须严格转义为 `\\\"`，绝对不能输出未闭合或损坏的 JSON。\n\
                         4. 绝对禁止在 JSON 外输出任何解释性文本。\n\n\
                         Fix the syntax error and generate valid JSON again.",
                        e
                    )
                }));
                append_final_json_output_anchor(&mut api_values);
                */
                // JSON 候选的修复请求只携带候选、错误和通用协议，避免重发整段任务历史。
                // 自由文本响应已在上方交由外层路由恢复，不在同一模型内机械重试。
                replace_with_isolated_json_repair_context(
                    &mut api_values,
                    &final_json_str,
                    &e,
                );
                uses_primary_prompt = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_final_json_anchor_after_existing_history() {
        let mut messages = vec![json!({"role": "system", "content": "contract"})];

        append_final_json_output_anchor(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], FINAL_JSON_OUTPUT_ANCHOR);
        assert!(FINAL_JSON_OUTPUT_ANCHOR.contains("首字符必须是 `{`"));
    }

    #[test]
    fn isolated_json_repair_context_drops_task_history() {
        let mut messages = vec![
            json!({"role": "system", "content": "large task contract"}),
            json!({"role": "user", "content": "large user goal"}),
        ];

        replace_with_isolated_json_repair_context(
            &mut messages,
            "{\"tool\": \"read_text_file\"",
            "unexpected end of JSON input",
        );

        assert_eq!(messages.len(), 3);
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("outer AgentInstruction JSON protocol"));
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("unexpected end of JSON input"));
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("read_text_file"));
        assert!(!messages
            .iter()
            .any(|message| message["content"].as_str() == Some("large task contract")));
        assert_eq!(messages[2]["content"], FINAL_JSON_OUTPUT_ANCHOR);
    }

    #[test]
    fn regeneration_feedback_keeps_context_without_legacy_format_rules() {
        let mut messages = vec![json!({"role": "system", "content": "task contract"})];

        append_json_regeneration_feedback(&mut messages, "expected value at line 1 column 1");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"], "task contract");
        let feedback = messages[1]["content"].as_str().unwrap();
        assert!(feedback.contains("valid AgentInstruction JSON object"));
        assert!(!feedback.contains("<think>"));
        assert!(!feedback.contains("HTML"));
        assert_eq!(messages[2]["content"], FINAL_JSON_OUTPUT_ANCHOR);
    }
    #[test]
    fn pure_text_protocol_failures_escalate_to_outer_recovery() {
        assert!(requires_outer_protocol_recovery("计划：先建立项目地图"));
        assert!(requires_outer_protocol_recovery("I will inspect the repository first."));
        assert!(!requires_outer_protocol_recovery("{\"tool\":\"read_text_file\"}"));
    }
}

// ---------------------------------------------------------------------------
// Provider 消息能力归一化
//
// 通用组装器（assembler）输出原始消息序列，历史与动态尾部各自独立成条；
// 是否把连续同角色消息归并为一条，由这里的 provider 能力决定——
// 这是发送适配层的职责，不允许进入通用上下文组装路径。
// ---------------------------------------------------------------------------

/// 判断 provider 是否要求严格 user/assistant 交替（需要在发送前归并连续
/// 同角色消息）。优先读 platforms.requires_alternating_roles 显式配置
///（1=要求归并、0=接受原序列）；未设置（-1，含旧库）时按 base_url 推断：
/// Anthropic Messages API 以交替 turn 训练（现行服务端会把连续 turn 自动
/// 合并为单 turn，历史上曾直接 422，保守起见仍客户端归并）。OpenAI 兼容
/// 路由接受连续同角色消息，保持原序列以稳定前缀缓存。
fn provider_requires_alternating_roles(config: &crate::llm::ModelConfig) -> bool {
    match config.requires_alternating_roles {
        1 => true,
        0 => false,
        _ => config.base_url.to_ascii_lowercase().contains("anthropic"),
    }
}

fn normalize_messages_for_provider(
    messages: Vec<crate::agent::context::types::ChatMessage>,
    config: &crate::llm::ModelConfig,
) -> Vec<crate::agent::context::types::ChatMessage> {
    if provider_requires_alternating_roles(config) {
        crate::agent::context::assembler::consolidate_consecutive_messages(messages)
    } else {
        messages
    }
}

#[cfg(test)]
mod provider_normalization_tests {
    use super::*;
    use crate::agent::context::types::ChatMessage;
    use serde_json::json;

    fn config_with_base_url(url: &str) -> crate::llm::ModelConfig {
        crate::llm::ModelConfig {
            model_id: "m".to_string(),
            platform_name: "p".to_string(),
            base_url: url.to_string(),
            api_key: "k".to_string(),
            model_name: "n".to_string(),
            max_tokens: 1024,
            temperature: 0.7,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 200_000,
            provider_capabilities: "{}".to_string(),
            timeout_secs: 60,
            requires_alternating_roles: -1,
        }
    }

    fn sample_messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage { role: "user".to_string(), content: json!("历史输出 C") },
            ChatMessage { role: "user".to_string(), content: json!("【状态卡】") },
            ChatMessage { role: "user".to_string(), content: json!("【决策指令】") },
        ]
    }

    #[test]
    fn openai_compatible_routes_keep_original_sequence() {
        for url in [
            "https://ark.cn-beijing.volces.com/api/coding/v3",
            "https://api.deepseek.com",
            "https://zhandou.online/v1",
        ] {
            let normalized = normalize_messages_for_provider(sample_messages(), &config_with_base_url(url));
            assert_eq!(normalized.len(), 3, "OpenAI 兼容路由不得归并：{}", url);
            assert_eq!(normalized[0].content, json!("历史输出 C"), "历史消息必须保持独立");
        }
    }

    #[test]
    fn explicit_platform_capability_overrides_url_guess() {
        // 显式 0：即使 URL 含 anthropic 也保持原序列（兼容层/代理场景）
        let mut cfg = config_with_base_url("https://proxy.example.com/anthropic/v1");
        cfg.requires_alternating_roles = 0;
        let normalized = normalize_messages_for_provider(sample_messages(), &cfg);
        assert_eq!(normalized.len(), 3, "显式 0 必须压过 URL 猜测");

        // 显式 1：非 anthropic URL 也归并
        let mut cfg2 = config_with_base_url("https://ark.cn-beijing.volces.com/api/coding/v3");
        cfg2.requires_alternating_roles = 1;
        let normalized2 = normalize_messages_for_provider(sample_messages(), &cfg2);
        assert_eq!(normalized2.len(), 1, "显式 1 必须归并");

        // -1（未设置/旧库）：回落 URL 推断
        let mut cfg3 = config_with_base_url("https://api.anthropic.com");
        cfg3.requires_alternating_roles = -1;
        assert!(provider_requires_alternating_roles(&cfg3));
    }

    #[test]
    fn anthropic_routes_consolidate_consecutive_same_role() {
        let normalized = normalize_messages_for_provider(
            sample_messages(),
            &config_with_base_url("https://api.anthropic.com/v1"),
        );
        assert_eq!(normalized.len(), 1, "Anthropic 系必须归并为单条");
        let text = normalized[0].content.as_str().unwrap();
        assert!(text.contains("历史输出 C") && text.contains("【状态卡】") && text.contains("【决策指令】"));
    }
}
