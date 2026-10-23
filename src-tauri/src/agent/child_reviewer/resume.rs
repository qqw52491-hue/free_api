//! 续接审查保留完整契约产物，提交错误沿用首次审查的有界补正语义。
use super::*;

pub(crate) type ResumeProgressSink = std::sync::Arc<dyn Fn(Value) + Send + Sync>;

pub(crate) struct ResumeReviewError {
    pub route: &'static str,
    pub message: String,
}

/// 与界面无关的实际审查对质入口：恢复同一任务、验证输入、预扣预算、落账并派发。
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_review_resume(
    db: &DbState,
    parent_run: &str,
    session: &str,
    workspace: &Path,
    target: &str,
    message: &str,
    mut config: ModelConfig,
    effort: String,
    checkpoint: &mut crate::agent::checkpoint::AgentCheckpoint,
    interrupt: watch::Receiver<u64>,
    progress: Option<ResumeProgressSink>,
) -> Result<String, ResumeReviewError> {
    let error = |route, message| ResumeReviewError { route, message };
    crate::agent::review_control::ensure_allowed(db, session, &config.model_id)
        .map_err(|message| error("host_rejection:reviewer_excluded", message))?;
    let (status, kind, _, history, _, _, previous, identity) =
        load_child_run_for_resume(db, target, session)
            .map_err(|e| error("host_rejection:child_not_found", e))?;
    let invalid_with_findings = status == "invalid_output"
        && previous
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .is_some_and(|v| {
                ["findings", "objection_verdicts"].iter().any(|key| {
                    v.get(key)
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty())
                })
            });
    if !matches!(kind.as_str(), "review" | "reviewer")
        || (status != "succeeded" && !invalid_with_findings)
    {
        return Err(error(
            "host_rejection:status_not_succeeded",
            "目标不是可续接的审查任务，未派发对质。".into(),
        ));
    }
    let mut messages: Vec<Value> = history
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    retire_consumer_history(&mut messages);
    refresh_review_instructions(&mut messages);
    if messages.is_empty() {
        return Err(error(
            "host_rejection:missing_history",
            "该审查任务未留存历史对话记录，无法恢复对质。".into(),
        ));
    }
    let stage = checkpoint.is_stage_review_target(db, target);
    let mut identity = ChildRunIdentity::inherited(
        &identity,
        if stage {
            Some("rebuttal".into())
        } else {
            identity.purpose.clone().or(Some("review".into()))
        },
    );
    retire_consumer_schema_json(&mut identity.contract_schema);
    let schema = identity
        .contract_schema
        .as_deref()
        .map(serde_json::from_str::<crate::agent::protocol::ContractSchema>)
        .transpose()
        .map_err(|e| {
            error(
                "resume_child:invalid_saved_schema",
                format!("审查契约记录无法解析，未发起模型请求；原记录已保留：{e}"),
            )
        })?;
    messages.push(json!({"role":"user","content":format!(
        "【父 Agent 反驳与对质】\n{message}\n\n【对质研判规则】\n请提交完整的更新审查 JSON。旧问题沿用原 finding_id，新问题使用未用过的编号；编号含义不要改变。\n若反驳或修复证据成立，在 objection_verdicts 中将对应 finding_id 标为 withdraw 并给出理由；仍成立则 maintain。没有提及旧发现不等于撤回，也不要撤回其他审查任务的发现。重新判断 decision 和 routing。" )}));
    if let Some(schema) = schema.as_ref() {
        messages.push(json!({"role":"user","content":format!("请重新提交完整审查结果，不能只提交差异或省略仍适用的必填字段。\n{}", schema.render_prompt_instructions())}));
    }
    let can_finalize = previous
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("plan_review_can_finalize").and_then(Value::as_bool));
    let old_budget = checkpoint.plan_reviewer_retry_count;
    let old_consumed = checkpoint.plan_reviewer_consumed;
    if !checkpoint.plan_phase.is_approved() {
        checkpoint
            .reserve_plan_review_round()
            .map_err(|e| error("plan_gatekeeper:reviewer_retry_limit", e))?;
    }
    let id = match begin_child_run_extended(
        db,
        parent_run,
        session,
        workspace,
        &config.model_id,
        0,
        "review",
        None,
        None,
        Some(target),
        &identity,
    ) {
        Ok(id) => id,
        Err(e) => {
            checkpoint.plan_reviewer_retry_count = old_budget;
            checkpoint.plan_reviewer_consumed = old_consumed;
            return Err(error("host_rejection:begin_run_failed", e));
        }
    };
    if !stage
        && can_finalize.is_some()
        && checkpoint.submitted_plan.is_some()
        && checkpoint.run_belongs_to_plan_batch(db, &id)
    {
        // 已批准后的同批复审仍须等新尝试收口；失败会保留旧有效裁决，由原门禁重算。
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        checkpoint.plan_reviewer_consumed = false;
    }
    config.max_tokens = 900;
    tokio::spawn(run_resumed_review(
        DbState(db.0.clone()),
        id.clone(),
        session.into(),
        config,
        effort,
        messages,
        schema,
        can_finalize,
        interrupt,
        progress,
    ));
    Ok(id)
}

pub(crate) async fn run_resumed_review(
    db: DbState,
    child_id: String,
    session_id: String,
    config: ModelConfig,
    effort: String,
    mut messages: Vec<Value>,
    schema: Option<crate::agent::protocol::ContractSchema>,
    can_finalize: Option<bool>,
    mut interrupt_rx: watch::Receiver<u64>,
    progress: Option<ResumeProgressSink>,
) {
    let registration = match crate::agent::review_control::register(&db, &session_id, &child_id, &config.model_id, true, interrupt_rx.clone()) {
        Ok(registration) => registration,
        Err(_) => {
            if let Ok(conn) = db.0.lock() { let _ = crate::agent::review_control::cancel_child_on(&conn, &child_id); }
            crate::agent::job_engine::notify_job_state_changed();
            return;
        }
    };
    interrupt_rx = registration.interrupt();
    let _registration = registration;
    let started = std::time::Instant::now();
    let log_suffix = format!("child-{child_id}");
    let stage = format!("child:{child_id}");
    let mut completion_tokens = 0;
    let mut prompt_tokens = 0;
    for attempt in 1..=MAX_REVIEW_OUTPUT_ATTEMPTS {
        crate::agent::control::mark_stage(&stage, &format!("child_task:llm_call:{attempt}"));
        if let Some(sink) = &progress {
            sink(json!({"type":"child_reviewer_action", "child_run_id":child_id,
                "step":attempt, "action":"deliberate", "params":{"round":attempt}}));
        }
        let mut activity = std::time::Instant::now();
        let mut on_event = |event: crate::llm::StreamEvent<'_>| {
            if activity.elapsed() >= Duration::from_secs(5) {
                crate::agent::control::mark_stage(&stage, "child_task:stream_output");
                activity = std::time::Instant::now();
            }
            if let Some(sink) = &progress {
                let (kind, chunk) = match event {
                    crate::llm::StreamEvent::Thinking { chunk, .. } => ("thinking", chunk),
                    crate::llm::StreamEvent::Content { chunk, .. } => ("content", chunk),
                };
                if !chunk.is_empty() {
                    sink(json!({"type":"child_reviewer_stream", "child_run_id":child_id,
                        "stream_type":kind, "chunk":chunk, "model": &config.model_name}));
                }
            }
            true
        };
        let response = crate::agent::review_stream::stream_review(
            &config,
            &messages,
            &effort,
            &mut interrupt_rx,
            None,
            child_timeout_secs(&config.model_name),
            &mut on_event,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                failure.log(
                    &session_id,
                    &log_suffix,
                    attempt as usize,
                    &config,
                    &messages,
                );
                let (status, kind) = if failure.error == crate::llm::STREAM_INTERRUPTED_ERROR {
                    ("cancelled", ChildErrorKind::Cancelled)
                } else if failure.is_timeout() {
                    ("timed_out", ChildErrorKind::Timeout)
                } else {
                    ("failed", ChildErrorKind::RequestOrStream)
                };
                let elapsed = started.elapsed().as_millis() as u64;
                finish_child_run_with_history(
                    &db,
                    &child_id,
                    status,
                    elapsed,
                    completion_tokens,
                    prompt_tokens,
                    Some(kind.clone()),
                    Some(&child_error_result_json(
                        &child_id,
                        status,
                        &kind,
                        elapsed,
                        completion_tokens,
                        prompt_tokens,
                        Some(&failure.description()),
                    )),
                    serde_json::to_string(&messages).ok().as_deref(),
                );
                return;
            }
        };
        if let Some(usage) = response.usage {
            completion_tokens += usage.completion_tokens;
            prompt_tokens += usage.prompt_tokens;
        }
        let elapsed = started.elapsed().as_millis() as u64;
        let raw = &response.full_response;
        let delivery = parse_controlled_output(raw, &[]).and_then(
            |(
                findings,
                confidence,
                evidence_refs,
                missing_evidence,
                routing,
                scorecard,
                objection_verdicts,
                withdrawn_findings,
            )| {
                let result = ChildReviewResult {
                    child_run_id: child_id.clone(),
                    status: "succeeded".into(),
                    findings,
                    confidence,
                    evidence_refs,
                    missing_evidence,
                    duration_ms: elapsed,
                    completion_tokens,
                    prompt_tokens,
                    error_kind: None,
                    raw_result_json: None,
                    kind: Some("review".into()),
                    routing: Some(routing),
                    withdrawn_findings,
                    objection_verdicts,
                    scorecard,
                    task_id: None,
                    attempt_id: None,
                };
                let mut delivery = merge_review_extensions(raw, &result);
                delivery["model"] = json!(config.model_name);
                if let Some(allowed) = can_finalize {
                    delivery["plan_review_can_finalize"] = json!(allowed);
                }
                validate_review_delivery(delivery, schema.as_ref())
            },
        );
        crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
            &session_id,
            &log_suffix,
            attempt as usize,
            &config,
            &messages,
            Some(&response),
            if delivery.is_ok() {
                "success"
            } else {
                "parse_error"
            },
            delivery.as_ref().err().map(String::as_str),
        );
        // 包含最后一次有效回复，后续追问不再从上一次的补正提示继续。
        messages.push(json!({"role":"assistant", "content":raw}));
        match delivery {
            Ok(value) => {
                finish_child_run_with_history(
                    &db,
                    &child_id,
                    "succeeded",
                    elapsed,
                    completion_tokens,
                    prompt_tokens,
                    None,
                    serde_json::to_string(&value).ok().as_deref(),
                    serde_json::to_string(&messages).ok().as_deref(),
                );
                return;
            }
            Err(error) => {
                if attempt == MAX_REVIEW_OUTPUT_ATTEMPTS {
                    finish_child_run_with_history(
                        &db,
                        &child_id,
                        "invalid_output",
                        elapsed,
                        completion_tokens,
                        prompt_tokens,
                        Some(ChildErrorKind::InvalidOutput),
                        Some(&child_invalid_result_json(
                            &child_id,
                            elapsed,
                            completion_tokens,
                            prompt_tokens,
                            raw,
                            &error,
                        )),
                        serde_json::to_string(&messages).ok().as_deref(),
                    );
                    return;
                }
                messages.push(json!({"role":"user", "content":
                    review_output_rejection_feedback(&error, attempt, MAX_REVIEW_OUTPUT_ATTEMPTS)}));
            }
        }
    }
}
