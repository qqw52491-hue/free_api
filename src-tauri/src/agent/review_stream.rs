//! 审查请求的活动超时：正文/思考刷新静默时限；所有尝试仍受既有调用总预算约束。
use crate::llm::{self, ModelConfig, StreamEvent, StreamResult};
use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

const CAPTURE_CHARS: usize = 4_000;

#[derive(Clone, Copy)]
struct Budget {
    idle: Duration,
    total: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeoutKind {
    FirstOutput,
    Idle,
    Total,
}

struct Activity {
    started: Instant,
    last: Instant,
    first: Option<Instant>,
    content_chars: usize,
    thinking_chars: usize,
    content_prefix: String,
    thinking_prefix: String,
}

impl Activity {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            first: None,
            content_chars: 0,
            thinking_chars: 0,
            content_prefix: String::new(),
            thinking_prefix: String::new(),
        }
    }

    fn observe(&mut self, event: &StreamEvent<'_>) {
        let (chunk, prefix, count) = match event {
            StreamEvent::Content { chunk, .. } => {
                (chunk, &mut self.content_prefix, &mut self.content_chars)
            }
            StreamEvent::Thinking { chunk, .. } => {
                (chunk, &mut self.thinking_prefix, &mut self.thinking_chars)
            }
        };
        if chunk.is_empty() {
            return;
        }
        let remaining = CAPTURE_CHARS.saturating_sub(*count);
        prefix.extend(chunk.chars().take(remaining));
        *count = count.saturating_add(chunk.chars().count());
        self.last = Instant::now();
        self.first.get_or_insert(self.last);
    }

    fn failure(
        &self,
        kind: Option<TimeoutKind>,
        error: String,
        budget: Budget,
    ) -> ReviewStreamFailure {
        ReviewStreamFailure {
            kind,
            error,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            idle_ms: self.last.elapsed().as_millis() as u64,
            first_output_ms: self
                .first
                .map(|t| t.duration_since(self.started).as_millis() as u64),
            content_chars: self.content_chars,
            thinking_chars: self.thinking_chars,
            content_prefix: self.content_prefix.clone(),
            thinking_prefix: self.thinking_prefix.clone(),
            idle_limit_ms: budget.idle.as_millis() as u64,
            total_limit_ms: budget.total.as_millis() as u64,
        }
    }
}

pub(crate) struct ReviewStreamFailure {
    kind: Option<TimeoutKind>,
    pub(crate) error: String,
    elapsed_ms: u64,
    idle_ms: u64,
    first_output_ms: Option<u64>,
    content_chars: usize,
    thinking_chars: usize,
    content_prefix: String,
    thinking_prefix: String,
    idle_limit_ms: u64,
    total_limit_ms: u64,
}

impl ReviewStreamFailure {
    pub(crate) fn is_timeout(&self) -> bool {
        self.kind.is_some()
    }

    /// 有持续输出却耗尽总预算时不重发同一请求；首输出/静默超时仍沿用调用方的重试次数。
    pub(crate) fn may_retry_timeout(&self) -> bool {
        matches!(
            self.kind,
            Some(TimeoutKind::FirstOutput | TimeoutKind::Idle)
        )
    }

    pub(crate) fn description(&self) -> String {
        let reason = match self.kind {
            Some(TimeoutKind::FirstOutput) => "等待首个正文或思考片段超时",
            Some(TimeoutKind::Idle) => "正文或思考片段长时间未更新",
            Some(TimeoutKind::Total) => "本次模型调用总预算耗尽（含传输重试）",
            None => &self.error,
        };
        format!("{}；本次已等待 {:.1} 秒，连续 {:.1} 秒未收到新片段；已收到正文 {} 字、思考 {} 字。接收片段仅供诊断，不构成完整审查结论",
            reason, self.elapsed_ms as f64 / 1000.0, self.idle_ms as f64 / 1000.0,
            self.content_chars, self.thinking_chars)
    }

    pub(crate) fn log(
        &self,
        session_id: &str,
        suffix: &str,
        call_index: usize,
        config: &ModelConfig,
        messages: &[Value],
    ) {
        // 不制造 StreamResult/usage，也不把部分 JSON 当成有效判决。
        let diagnostic = format!(
            "{}；elapsed_ms={}，idle_ms={}，first_output_ms={:?}，idle_limit_ms={}，total_limit_ms={}；片段为本次逻辑调用所观察到的前 {} 字符（可跨传输尝试），truncated={}",
            self.description(),
            self.elapsed_ms, self.idle_ms, self.first_output_ms, self.idle_limit_ms, self.total_limit_ms,
            CAPTURE_CHARS,
            self.content_chars > CAPTURE_CHARS || self.thinking_chars > CAPTURE_CHARS
        );
        crate::agent::utils::llm_client::log_llm_exchange_to_file(
            &format!("{}__{}", session_id, suffix),
            call_index,
            0,
            config,
            messages,
            &self.thinking_prefix,
            &self.content_prefix,
            None,
            "request_error",
            Some(&diagnostic),
            None,
        );
    }
}

async fn wait_for_stream<F>(
    call: F,
    activity: Arc<Mutex<Activity>>,
    budget: Budget,
) -> Result<StreamResult, ReviewStreamFailure>
where
    F: Future<Output = Result<StreamResult, String>>,
{
    let started = activity.lock().unwrap().started;
    let idle = tokio::time::sleep_until(started + budget.idle);
    let total = tokio::time::sleep_until(started + budget.total);
    tokio::pin!(call, idle, total);
    loop {
        tokio::select! {
            biased;
            result = &mut call => return result.map_err(|error| {
                let state = activity.lock().unwrap();
                let kind = (error != llm::STREAM_INTERRUPTED_ERROR && state.started.elapsed() >= budget.total)
                    .then_some(TimeoutKind::Total);
                state.failure(kind, error, budget)
            }),
            _ = &mut total => return Err(activity.lock().unwrap().failure(Some(TimeoutKind::Total), String::new(), budget)),
            _ = &mut idle => {
                let state = activity.lock().unwrap();
                let next = state.last + budget.idle;
                if Instant::now() < next {
                    idle.as_mut().reset(next);
                } else {
                    let kind = if state.first.is_some() { TimeoutKind::Idle } else { TimeoutKind::FirstOutput };
                    return Err(state.failure(Some(kind), String::new(), budget));
                }
            }
        }
    }
}

/// None 保留子审查既有的 Key 最小尝试数；Some(n) 保留终审既有的精确尝试上限。
pub(crate) async fn stream_review<F: FnMut(StreamEvent<'_>) -> bool>(
    config: &ModelConfig,
    messages: &[Value],
    effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    attempt_limit: Option<u32>,
    idle_secs: u64,
    on_event: &mut F,
) -> Result<StreamResult, ReviewStreamFailure> {
    let budget = Budget {
        idle: Duration::from_secs(idle_secs),
        total: Duration::from_millis(llm::llm_call_total_budget_ms()),
    };
    // 仅审查调用副本：底层 HTTP 不再用 120s 固定时长切断仍在输出的流。
    // 连接超时仍由共享客户端保持 15s，整次调用仍受现有总预算限制。
    let mut config = config.clone();
    config.timeout_secs = budget.total.as_secs().max(1);
    let activity = Arc::new(Mutex::new(Activity::new()));
    let observed = activity.clone();
    let mut observer = |event: StreamEvent<'_>| {
        observed.lock().unwrap().observe(&event);
        on_event(event)
    };
    let call = async {
        match attempt_limit {
            Some(limit) => {
                llm::stream_with_attempt_limit(
                    &config,
                    messages,
                    effort,
                    interrupt_rx,
                    limit,
                    &mut observer,
                )
                .await
            }
            None => {
                llm::stream_with_retry(&config, messages, effort, interrupt_rx, 1, &mut observer)
                    .await
            }
        }
    };
    wait_for_stream(call, activity, budget).await
}

#[cfg(test)]
mod tests;
