//! 续接审查保留完整契约产物，提交错误沿用首次审查的有界补正语义。
use super::*;

pub(crate) async fn run_resumed_review(
    db: DbState,
    child_id: String,
    session_id: String,
    config: ModelConfig,
    effort: String,
    mut messages: Vec<Value>,
    schema: Option<crate::agent::protocol::ContractSchema>,
    mut interrupt_rx: watch::Receiver<u64>,
) {
    let started = std::time::Instant::now();
    let log_suffix = format!("child-{child_id}");
    let stage = format!("child:{child_id}");
    let mut completion_tokens = 0;
    let mut prompt_tokens = 0;
    for attempt in 1..=MAX_REVIEW_OUTPUT_ATTEMPTS {
        crate::agent::control::mark_stage(&stage, &format!("child_task:llm_call:{attempt}"));
        let mut activity = std::time::Instant::now();
        let mut on_event = |_: crate::llm::StreamEvent<'_>| {
            if activity.elapsed() >= Duration::from_secs(5) {
                crate::agent::control::mark_stage(&stage, "child_task:stream_output");
                activity = std::time::Instant::now();
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
                validate_review_delivery(merge_review_extensions(raw, &result), schema.as_ref())
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
