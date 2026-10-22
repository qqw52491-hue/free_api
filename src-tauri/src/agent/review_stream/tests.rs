use super::*;
use serde_json::json;

fn result() -> StreamResult {
    StreamResult {
        full_response: "{}".into(),
        thinking_text: String::new(),
        finish_reason: None,
        usage: None,
        ttft_ms: 0,
        total_duration_ms: 0,
        tokens_per_second: 0.0,
        attempt_count: 1,
    }
}

fn budget() -> Budget {
    Budget {
        idle: Duration::from_secs(60),
        total: Duration::from_secs(240),
    }
}

fn emit(activity: &Arc<Mutex<Activity>>, thinking: bool, text: &str) {
    // 真实 SSE 解析器：保活、usage 和空 delta 不会产生 Thinking/Content 活动。
    let delta = if thinking {
        json!({"reasoning_content":text})
    } else {
        json!({"content":text})
    };
    let line = format!("data: {}", json!({"choices":[{"delta":delta}]}));
    let mut on_event = |event: StreamEvent<'_>| {
        activity.lock().unwrap().observe(&event);
        true
    };
    llm::process_sse_line(
        &line,
        &mut String::new(),
        &mut String::new(),
        &mut None,
        &mut None,
        &mut None,
        &mut on_event,
    );
}

#[tokio::test(start_paused = true)]
async fn no_output_times_out_without_inventing_progress() {
    let state = Arc::new(Mutex::new(Activity::new()));
    let failure = wait_for_stream(std::future::pending(), state, budget())
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind, Some(TimeoutKind::FirstOutput));
    assert_eq!(failure.elapsed_ms, 60_000);
    assert_eq!(failure.first_output_ms, None);
    assert!(failure.may_retry_timeout());
}

#[tokio::test(start_paused = true)]
async fn thinking_and_content_keep_the_original_request_alive_past_sixty_seconds() {
    let state = Arc::new(Mutex::new(Activity::new()));
    let observed = state.clone();
    let call = async move {
        for i in 0..4 {
            tokio::time::sleep(Duration::from_secs(40)).await;
            emit(&observed, i % 2 == 0, "仍在生成");
        }
        Ok(result())
    };
    assert!(wait_for_stream(call, state.clone(), budget()).await.is_ok());
    assert_eq!(
        state.lock().unwrap().started.elapsed(),
        Duration::from_secs(160)
    );
}

#[tokio::test(start_paused = true)]
async fn output_then_silence_preserves_partial_diagnostics_but_never_returns_a_verdict() {
    let state = Arc::new(Mutex::new(Activity::new()));
    let observed = state.clone();
    let call = async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        emit(&observed, true, "正在核验报告");
        tokio::time::sleep(Duration::from_secs(10)).await;
        emit(&observed, false, "{\"verdict\":");
        std::future::pending().await
    };
    let failure = wait_for_stream(call, state, budget()).await.err().unwrap();
    assert_eq!(failure.kind, Some(TimeoutKind::Idle));
    assert_eq!(
        (failure.elapsed_ms, failure.idle_ms, failure.first_output_ms),
        (80_000, 60_000, Some(10_000))
    );
    assert_eq!(failure.content_prefix, "{\"verdict\":");
    assert_eq!(failure.thinking_prefix, "正在核验报告");
    assert!(failure.description().contains("不构成完整审查结论"));
}

#[tokio::test(start_paused = true)]
async fn keepalive_and_empty_deltas_do_not_extend_the_idle_deadline() {
    let state = Arc::new(Mutex::new(Activity::new()));
    let observed = state.clone();
    let call = async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            emit(&observed, false, "");
            let mut callback = |event: StreamEvent<'_>| {
                observed.lock().unwrap().observe(&event);
                true
            };
            llm::process_sse_line(
                ": keepalive",
                &mut String::new(),
                &mut String::new(),
                &mut None,
                &mut None,
                &mut None,
                &mut callback,
            );
        }
    };
    let failure = wait_for_stream(call, state, budget()).await.err().unwrap();
    assert_eq!(failure.kind, Some(TimeoutKind::FirstOutput));
    assert_eq!(failure.elapsed_ms, 60_000);
}

#[tokio::test(start_paused = true)]
async fn continuous_output_cannot_extend_the_existing_total_budget() {
    let state = Arc::new(Mutex::new(Activity::new()));
    let observed = state.clone();
    let call = async move {
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            emit(&observed, true, "继续生成");
        }
    };
    let failure = wait_for_stream(call, state, budget()).await.err().unwrap();
    assert_eq!(failure.kind, Some(TimeoutKind::Total));
    assert_eq!(failure.elapsed_ms, 240_000);
    assert!(!failure.may_retry_timeout());
    assert!(failure.thinking_chars > 0);
}

#[tokio::test(start_paused = true)]
async fn cancellation_remains_cancellation_and_diagnostic_capture_is_bounded() {
    let state = Arc::new(Mutex::new(Activity::new()));
    emit(&state, true, &"字".repeat(CAPTURE_CHARS + 20));
    let failure = wait_for_stream(
        async { Err(llm::STREAM_INTERRUPTED_ERROR.into()) },
        state,
        budget(),
    )
    .await
    .err()
    .unwrap();
    assert!(!failure.is_timeout());
    assert!(!failure.may_retry_timeout());
    assert_eq!(failure.error, llm::STREAM_INTERRUPTED_ERROR);
    assert_eq!(failure.thinking_chars, CAPTURE_CHARS + 20);
    assert_eq!(failure.thinking_prefix.chars().count(), CAPTURE_CHARS);
}

#[tokio::test]
async fn real_stream_outlives_old_http_timeout_and_user_stop_still_interrupts_it() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            count.fetch_add(1, Ordering::Relaxed);
            async {
                let stream = futures_util::stream::unfold(0usize, |index| async move {
                    if index >= 8 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(180)).await;
                    let text = if index == 0 {
                        "{"
                    } else if index == 7 {
                        "}"
                    } else {
                        " "
                    };
                    let frame = format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":text}}]})
                    );
                    Some((Ok::<_, std::convert::Infallible>(frame), index + 1))
                });
                axum::response::Response::builder()
                    .header("Content-Type", "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = ModelConfig {
        model_id: uuid::Uuid::new_v4().to_string(),
        platform_name: "local-review-timeout-test".into(),
        base_url: format!("http://{address}/v1"),
        api_key: "test-only".into(),
        model_name: "mock".into(),
        max_tokens: 8192,
        temperature: 0.0,
        is_ollama: false,
        is_lmstudio: false,
        context_window: 32_768,
        provider_capabilities: "{}".into(),
        timeout_secs: 1,
        requires_alternating_roles: 0,
    };
    let messages = vec![json!({"role":"user","content":"核验报告"})];
    let (tx, mut rx) = watch::channel(0u64);
    let completed = stream_review(&config, &messages, "auto", &mut rx, Some(1), 1, &mut |_| {
        true
    })
    .await
    .unwrap_or_else(|e| panic!("{}", e.description()));
    assert!(
        completed.total_duration_ms > 1_000,
        "必须真的跨过原 HTTP 1 秒上限"
    );
    assert!(serde_json::from_str::<Value>(&completed.full_response).is_ok());
    assert_eq!(config.timeout_secs, 1, "只改变调用副本，不改变共享模型配置");
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let mut signal = Some(seen_tx);
    let call = tokio::spawn(async move {
        stream_review(
            &config,
            &messages,
            "auto",
            &mut rx,
            Some(1),
            1,
            &mut |event| {
                if let Some(signal) = signal.take() {
                    let _ = signal.send(());
                }
                let _ = event;
                true
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), seen_rx)
        .await
        .unwrap()
        .unwrap();
    tx.send_modify(|v| *v += 1);
    let stopped = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(stopped.error, llm::STREAM_INTERRUPTED_ERROR);
    assert!(!stopped.may_retry_timeout());
    assert_eq!(
        requests.load(Ordering::Relaxed),
        2,
        "成功和中断都不应额外重发请求"
    );
    server.abort();
}
