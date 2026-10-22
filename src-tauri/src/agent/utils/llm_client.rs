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
const FINAL_JSON_OUTPUT_ANCHOR: &str = "【本轮最终输出】现在直接输出一个合法 JSON 对象。首字符必须是 `{`；JSON 外不得添加说明文字，也不得用代码围栏包裹整个对象。summary、details 等面向用户的字符串正文可以使用 Markdown（标题、加粗、列表、代码块等）；字符串中的换行、双引号与反斜杠须按 JSON 规则转义。";

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

pub(crate) fn instruction_recovery_feedback(error: &str) -> String {
    if error.starts_with("[output_incomplete]") {
        format!("上一轮没有获得完整可执行的正式回复，本次响应未执行（{}）。保留此前已完成的工作，只提交下一项必要操作的完整 JSON；长脚本可以拆成较小的写入操作，不必重新规划整个任务。", error)
    } else {
        format!("上一轮指令未通过协议校验，本次响应未执行（{}）。请按系统契约返回一个完整、可执行的 JSON 指令。", error)
    }
}

fn parse_instruction_response(content: &str, thinking: &str, finish_reason: Option<&str>) -> Result<AgentInstruction, String> {
    if matches!(finish_reason, Some("length" | "max_tokens")) {
        return Err(format!("[output_incomplete] 供应商报告输出达到上限（finish_reason={}）；未执行截断响应。", finish_reason.unwrap()));
    }
    if content.trim().is_empty() {
        return Err(format!("[output_incomplete] 正式回复为空；收到 {} 字符思考文本，思考文本不能作为工具指令。", thinking.chars().count()));
    }
    let candidate = extract_json_from_text(content).unwrap_or_else(|| content.to_owned());
    parse_executable_instruction(&candidate)
}

fn append_json_regeneration_feedback(api_values: &mut Vec<serde_json::Value>, parse_error: &str) {
    api_values.push(json!({
        "role": "user",
        "content": instruction_recovery_feedback(parse_error),
    }));
    append_final_json_output_anchor(api_values);
}

/// 将模型文本转换为“可执行”的 AgentInstruction。
/// JSON 语法、serde 结构与可执行形状三层都通过，才允许进入主循环。
/// 失败统一返回到既有 regeneration feedback，不能默默退化为 `core` 动作。
fn parse_executable_instruction(final_json_str: &str) -> Result<AgentInstruction, String> {
    let json_to_parse = repair_json(final_json_str).unwrap_or_else(|| final_json_str.to_string());
    let mut value: serde_json::Value = serde_json::from_str(&json_to_parse)
        .map_err(|error| format!("基础 JSON 语法错误: {}", error))?;
    normalize_raw_instruction_value(&mut value);
    normalize_completion_state_updates(&mut value);
    validate_instruction_todo_ids(&mut value)?;
    let mut instruction: AgentInstruction =
        serde_json::from_value(value).map_err(|error| format!("数据结构转换失败: {}", error))?;
    instruction
        .validate_executable_shape()
        .map_err(|error| format!("指令可执行性校验失败: {}", error))?;
    // submit_plan / prepare_finish 与工具调用混发：在进入主循环前就地丢弃夹带的工具入口（不执行），
    // 下游写前门/审计台账都只会看到纯状态动作，免整轮打回重发。
    instruction.strip_entry_when_status_action();
    Ok(instruction)
}

/// 仅为宿主交付入口接收嵌套的协议状态更新，普通工具的参数保持原义。
/// 顶层字段显式存在时优先（包括 []），不从嵌套参数补写或覆盖。
fn normalize_completion_state_updates(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let completion_entry = ["tool", "action"].iter().any(|key| {
        map.get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| {
                name.eq_ignore_ascii_case("finish") || name.eq_ignore_ascii_case("prepare_finish")
            })
    }) || map
        .get("prepare_finish")
        .is_some_and(serde_json::Value::is_object);
    if !completion_entry {
        return;
    }
    let nested = map
        .get("prepare_finish")
        .filter(|v| v.is_object())
        .or_else(|| map.get("command").filter(|v| v.is_object()))
        .or_else(|| map.get("params").filter(|v| v.is_object()))
        .cloned();
    if let Some(nested) = nested {
        for field in [
            "todo_update",
            "review_contract_update",
            "change_verification_update",
        ] {
            if !map.contains_key(field) {
                if let Some(update) = nested.get(field) {
                    map.insert(field.into(), update.clone());
                }
            }
        }
    }
}

/// 模型提交的 ID 不得悄悄变成 0；旧检查点仍用兼容反序列化，不因此无法恢复。
fn validate_instruction_todo_ids(value: &mut serde_json::Value) -> Result<(), String> {
    let Some(updates) = value.get_mut("todo_update") else {
        return Ok(());
    };
    if let Some(text) = updates.as_str() {
        *updates = serde_json::from_str(text)
            .map_err(|e| format!("todo_update 字符串不是合法 JSON 数组：{e}"))?;
    }
    if updates.is_null() {
        *updates = json!([]);
    }
    let updates = updates.as_array_mut().ok_or("todo_update 必须为数组")?;
    let mut ids = std::collections::HashSet::new();
    for (index, item) in updates.iter_mut().enumerate() {
        let object = item
            .as_object_mut()
            .ok_or_else(|| format!("todo_update[{index}] 必须为对象"))?;
        let id = object.get("id").and_then(crate::agent::types::TodoItem::parse_input_id)
            .ok_or_else(|| format!("todo_update[{index}].id 缺失或格式错误：必须为非负整数、数字字符串或 #数字，不能默认归为 0"))?;
        if !ids.insert(id) {
            return Err(format!(
                "todo_update[{index}].id={id} 重复；同一批更新每个 ID 只能出现一次"
            ));
        }
        object.insert("id".into(), json!(id));
    }
    Ok(())
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
    finish_reason: Option<&str>,
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
        "finish_reason": finish_reason,
        "response_error": error,
        "parse_error": if status == "parse_error" { error } else { None },
        "request_error": if status == "request_error" { error } else { None },
        "thinking_text": thinking_text,
        "raw_response": raw_response,
        "messages_sent_to_model": api_messages,
    });

    logs_array.push(log_data);

    if let Ok(s) = serde_json::to_string_pretty(&logs_array) {
        let _ = std::fs::write(&log_path, s);
        // 成功归档是常规簿记，不广播——每步一行的公告渲染进对话流是纯噪音
        // （长路径在 md 下显示为一坨链接，实测观感"卡卡"）。只有异常值得披露。
        if let Some(err) = error {
            println!(
                "📁 [模型异常已记录] 步骤 #{} (尝试 {}) 报错已归档到 {}: {}",
                step_id,
                try_index,
                log_path.display(),
                err
            );
        }
    }
}

/// 辅助 LLM 调用（子 Agent / 门禁审查）归档：写入 `<session>__<suffix>.json`
/// 独立文件，与主会话归档同 schema，供卡带提取（真实审查员牌）与停摆排查使用。
///
/// 独立文件而非并入主会话文件的原因：归档是 read-modify-write 无锁写，
/// 多个子任务与主循环并发写同一文件会互相覆盖丢条。
///
/// `call_index` 为该调用方内部的第几次 LLM 调用；`try_index` 记 transport
/// 实际尝试数（失败路径不可知记 0）。
pub fn log_auxiliary_llm_exchange(
    parent_session_id: &str,
    log_suffix: &str,
    call_index: usize,
    model_config: &ModelConfig,
    api_messages: &[serde_json::Value],
    result: Option<&llm::StreamResult>,
    status: &str,
    error: Option<&str>,
) {
    let usage = result.map(|res| match res.usage {
        Some(raw) => TokenUsage::new(
            raw.prompt_tokens,
            raw.completion_tokens,
            raw.cached_tokens,
            model_config.context_window,
            res.ttft_ms,
            res.total_duration_ms,
            res.tokens_per_second,
        ),
        None => {
            let (est_p, est_c) = llm::estimate_usage(
                api_messages,
                res.full_response.len(),
                model_config.context_window,
            );
            TokenUsage::new(
                est_p,
                est_c,
                0,
                model_config.context_window,
                res.ttft_ms,
                res.total_duration_ms,
                res.tokens_per_second,
            )
        }
    });
    log_llm_exchange_to_file(
        &format!("{}__{}", parent_session_id, log_suffix),
        call_index,
        result.map(|r| r.attempt_count as usize).unwrap_or(0),
        model_config,
        api_messages,
        result.map(|r| r.thinking_text.as_str()).unwrap_or(""),
        result.map(|r| r.full_response.as_str()).unwrap_or(""),
        usage.as_ref(),
        status,
        error,
        result.and_then(|r| r.finish_reason.as_deref()),
    );
}

pub async fn call_llm(
    messages: &SandwichContext,
    state: &State<'_, DbState>,
    model_id: String,
    model_tier: &str,
    app: Option<&tauri::AppHandle>,
    step_id: usize,
    session_id: &str,
    log_context: bool,
    transient_request_retries: u32,
    reasoning_effort: &str,
    _can_early_challenge_finish: bool,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<(AgentInstruction, TokenUsage, String, Option<usize>), String> {
    // 1. 从数据库解析模型配置
    let config = ModelConfig::from_db(state, &model_id)?;
    config.log_platform_info();

    // 2. 组装消息（保持原始序列）+ provider 能力归一化。
    // OpenAI 兼容路由（火山/DeepSeek/中转站/Ollama 等）直接发送原序列：
    // 历史尾部字节跨轮稳定，前缀缓存不被每轮重渲染的状态卡打断。
    // 仅要求严格 user/assistant 交替的 provider（Anthropic 系）才归并。
    let api_messages = normalize_messages_for_provider(messages.assemble_messages(), &config);
    let history_estimate_at_request = messages.history_estimated_tokens();
    print_debug_messages(&api_messages);

    // � 尾部状态卡分段计量（审计四问前置取证）：只测不改——把实际发出的
    // 卡片按【段名】切成字节数落库，给后续裁剪/降档决策提供分段证据。
    {
        let sections =
            crate::agent::context::assembler::measure_card_sections(&api_messages);
        if !sections.is_empty() {
            let level = match messages.budget_config.state_card_level {
                crate::agent::context::budget::StateCardLevel::Off => "off",
                crate::agent::context::budget::StateCardLevel::Minimal => "minimal",
                crate::agent::context::budget::StateCardLevel::Full => "full",
            };
            crate::agent::telemetry::record_card_sections(
                state, session_id, step_id, level, &sections,
            );
        }
        // 整提示分桶（圆环悬浮明细数据源）：与卡片分段同表异 level 落库。
        let breakdown =
            crate::agent::context::assembler::measure_prompt_breakdown(&api_messages);
        if !breakdown.is_empty() {
            crate::agent::telemetry::record_card_sections(
                state, session_id, step_id, "prompt_breakdown", &breakdown,
            );
        }
    }

    // �📊 逐调用 telemetry 的当轮上下文快照（循环外算一次，各请求行共用）：
    // 前缀指纹仅在完整上下文首次请求时有效；修复重发行固定 NULL。
    let telemetry_history_count = messages.turns_history.len() as i64;
    let telemetry_fetched_tools_count = messages.fetched_tools.len() as i64;
    let telemetry_prefix_fingerprint: Option<String> = Some(
        crate::agent::context::determinism::ContextFingerprint::capture(messages)
            .combined_prefix_hash
            .chars()
            .take(16)
            .collect(),
    );

    // 3. 转为 serde_json::Value 供 llm 层使用
    let mut api_values: Vec<serde_json::Value> = api_messages
        .iter()
        .map(|m| json!({"role": m.role, "content": &m.content}))
        .collect();
    append_final_json_output_anchor(&mut api_values);

    // 4. JSON 解析重试循环 (最多 2 次：1 次初试 + 1 次纠偏)
    let mut current_try = 0;
    let max_retries = 2;
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
        let mut on_event = |event: StreamEvent| -> bool {
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
            true
        };

        // 5. 调用 llm 层发送请求（含 HTTP 重试 + 端点韧性等待：硬失败等恢复窗口再重试，任务不再因单次端点故障即死）
        let result = match llm::stream_with_resilient_retry(
            &config,
            &api_values,
            reasoning_effort,
            interrupt_rx,
            transient_request_retries,
            &mut on_event,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                // 📊 请求级失败也占一行 telemetry：失败重试成本不再从账本上消失
                crate::agent::telemetry::record_llm_call(
                    state.inner(),
                    &crate::agent::telemetry::LlmCallRecord {
                        session_id,
                        step_id: step_id as i64,
                        model_id: &model_id,
                        model_tier,
                        reasoning_effort,
                        request_kind: if uses_primary_prompt {
                            "full_context"
                        } else {
                            "regeneration_retry"
                        },
                        usage_source: "unavailable",
                        outcome: "request_error",
                        request_try: current_try as i64,
                        transport_attempt_count: 0,
                        usage: &TokenUsage::default(),
                        history_message_count: telemetry_history_count,
                        fetched_tools_count: telemetry_fetched_tools_count,
                        prefix_fingerprint: None,
                    },
                );
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
                        None,
                    );
                }
                return Err(error);
            }
        };



        // 6. Token 用量与速度指标统计（usage 来源区分：provider 真实值 / estimated 估算）
        let (token_usage, usage_source) = match result.usage {
            Some(raw) => (
                TokenUsage::new(
                    raw.prompt_tokens,
                    raw.completion_tokens,
                    raw.cached_tokens,
                    config.context_window,
                    result.ttft_ms,
                    result.total_duration_ms,
                    result.tokens_per_second,
                ),
                "provider",
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
                (usage, "estimated")
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

        // 只执行正式回复；思考文本独立归档，不能冒充工具指令。
        let final_json_str = &result.full_response;
        let parse_result = parse_instruction_response(
            final_json_str, &result.thinking_text, result.finish_reason.as_deref(),
        );

        match parse_result {
            Ok(inst) => {
                // 📊 成功行：instruction_ready（含完整/修复两种请求形态，各自成行）
                crate::agent::telemetry::record_llm_call(
                    state.inner(),
                    &crate::agent::telemetry::LlmCallRecord {
                        session_id,
                        step_id: step_id as i64,
                        model_id: &model_id,
                        model_tier,
                        reasoning_effort,
                        request_kind: if uses_primary_prompt {
                            "full_context"
                        } else {
                            "regeneration_retry"
                        },
                        usage_source,
                        outcome: "instruction_ready",
                        request_try: current_try as i64,
                        transport_attempt_count: result.attempt_count as i64,
                        usage: &token_usage,
                        history_message_count: telemetry_history_count,
                        fetched_tools_count: telemetry_fetched_tools_count,
                        prefix_fingerprint: if uses_primary_prompt {
                            telemetry_prefix_fingerprint.as_deref()
                        } else {
                            None
                        },
                    },
                );
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
                        result.finish_reason.as_deref(),
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
                let outcome = if e.starts_with("[output_incomplete]") { "output_incomplete" } else { "parse_error" };
                // 📊 解析失败行：格式错误的真实成本（请求已发生、token 已消耗）
                // 各占一行，不再只被最终成功的快照掩盖
                crate::agent::telemetry::record_llm_call(
                    state.inner(),
                    &crate::agent::telemetry::LlmCallRecord {
                        session_id,
                        step_id: step_id as i64,
                        model_id: &model_id,
                        model_tier,
                        reasoning_effort,
                        request_kind: if uses_primary_prompt {
                            "full_context"
                        } else {
                            "regeneration_retry"
                        },
                        usage_source,
                        outcome,
                        request_try: current_try as i64,
                        transport_attempt_count: result.attempt_count as i64,
                        usage: &token_usage,
                        history_message_count: telemetry_history_count,
                        fetched_tools_count: telemetry_fetched_tools_count,
                        prefix_fingerprint: None,
                    },
                );
                log_llm_exchange_to_file(
                    session_id,
                    step_id,
                    current_try,
                    &config,
                    &api_values,
                    &result.thinking_text,
                    &final_json_str,
                    Some(&token_usage),
                    outcome,
                    Some(&e),
                    result.finish_reason.as_deref(),
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
                            "error_kind": outcome,
                            "finish_reason": result.finish_reason,
                            "raw_response": &final_json_str,
                            "thinking_text": &result.thinking_text,
                        }),
                    );
                    let _ = app_handle.emit(
                        &format!("agent-log-{}", session_id),
                        format!(
                            "⚠️ [模型响应异常·第{}次尝试] {}（原始响应已归档）",
                            current_try, e
                        ),
                    );
                }

                if current_try >= max_retries {
                    return Err(e);
                }
                println!(
                    "⚠️ 未获得有效工具指令，按既有额度重试 (第 {} 次 / 共 {} 次): {}",
                    current_try, max_retries, e
                );

                // 将模型上一轮的原始回复作为 assistant turn 追加，保持对话轮次完整性
                api_values.push(json!({"role": "assistant", "content": &result.full_response}));
                // 保留完整任务历史与工具定义，追加精准纠偏提示让模型在原位上下文重新生成合规 JSON
                append_json_regeneration_feedback(&mut api_values, &e);
                uses_primary_prompt = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn video_run_reasoning_is_never_an_executable_instruction() {
        let instruction = r#"{"tool":"local_shell","command":{"command":"echo should-not-run"}}"#;
        let error = super::parse_instruction_response("", instruction, None).err().unwrap();
        assert!(error.starts_with("[output_incomplete]"));
        assert!(!error.contains("should-not-run"));
        assert!(super::instruction_recovery_feedback(&error).contains("下一项"));
    }

    #[test]
    fn video_run_provider_truncation_is_not_repaired_into_an_action() {
        let instruction = r#"{"tool":"local_shell","command":{"command":"echo ok"}}"#;
        for reason in ["length", "max_tokens"] {
            let error = super::parse_instruction_response(instruction, "", Some(reason)).err().unwrap();
            assert!(error.starts_with("[output_incomplete]"));
        }
        assert!(super::parse_instruction_response(instruction, "unrelated reasoning", Some("stop")).is_ok());
        assert!(super::parse_instruction_response(instruction, "", None).is_ok());
    }

    #[test]
    fn video_run_syntax_error_does_not_echo_the_entire_response() {
        let raw = "not json ".repeat(10000);
        let error = super::parse_instruction_response(&raw, "", None).err().unwrap();
        assert!(error.len() < 1000);
        assert!(!error.starts_with("[output_incomplete]"));
        let mut messages = vec![serde_json::json!({"role":"user","content":"existing task"})];
        super::append_json_regeneration_feedback(&mut messages, &error);
        assert_eq!(messages[0]["content"], "existing task");
    }

    #[test]
    fn unused_parallel_placeholders_do_not_override_model_single_step() {
        // 旧兼容继续成立；[] 和此前的空字符串、false、null 一样表示未使用。
        for placeholder in [json!([]), json!(""), json!(false), json!(null)] {
            let raw = json!({
                "tool":"local_shell", "command":{"command":"printf ok"},
                "parallel":placeholder
            }).to_string();
            let instruction = parse_executable_instruction(&raw).unwrap();
            assert!(instruction.parallel.is_none());
            assert_eq!(instruction.get_tool(), "local_shell");
            assert_eq!(instruction.get_params()["command"], "printf ok");
        }
    }

    #[test]
    fn model_empty_parallel_without_an_action_is_rejected() {
        assert!(parse_executable_instruction(r#"{"parallel":[]}"#).is_err());
        let instruction = parse_executable_instruction(
            r#"{"parallel":[{"tool":"local_file","command":{"path":"a.txt"}}]}"#,
        ).unwrap();
        assert_eq!(instruction.get_tool(), "parallel");
        assert_eq!(instruction.parallel.as_ref().unwrap().len(), 1);
    }

    use super::*;

    #[test]
    fn finish_parameter_aliases_preserve_the_complete_delivery_in_dispatch() {
        use crate::agent::{dispatcher::run_agent_step, mcp::PluginRegistry};
        use std::sync::{Arc, Mutex};
        let db = DbState(Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())));
        let report = json!({"summary":"摘要", "details":"# 正文\n\n**结论**\n\n```json\n{\"tool\":\"finish\"}\n```"});
        let expected = format!("摘要\n\n{}", report["details"].as_str().unwrap());
        for raw in [
            json!({"tool":"finish", "command":report}),
            json!({"tool":"finish", "params":report}),
            json!({"tool":"finish", "command":null, "params":report}),
            json!({"tool":"finish", "command":"", "params":report}),
            json!({"tool":"finish", "command":"finish", "params":report}),
            json!({"tool":"finish", "command":report.to_string()}),
            json!({"tool":"finish", "summary":report["summary"], "details":report["details"]}),
            json!({"tool":"finish", "command":report, "params":{"summary":"不能覆盖正式参数"}}),
        ] {
            let inst = parse_executable_instruction(&raw.to_string()).unwrap();
            assert_eq!(inst.get_params()["details"], report["details"]);
            let result = run_agent_step(&db, "finish-alias", &inst, &PluginRegistry::new(), None);
            assert!(result.success);
            assert_eq!(result.stdout, expected, "{raw}");
        }
        let inst = parse_executable_instruction(&json!({"tool":"finish", "command":"",
            "params":{"summary":"", "details":"# 仅正文"}}).to_string()).unwrap();
        let result = run_agent_step(&db, "finish-details", &inst, &PluginRegistry::new(), None);
        assert_eq!(result.stdout, "# 仅正文");
    }

    #[test]
    fn malformed_summary_cannot_turn_trailing_protocol_fields_into_delivery_text() {
        // Synthetic shape of the saved incident; original provider response was not archived.
        for raw in [
            r#"{"description":"交付结论","params":{"summary":"报告包含 "引用"，说明完成。"},"plan_title":"评估任务","tool":"finish"}"#,
            r#"{"tool":"finish","command":{"summary":"报告包含 "引用"。","details":"详细结论"},"todo_update":[{"id":1,"status":"done"}]}"#,
        ] {
            assert!(parse_executable_instruction(raw).is_err(), "malformed output must enter existing format correction");
            assert!(extract_json_from_text(raw).is_none(), "extraction cannot manufacture a different finish instruction");
        }
    }

    #[test]
    fn malformed_summary_in_non_finish_tool_cannot_be_promoted_to_finish() {
        let raw = r#"{"tool":"local_shell","command":{"summary":"说明包含 "引用"。","command":"printf untouched"}}"#;
        assert!(parse_executable_instruction(raw).is_err(), "the presence of summary is not a finish request");
        assert!(extract_json_from_text(raw).is_none());
    }

    #[test]
    fn valid_delivery_markdown_and_protocol_examples_remain_opaque() {
        let summary = "## 结论\n\n保留 **Markdown**、\"引用\"、换行与末尾符号。\n```json\n{\"tool\":\"finish\",\"plan_title\":\"示例\",\"summary\":\"文档\"}\n```\n末尾：\"},]";
        let details = "详细依据包含 todo_update 与 review_contract_update 字面量。";
        let raw = json!({"description":"交付", "params":{"summary":summary,"details":details},
            "plan_title":"当前任务", "tool":"finish", "todo_update":[]}).to_string();
        let extracted = extract_json_from_text(&raw).unwrap();
        let inst = parse_executable_instruction(&extracted).unwrap();
        assert_eq!(inst.get_tool(), "finish");
        assert_eq!(inst.get_params()["summary"], summary);
        assert_eq!(inst.get_params()["details"], details);
    }

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
        assert!(feedback.contains("JSON 指令"));
        assert!(feedback.contains("expected value at line 1 column 1"));
        assert!(feedback.contains("本次响应未执行"));
        assert!(!feedback.contains("<think>"));
        assert!(!feedback.contains("HTML"));
        assert_eq!(messages[2]["content"], FINAL_JSON_OUTPUT_ANCHOR);
    }
    #[test]
    fn pure_text_protocol_failures_escalate_to_outer_recovery() {
        assert!(requires_outer_protocol_recovery("计划：先建立项目地图"));
        assert!(requires_outer_protocol_recovery(
            "I will inspect the repository first."
        ));
        assert!(!requires_outer_protocol_recovery(
            "{\"tool\":\"read_text_file\"}"
        ));
    }

    #[test]
    fn executable_parser_rejects_valid_json_without_action() {
        let error =
            parse_executable_instruction(r#"{"plan_title":"审查上下文","description":"继续读取"}"#)
                .expect_err("state-only JSON must enter recoverable validation");

        assert!(error.contains("指令可执行性校验失败"));
        assert!(error.contains("缺少可执行入口"));
    }

    #[test]
    fn executable_parser_accepts_valid_tool_instruction() {
        let instruction = parse_executable_instruction(
            r#"{"tool":"read_text_file","command":{"path":"src/lib.rs"}}"#,
        )
        .expect("tool instruction should remain executable");

        assert_eq!(instruction.get_tool(), "read_text_file");
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
pub(crate) fn provider_requires_alternating_roles(config: &crate::llm::ModelConfig) -> bool {
    match config.requires_alternating_roles {
        1 => true,
        0 => false,
        // 自动探测：除 base_url 嗅探外，按模型家族识别 Claude 系——
        // 中转站（如本地反代）base_url 不含 "anthropic"，但上游仍是严格交替协议；
        // 连续同角色消息经其转换会给 assistant 轮注入无签名 thinking 块（实测 400:
        // messages.N.content.0.thinking.signature: Field required）。
        _ => {
            if config.base_url.to_ascii_lowercase().contains("anthropic") {
                return true;
            }
            matches!(
                crate::agent::model_filter::detect_model_family(
                    &config.model_name,
                    &config.base_url
                ),
                crate::agent::model_filter::ModelFamily::ClaudeThinking
            )
        }
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
            ChatMessage {
                role: "user".to_string(),
                content: json!("历史输出 C"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【状态卡】"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【决策指令】"),
            },
        ]
    }

    #[test]
    fn openai_compatible_routes_keep_original_sequence() {
        for url in [
            "https://ark.cn-beijing.volces.com/api/coding/v3",
            "https://api.deepseek.com",
            "https://zhandou.online/v1",
        ] {
            let normalized =
                normalize_messages_for_provider(sample_messages(), &config_with_base_url(url));
            assert_eq!(normalized.len(), 3, "OpenAI 兼容路由不得归并：{}", url);
            assert_eq!(
                normalized[0].content,
                json!("历史输出 C"),
                "历史消息必须保持独立"
            );
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
        assert!(
            text.contains("历史输出 C")
                && text.contains("【状态卡】")
                && text.contains("【决策指令】")
        );
    }
}

#[cfg(test)]
mod completion_todo_regressions {
    use super::*;
    use crate::agent::types::TodoItem;

    fn context() -> SandwichContext {
        let mut context = SandwichContext::new("sys".into(), "只读审查并交付结论".into());
        context.todo_list = [1, 2, 4]
            .into_iter()
            .map(|id| TodoItem {
                id,
                status: "pending".into(),
                description: format!("[审查发现] [R-{id}] [BLOCKING] 原始问题"),
                evidence_output_id: None,
            })
            .collect();
        context
    }

    #[test]
    fn nested_completion_todos_close_real_finding_ids_without_losing_descriptions() {
        for entry in ["params", "command"] {
            for finish in ["finish", "prepare_finish"] {
                let mut raw = json!({"tool":finish});
                raw[entry] = json!({"summary":"只读审查报告", "todo_update":[
                    {"id":"#1", "status":"done", "note":"已核验第一项，结论见报告"},
                    {"id":"#2", "status":"done", "title":"原始问题二", "note":"已核验第二项"},
                    {"id":"4", "status":"done", "note":"证据缺口已补齐"}
                ], "change_verification_update":{"status":"waived","waived_reason":"本轮只读"}});
                let instruction = parse_executable_instruction(&raw.to_string()).unwrap();
                assert_eq!(
                    instruction
                        .todo_update
                        .iter()
                        .map(|t| t.id)
                        .collect::<Vec<_>>(),
                    vec![1, 2, 4]
                );
                assert!(instruction.change_verification_update.is_some());
                let mut context = context();
                let names = context
                    .todo_list
                    .iter()
                    .map(|t| t.description.clone())
                    .collect::<Vec<_>>();
                crate::agent::finish_guard::apply_instruction_state_updates(
                    &mut context,
                    &instruction,
                    13,
                );
                assert!(crate::agent::finish_guard::finish_blockers(
                    &context.todo_list,
                    None,
                    false,
                    0,
                    None
                )
                .is_empty());
                assert_eq!(
                    context
                        .todo_list
                        .iter()
                        .map(|t| t.description.clone())
                        .collect::<Vec<_>>(),
                    names
                );
                assert!(context
                    .turns_history
                    .iter()
                    .any(|m| m.content.to_string().contains("已核验第一项")));
                let notes = context.turns_history.len();
                crate::agent::finish_guard::apply_todo_update_guarded(
                    &mut context,
                    &instruction.todo_update,
                );
                assert_eq!(
                    context.turns_history.len(),
                    notes,
                    "证据回填不重复写处理说明"
                );
            }
        }
    }

    #[test]
    fn bare_prepare_and_action_finish_keep_nested_updates_and_legacy_todos_readable() {
        for raw in [
            json!({"prepare_finish":{"summary":"报告", "todo_update":[{"id":"#1","status":"done","note":"已核验"}]}}),
            json!({"action":"finish", "command":{"summary":"报告", "todo_update":"[{\"id\":\"1\",\"status\":\"done\"}]"}}),
        ] {
            let instruction = parse_executable_instruction(&raw.to_string()).unwrap();
            assert_eq!(instruction.todo_update.len(), 1);
            assert_eq!(instruction.todo_update[0].id, 1);
        }
        let legacy: TodoItem =
            serde_json::from_value(json!({"id":"old-id", "status":"done"})).unwrap();
        assert_eq!(legacy.id, 0, "旧检查点不因实时校验变严格而无法恢复");
    }

    #[test]
    fn bad_or_duplicate_todo_ids_report_errors_instead_of_becoming_zero() {
        for id in [
            json!("#oops"),
            json!(-1),
            json!(1.5),
            json!(true),
            json!(null),
            json!({}),
        ] {
            let raw = json!({"tool":"finish", "params":{"summary":"报告", "todo_update":[{"id":id,"status":"done"}]}});
            let error = parse_executable_instruction(&raw.to_string()).unwrap_err();
            assert!(error.contains("todo_update[0].id"), "{error}");
        }
        for updates in [
            json!([{"status":"done"}]),
            json!([{"id":"#1","status":"done"},{"id":1,"status":"done"}]),
            json!("not an array"),
        ] {
            let raw = json!({"tool":"finish", "command":{"summary":"报告", "todo_update":updates}});
            assert!(parse_executable_instruction(&raw.to_string())
                .unwrap_err()
                .contains("todo_update"));
        }
    }

    #[test]
    fn completion_updates_have_explicit_precedence_and_do_not_leak_from_other_tools() {
        let raw = json!({"tool":"finish", "todo_update":[{"id":0,"status":"done","description":"顶层任务"}],
            "params":{"summary":"报告", "todo_update":[{"id":"#1","status":"done"}]}});
        let instruction = parse_executable_instruction(&raw.to_string()).unwrap();
        assert_eq!(instruction.todo_update.len(), 1);
        assert_eq!(instruction.todo_update[0].id, 0);
        let raw = json!({"tool":"local_shell", "params":{"command":"echo test", "todo_update":[{"id":1,"status":"done"}]}});
        let instruction = parse_executable_instruction(&raw.to_string()).unwrap();
        assert!(instruction.todo_update.is_empty());
        assert!(instruction.get_params().get("todo_update").is_some());
        let mut context = context();
        crate::agent::finish_guard::apply_instruction_state_updates(&mut context, &instruction, 1);
        assert_eq!(
            crate::agent::finish_guard::finish_blockers(&context.todo_list, None, false, 0, None)
                .len(),
            3
        );
    }
}
