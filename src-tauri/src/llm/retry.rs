//! 重试编排：暂态退避、速率限制恢复与调用总预算。

//! LLM 请求基础设施
//!
//! 提供统一的模型配置、HTTP 请求构建、SSE 流式解析等公共能力，
//! 供 Agent (call_llm) 和 Chat (send_chat) 两条路径共用。

use crate::db::DbState;
use futures_util::StreamExt;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::State;
use tokio::sync::watch;


use super::*;

static GLOBAL_KEY_REQUEST_ROTATOR: AtomicUsize = AtomicUsize::new(0);


/// 单次暂态请求重试的最大等待时间。上限允许长流程在 429/网络波动时持续恢复，
/// 同时避免 20 次尝试沿用无界指数退避而等待数小时。
const MAX_TRANSIENT_RETRY_BACKOFF_MS: u64 = 15_000;

/// 单次逻辑调用（含全部重试尝试与退避）的总预算。实测端点劣化时单个请求曾在
/// 重试循环中挂起 21 分钟、吃光整个运行预算；此后任何一次调用最多消耗预算即放弃，
/// 由上层按 request_error 处理。ask_human 不经过此路径（等人不发生 LLM 调用），不受影响。
const DEFAULT_LLM_CALL_TOTAL_BUDGET_MS: u64 = 240_000;
const MIN_LLM_CALL_TOTAL_BUDGET_MS: u64 = 5_000;

fn parse_llm_call_total_budget_ms(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .map(|secs| secs.saturating_mul(1000))
        .filter(|ms| *ms >= MIN_LLM_CALL_TOTAL_BUDGET_MS)
        .unwrap_or(DEFAULT_LLM_CALL_TOTAL_BUDGET_MS)
}

fn llm_call_total_budget_ms() -> u64 {
    parse_llm_call_total_budget_ms(std::env::var("FREE_API_LLM_CALL_BUDGET_SECONDS").ok().as_deref())
}

pub(crate) fn transient_retry_backoff_ms(attempt_index: u32) -> u64 {
    let exponent = attempt_index.min(4);
    (1_000u64 * (1u64 << exponent)).min(MAX_TRANSIENT_RETRY_BACKOFF_MS)
}

/// 限流类失败（429/rpm/quota）的专用退避：配额窗口按分钟计，快速重试只会
/// 连续撞墙并把重试预算烧光（实测教训：deepseek 实弹 429 后 1-16s 退避连续
/// 重试 20 次全部 429，240s 预算耗尽整个评测被带崩）。固定退避 60s 让重试
/// 落在下一个配额窗口。
const RATE_LIMIT_RETRY_BACKOFF_MS: u64 = 60_000;

fn is_rate_limit_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("429") || lower.contains("too many requests") || lower.contains("rpm exhausted") || lower.contains("quota")
}

/// 带自动重试与多 Key 故障转移的流式请求。
/// 当遇到 401/402/429、余额不足或限流时，自动切换至该平台下的下一个可用 Key 重试；
/// 全部尝试与退避共享单次调用总预算：预算耗尽立即放弃，避免端点挂起拖垮整个运行。
/// 前后挂槽位健康熔断：连续失败快速失败（llm_breaker），用户中断不计入失败。
pub async fn stream_with_retry<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    max_retries: u32,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    stream_with_attempt_budget(
        config, messages, reasoning_effort, interrupt_rx,
        AttemptBudget::KeyMinimum(max_retries), on_event,
    ).await
}

/// 终审等已有明确预算的调用方使用精确上限，不再被 Key 数量或外层重试乘倍。
/// 与普通调用共用原有 Key 轮换、退避、取消、总时限和熔断记账。
pub(crate) async fn stream_with_attempt_limit<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    max_attempts: u32,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    stream_with_attempt_budget(
        config, messages, reasoning_effort, interrupt_rx,
        AttemptBudget::Exact(max_attempts), on_event,
    ).await
}

#[derive(Clone, Copy)]
enum AttemptBudget {
    KeyMinimum(u32),
    Exact(u32),
}

impl AttemptBudget {
    fn attempts(self, key_count: usize) -> usize {
        match self {
            Self::KeyMinimum(n) => (n as usize).max(key_count * 2),
            Self::Exact(n) => n.max(1) as usize,
        }
    }
}

async fn stream_with_attempt_budget<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    attempt_budget: AttemptBudget,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let slot = crate::llm_breaker::slot_key(&config.platform_name, &config.model_id);
    crate::llm_breaker::gate(&slot)?;

    let budget_ms = llm_call_total_budget_ms();
    let started = std::time::Instant::now();
    let budget_exhausted = || {
        format!(
            "单次调用总预算 {}s 耗尽（含重试与Key轮换），已放弃本次调用",
            budget_ms / 1000
        )
    };

    let mut available_keys = config.api_keys();
    if available_keys.is_empty() {
        let raw = config.api_key.trim();
        if !raw.is_empty() && !raw.starts_with('[') {
            available_keys.push(raw.to_string());
        }
    }
    if available_keys.is_empty() {
        return Err(format!(
            "平台 [{}] 当前没有可用或处于启用状态的 API Key（所有 Key 均被临时禁用，请在【LLM 管理】中开启至少一个 Key）",
            config.platform_name
        ));
    }
    let total_keys = available_keys.len();

    let outcome: Result<StreamResult, String> = async {
        let mut last_error = String::new();
        let start_key_idx = if total_keys > 1 {
            GLOBAL_KEY_REQUEST_ROTATOR.fetch_add(1, Ordering::Relaxed) % total_keys
        } else {
            0
        };
        let mut current_key_idx = start_key_idx;
        let mut key_switch_count = 0usize;

        // 普通调用保留既有 Key 最小机会；终审精确预算则是硬上限。
        let total_attempts_allowed = attempt_budget.attempts(total_keys);
        let mut attempts_made = 0usize;

        for attempt_i in 0..total_attempts_allowed {
            let remaining_ms =
                budget_ms.saturating_sub(started.elapsed().as_millis() as u64);
            if remaining_ms == 0 {
                break;
            }

            let current_key = &available_keys[current_key_idx];
            attempts_made += 1;
            let attempt = tokio::time::timeout(
                tokio::time::Duration::from_millis(remaining_ms),
                stream_once_with_key(config, current_key, messages, reasoning_effort, interrupt_rx, on_event),
            )
            .await;

            match attempt {
                Ok(Ok(mut result)) => {
                    result.attempt_count = (attempt_i + 1) as u32;
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    if e == STREAM_INTERRUPTED_ERROR {
                        return Err(e);
                    }

                    last_error = e.clone();

                    // 🛑 快速失败: 400 Bad Request 明确属于客户端参数/格式硬错误，自愈重试未解决说明无法恢复，终止盲目重试
                    if e.contains("API 错误 400") || e.contains("400 Bad Request") {
                        eprintln!(
                            "🛑 [客户端请求错误] API 拒绝了请求格式 (400 Bad Request)，停止无效重试: {}",
                            e
                        );
                        return Err(e);
                    }

                    // 检查是否为限流/欠费/鉴权失效等可换 Key 错误
                    let rotatable = is_key_rotatable_error(None, &e);

                    if rotatable
                        && total_keys > 1
                        && key_switch_count < total_keys - 1
                        && attempt_i + 1 < total_attempts_allowed
                    {
                        let failed_key = current_key.clone();
                        current_key_idx = (current_key_idx + 1) % total_keys;
                        key_switch_count += 1;
                        println!(
                            "🔄 [多Key切换] 平台 [{}] 的 API Key [{}] 返回错误 ({})，自动切换至第 {}/{} 个备用 Key [{}] 重试模型 [{}]...",
                            config.platform_name,
                            mask_api_key(&failed_key),
                            e,
                            current_key_idx + 1,
                            total_keys,
                            mask_api_key(&available_keys[current_key_idx]),
                            config.model_name
                        );
                        // 如果是 503 等服务端高并发超载错误，切换 Key 时短暂缓冲 600ms，避免毫秒级瞬发连击被 Google 机房全数拒绝
                        if e.contains("503")
                            || e.contains("high demand")
                            || e.contains("spikes in demand")
                            || e.contains("UNAVAILABLE")
                            || e.contains("Service Unavailable")
                        {
                            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
                        }
                        continue;
                    }

                    println!(
                        "⚠️ LLM 请求失败 (第 {} 次 / 共 {} 次, 当前Key: {}): {}",
                        attempt_i + 1,
                        total_attempts_allowed,
                        mask_api_key(current_key),
                        last_error
                    );

                    if attempt_i < total_attempts_allowed - 1 {
                        let backoff_ms = if is_rate_limit_error(&last_error) {
                            RATE_LIMIT_RETRY_BACKOFF_MS
                        } else {
                            transient_retry_backoff_ms(attempt_i as u32)
                        };
                        if started.elapsed().as_millis() as u64 + backoff_ms >= budget_ms {
                            last_error = budget_exhausted();
                            break;
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {
                                // 退避休眠结束后，如果所有 Key 都尝试过一轮了，
                                // 重置切换计数，让下一轮重试可以继续轮询所有备用 Key
                                if key_switch_count >= total_keys.saturating_sub(1) {
                                    key_switch_count = 0;
                                    current_key_idx = (current_key_idx + 1) % total_keys;
                                }
                            },
                            _changed = interrupt_rx.changed() => {
                                // sender 已销毁＝再无人能中断：pending() 会把重试
                                // 退避永久挂死（实弹已见会话僵尸化），按中断返回。
                                return Err(STREAM_INTERRUPTED_ERROR.to_string());
                            }
                        }
                    }
                }
                Err(_elapsed) => {
                    if interrupt_rx.has_changed().unwrap_or(false) {
                        return Err(STREAM_INTERRUPTED_ERROR.to_string());
                    }
                    last_error = budget_exhausted();
                    break;
                }
            }
        }

        if last_error.is_empty() {
            last_error = budget_exhausted();
        }

        // 报实际发起的尝试，不把配置值或已轮换 Key 数冒充执行次数。
        Err(format!(
            "LLM 请求最终失败 (实际尝试 {} 次，平台 [{}]): {}",
            attempts_made, config.platform_name, last_error
        ))
    }
    .await;

    match &outcome {
        Ok(_) => crate::llm_breaker::record(&slot, true),
        Err(e) if e != STREAM_INTERRUPTED_ERROR => crate::llm_breaker::record(&slot, false),
        _ => {}
    }
    outcome
}

/// 韧性重试参数：硬失败后等待端点恢复窗口再重试当前逻辑调用。
/// 等待 = max(熔断剩余冷却, 最小驻留)，封顶上限；期间响应取消信号。
/// 整体仍受任务级看门狗（如评测 60 分钟）约束，不会造成无界挂起。
const RESILIENT_LLM_RETRIES: u32 = 2;
const RESILIENT_MIN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const RESILIENT_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// 带槽位韧性的流式请求：stream_with_retry 之上再包一层——
/// 传输层彻底失败（含预算耗尽）时，不再把死亡直接交给上层，
/// 而是等端点恢复窗口（熔断冷却/最小驻留）后重试当前逻辑调用，最多 RESILIENT_LLM_RETRIES 次。
/// 实测场景：端点突发故障恰好打在任务尾部（发 finish 前），一次硬失败曾直接判死整个任务。
pub async fn stream_with_resilient_retry<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    transient_request_retries: u32,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let slot = crate::llm_breaker::slot_key(&config.platform_name, &config.model_id);
    let mut result = stream_with_retry(
        config,
        messages,
        reasoning_effort,
        interrupt_rx,
        transient_request_retries,
        on_event,
    )
    .await;
    let mut attempt = 0u32;
    while let Err(err) = &result {
        // 用户取消不是端点问题：立即上抛
        if err == STREAM_INTERRUPTED_ERROR {
            break;
        }
        // 两类可等待恢复的硬失败：传输层彻底失败（含预算耗尽），以及熔断快速失败
        // （熔断注册表是进程级全局——上一任务打开的闸门不能让下一任务秒死，应等半开窗口）。
        let recoverable = err.contains("LLM 请求最终失败")
            || err.contains("熔断中")
            || err.contains("半开探测");
        if !recoverable {
            break;
        }
        if attempt >= RESILIENT_LLM_RETRIES {
            break;
        }
        attempt += 1;
        let wait = crate::llm_breaker::remaining_cooldown(&slot)
            .unwrap_or_default()
            .max(RESILIENT_MIN_WAIT)
            .min(RESILIENT_MAX_WAIT);
        println!(
            "⏳ [端点韧性] {} 传输层硬失败，等待 {}s 后重试当前步骤模型调用（{}/{}）",
            config.model_name,
            wait.as_secs(),
            attempt,
            RESILIENT_LLM_RETRIES
        );
        let cancelled = tokio::select! {
            _ = tokio::time::sleep(wait) => false,
            _changed = interrupt_rx.changed() => {
                // sender 已销毁＝中断通道失效：按已中断处理，不进入永久挂起。
                true
            }
        };
        if cancelled {
            return Err(STREAM_INTERRUPTED_ERROR.to_string());
        }
        result = stream_with_retry(
            config,
            messages,
            reasoning_effort,
            interrupt_rx,
            transient_request_retries,
            on_event,
        )
        .await;
    }
    result
}


#[cfg(test)]
mod exact_attempt_tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};

    // Exercise the real HTTP/SSE path without calling a paid provider or touching the DB.
    struct MockEndpoint {
        config: ModelConfig,
        requests: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockEndpoint {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl MockEndpoint {
        async fn start(success_requests: Vec<usize>, failure_status: u16) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let request_counter = requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 2048];
                    // Consume the complete request before replying, not just its headers.
                    loop {
                        let n = socket.read(&mut buffer).await.unwrap();
                        if n == 0 { break; }
                        request.extend_from_slice(&buffer[..n]);
                        if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&request[..header_end]);
                            let content_length = headers.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            }).unwrap_or(0);
                            if request.len() >= header_end + 4 + content_length { break; }
                        }
                        assert!(request.len() < 131_072, "unexpected mock request size");
                    }
                    let index = request_counter.fetch_add(1, Ordering::SeqCst) + 1;
                    let (status, content_type, body) = if success_requests.contains(&index) {
                        let chunk = json!({"choices": [{"delta": {"content": "{\"verdict\":\"approve\"}"}}]});
                        ("200 OK".to_owned(), "text/event-stream", format!("data: {}\n\ndata: [DONE]\n\n", chunk))
                    } else {
                        let body = json!({"error": {"message": if failure_status == 403 {
                            "upstream 403: WAF Block Page; retry after the block window"
                        } else {
                            "invalid request payload"
                        }}}).to_string();
                        (format!("{} {}", failure_status, if failure_status == 403 { "Forbidden" } else { "Bad Request" }), "application/json", body)
                    };
                    let response = format!(
                        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status, content_type, body.len(), body,
                    );
                    // Cancellation can legitimately close a connection before the reply.
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                }
            });
            Self {
                config: ModelConfig {
                    model_id: uuid::Uuid::new_v4().to_string(),
                    platform_name: "retry-regression".into(),
                    base_url: format!("http://{}/v1", address),
                    api_key: "test-only-key".into(),
                    model_name: "test-model".into(),
                    max_tokens: 512,
                    temperature: 0.7,
                    is_ollama: false,
                    is_lmstudio: false,
                    context_window: 128_000,
                    provider_capabilities: "{}".into(),
                    timeout_secs: 10,
                    requires_alternating_roles: 0,
                },
                requests,
                server,
            }
        }

        async fn call(&self, interrupt: &mut watch::Receiver<u64>, limit: u32) -> Result<StreamResult, String> {
            let messages = vec![json!({"role": "user", "content": "review"})];
            let mut on_event = |_: StreamEvent<'_>| true;
            timeout(Duration::from_secs(60), stream_with_attempt_limit(
                &self.config, &messages, "auto", interrupt, limit, &mut on_event,
            )).await.expect("retry test exceeded its watchdog")
        }

        async fn wait_for_first_request(&self) {
            timeout(Duration::from_secs(5), async {
                while self.requests.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.expect("mock did not receive a request");
        }
    }

    #[test]
    fn exact_budget_does_not_change_legacy_key_minimum() {
        assert_eq!(AttemptBudget::KeyMinimum(1).attempts(4), 8);
        assert_eq!(AttemptBudget::Exact(6).attempts(4), 6);
        assert_eq!(AttemptBudget::Exact(0).attempts(4), 1);
    }

    #[tokio::test]
    async fn six_attempt_limit_survives_403_and_multiple_keys() {
        let mut endpoint = MockEndpoint::start(vec![], 403).await;
        endpoint.config.api_key = json!(["test-a", "test-b", "test-c", "test-d"]).to_string();
        let (_sender, mut interrupt) = watch::channel(0u64);
        let error = endpoint.call(&mut interrupt, 6).await.err().unwrap();
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 6);
        assert!(error.contains("实际尝试 6 次"), "{}", error);
        assert!(error.contains("403 Forbidden") && error.contains("WAF Block Page"), "{}", error);
    }

    #[tokio::test]
    async fn success_resets_the_next_requests_attempt_budget() {
        let endpoint = MockEndpoint::start(vec![2, 8], 403).await;
        let (_sender, mut interrupt) = watch::channel(0u64);
        let first = endpoint.call(&mut interrupt, 6).await.unwrap();
        assert_eq!(first.attempt_count, 2);
        let second = endpoint.call(&mut interrupt, 6).await.unwrap();
        assert_eq!(second.attempt_count, 6);
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 8);
        assert_eq!(second.full_response, "{\"verdict\":\"approve\"}");
    }

    #[tokio::test]
    async fn disabled_retry_is_one_attempt_even_with_multiple_keys() {
        let mut endpoint = MockEndpoint::start(vec![], 403).await;
        endpoint.config.api_key = json!(["test-a", "test-b", "test-c", "test-d"]).to_string();
        let (_sender, mut interrupt) = watch::channel(0u64);
        let error = endpoint.call(&mut interrupt, 1).await.err().unwrap();
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 1);
        assert!(error.contains("实际尝试 1 次"), "{}", error);
    }

    #[tokio::test]
    async fn cancellation_stops_retry_wait_without_another_request() {
        let endpoint = MockEndpoint::start(vec![], 403).await;
        let (sender, mut interrupt) = watch::channel(0u64);
        let (result, ()) = tokio::join!(endpoint.call(&mut interrupt, 6), async {
            endpoint.wait_for_first_request().await;
            sender.send(1).unwrap();
        });
        assert_eq!(result.err().unwrap(), STREAM_INTERRUPTED_ERROR);
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_interrupt_sender_does_not_hang_retry_wait() {
        let endpoint = MockEndpoint::start(vec![], 403).await;
        let (sender, mut interrupt) = watch::channel(0u64);
        let (result, ()) = tokio::join!(endpoint.call(&mut interrupt, 6), async {
            endpoint.wait_for_first_request().await;
            drop(sender);
        });
        assert_eq!(result.err().unwrap(), STREAM_INTERRUPTED_ERROR);
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalid_400_payload_keeps_existing_fast_failure_policy() {
        let endpoint = MockEndpoint::start(vec![], 400).await;
        let (_sender, mut interrupt) = watch::channel(0u64);
        let error = endpoint.call(&mut interrupt, 6).await.err().unwrap();
        assert_eq!(endpoint.requests.load(Ordering::SeqCst), 1);
        assert!(error.contains("API 错误 400"), "{}", error);
    }
}
