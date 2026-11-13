//! 终审只上报宿主可观察的阶段/计时，不转发模型思考或原始响应，不改变请求预算。
use crate::llm::StreamEvent;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
use tauri::{AppHandle, Emitter, Runtime};

#[derive(Default)]
struct ProgressState {
    phase: String,
    stream_kind: String,
    message: String,
    round: usize,
    tool_steps: u32,
    tool_budget: u32,
    last_notice: String,
    failure_kind: String,
    timeout_retries: u32,
    format_corrections: u32,
    last_request_round: Option<usize>,
    last_request_try: Option<u64>,
    last_request_output_tokens: Option<i64>,
    last_request_ttft_ms: Option<u64>,
    last_request_tokens_per_second: Option<f64>,
}

pub(crate) struct ReviewProgress {
    sink: Box<dyn Fn(Value) + Send + Sync>,
    id: String,
    member_key: String,
    label: String,
    model: String,
    started_at_ms: i64,
    started: std::time::Instant,
    state: Mutex<ProgressState>,
    received_chars: AtomicUsize,
    terminal: AtomicBool,
}

impl ReviewProgress {
    pub(crate) fn id(&self) -> &str { &self.id }
    pub(super) fn new<R: Runtime>(
        app: &AppHandle<R>,
        session: &str,
        member: &str,
        label: &str,
        model: &str,
        parent_step_id: usize,
    ) -> Self {
        let app = app.clone();
        let channel = format!("agent-progress-{session}");
        let parent_run = crate::agent::child_reviewer::main_executor::parent_run();
        Self::with_sink(member, label, model, move |mut value| {
            if let (Some(run_id), Some(payload)) = (&parent_run, value.as_object_mut()) {
                payload.insert("run_id".into(), json!(run_id));
            }
            if let Some(payload) = value.as_object_mut() {
                payload.insert("parent_step_id".into(), json!(parent_step_id));
            }
            let _ = app.emit(&channel, value);
        })
    }

    pub(crate) fn with_sink(
        member: &str,
        label: &str,
        model: &str,
        sink: impl Fn(Value) + Send + Sync + 'static,
    ) -> Self {
        Self {
            sink: Box::new(sink),
            id: uuid::Uuid::new_v4().to_string(),
            member_key: member.into(),
            label: label.into(),
            model: model.into(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            started: std::time::Instant::now(),
            state: Mutex::new(ProgressState::default()),
            received_chars: AtomicUsize::new(0),
            terminal: AtomicBool::new(false),
        }
    }

    fn set_phase_inner(&self, phase: &str, message: &str, force: bool) {
        if !force && self.terminal.load(Ordering::Acquire) {
            return;
        }
        {
            let mut s = self.state.lock().unwrap();
            s.phase = phase.into();
            s.message = message.into();
            if matches!(phase, "retrying" | "format_retry") {
                s.last_notice = message.into();
            }
            if phase == "retrying" { s.timeout_retries += 1; }
            if phase == "format_retry" { s.format_corrections += 1; }
        }
        self.emit_payload();
    }

    pub(crate) fn phase(&self, phase: &str, message: &str) {
        self.set_phase_inner(phase, message, false);
    }

    pub(crate) fn request(&self, round: usize, steps: u32, budget: u32) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        self.received_chars.store(0, Ordering::Relaxed);
        {
            let mut s = self.state.lock().unwrap();
            s.stream_kind.clear();
            s.round = round;
            s.tool_steps = steps;
            s.tool_budget = budget;
        }
        self.phase("waiting", "请求已发起，等待模型响应（尚未收到片段）");
    }

    pub(crate) fn reset_stream_activity(&self) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        self.received_chars.store(0, Ordering::Relaxed);
        self.state.lock().unwrap().stream_kind.clear();
    }

    // A completed request sample, not a live estimate or an aggregate across tool rounds.
    // The transport's rate can be character-estimated without usage, so compute only
    // from provider-reported output tokens and a meaningful observed generation span.
    pub(crate) fn record_request_try(&self, attempt: Option<u64>) {
        if !self.terminal.load(Ordering::Acquire) {
            self.state.lock().unwrap().last_request_try = attempt;
        }
    }

    pub(crate) fn record_request_metrics(
        &self,
        round_hint: Option<usize>,
        output_tokens: Option<i64>,
        ttft_ms: u64,
        total_duration_ms: u64,
    ) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.round == 0 {
                if let Some(round) = round_hint {
                    s.round = round;
                }
            }
            s.last_request_round = Some(s.round);
            s.last_request_output_tokens = output_tokens.filter(|tokens| *tokens >= 0);
            s.last_request_ttft_ms = (self.received_chars.load(Ordering::Relaxed) > 0).then_some(ttft_ms);
            let generation_ms = total_duration_ms.saturating_sub(ttft_ms);
            s.last_request_tokens_per_second = s.last_request_output_tokens
                .filter(|tokens| *tokens > 0 && generation_ms > 50 && s.last_request_ttft_ms.is_some())
                .map(|tokens| tokens as f64 * 1000.0 / generation_ms as f64);
        }
        self.emit();
    }

    pub(super) fn response_completed(&self, output: &crate::llm::StreamResult) {
        self.record_request_metrics(
            None,
            output.usage.map(|u| u.completion_tokens),
            output.ttft_ms,
            output.total_duration_ms,
        );
    }

    pub(crate) fn observe_stream_activity(&self, kind: &str, count: usize) -> bool {
        if self.terminal.load(Ordering::Acquire) {
            return true;
        }
        if count > 0 {
            let first = self.received_chars.fetch_add(count, Ordering::Relaxed) == 0;
            let changed = {
                let mut s = self.state.lock().unwrap();
                let changed = s.stream_kind != kind;
                s.stream_kind = kind.into();
                changed
            };
            // 只发阶段切换与既有心跳，不逐 token 广播，更不转发思考正文。
            if first || changed {
                self.phase("responding", if kind == "thinking" {
                    "正在思考（已收到思考片段，尚未形成审查结论）"
                } else {
                    "正在生成结论（尚未形成有效审查结论）"
                });
            }
        }
        true
    }

    pub(super) fn observe(&self, event: StreamEvent<'_>) -> bool {
        let (kind, chunk) = match event {
            StreamEvent::Thinking { chunk, .. } => ("thinking", chunk),
            StreamEvent::Content { chunk, .. } => ("content", chunk),
        };
        self.observe_stream_activity(kind, chunk.chars().count())
    }

    // 定时器与被观察 Future 同生共死：停止/返回后没有后台心跳，也不重复发起 HTTP。
    pub(super) async fn wait<F: std::future::Future>(&self, future: F) -> F::Output {
        tokio::pin!(future);
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
        );
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                result = &mut future => return result,
                _ = tick.tick() => self.emit(),
            }
        }
    }

    // 固定枚举由宿主分支给出；不按错误文案猜测，不广播上游原始错误。
    pub(crate) fn fail(&self, kind: &str, message: &str) {
        self.terminal.store(true, Ordering::SeqCst);
        self.state.lock().unwrap().failure_kind = kind.into();
        self.set_phase_inner(if kind == "cancelled" { "cancelled" } else { "failed" }, message, true);
    }

    pub(crate) fn tool_completed(&self, steps: u32) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        self.state.lock().unwrap().tool_steps = steps;
        self.emit();
    }

    pub(crate) fn finish(&self, verdict: &str) {
        self.terminal.store(true, Ordering::SeqCst);
        let (phase, message) = match verdict {
            "passed" => ("passed", "终审通过（不代表整个任务已经完成交付）"),
            "rejected" => ("rejected", "终审提出阻塞问题，等待主执行者处理"),
            "skipped" => ("skipped", "本轮未执行模型终审"),
            _ => ("failed", "未获得有效审查结论，请检查调用诊断"),
        };
        self.set_phase_inner(phase, message, true);
    }

    fn emit(&self) {
        if self.terminal.load(Ordering::Acquire) {
            return;
        }
        self.emit_payload();
    }

    fn emit_payload(&self) {
        let s = self.state.lock().unwrap();
        let payload = json!({
            "type": "finish_review_progress", "review_id": self.id, "member_key": self.member_key,
            "role_label": self.label, "model": self.model, "started_at_ms": self.started_at_ms,
            "phase": s.phase, "description": s.message, "round": s.round, "tool_steps": s.tool_steps, "tool_budget": s.tool_budget,
            "last_notice": s.last_notice, "failure_kind": s.failure_kind, "stream_kind": s.stream_kind,
            "timeout_retries": s.timeout_retries, "format_corrections": s.format_corrections,
            "elapsed_ms": self.started.elapsed().as_millis() as u64,
            "received_chars": self.received_chars.load(Ordering::Relaxed),
            "last_request_round": s.last_request_round,
            "last_request_try": s.last_request_try,
            "last_request_output_tokens": s.last_request_output_tokens,
            "last_request_ttft_ms": s.last_request_ttft_ms,
            "last_request_tokens_per_second": s.last_request_tokens_per_second,
        });
        drop(s);
        (self.sink)(payload);
    }
}

impl Drop for ReviewProgress {
    fn drop(&mut self) {
        if !self.terminal.load(Ordering::Acquire) {
            self.set_phase_inner("incomplete", "审查已停止或调用结束，未获得有效结论", true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fixture() -> (ReviewProgress, Arc<Mutex<Vec<Value>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let out = events.clone();
        (
            ReviewProgress::with_sink(
                "finish-review-member-2",
                "安全审查",
                "test-model",
                move |v| out.lock().unwrap().push(v),
            ),
            events,
        )
    }

    #[test]
    fn progress_reports_identity_and_counts_but_never_stream_text() {
        let (p, events) = fixture();
        p.request(2, 1, 8);
        p.observe(StreamEvent::Thinking {
            chunk: "PRIVATE_THOUGHT",
            accumulated: "PRIVATE_THOUGHT",
        });
        p.phase("tool", "正在取证：read_file");
        p.finish("passed");
        drop(p);
        let e = events.lock().unwrap();
        assert_eq!(e[0]["member_key"], "finish-review-member-2");
        assert_eq!(e[0]["round"], 2);
        assert_eq!(e[0]["tool_steps"], 1);
        assert_eq!(e[1]["phase"], "responding");
        assert_eq!(e[1]["received_chars"], 15);
        assert_eq!(e.last().unwrap()["phase"], "passed");
        assert!(!serde_json::to_string(&*e)
            .unwrap()
            .contains("PRIVATE_THOUGHT"));
    }

    #[test]
    fn request_speed_uses_reported_usage_not_character_estimates() {
        let (p, events) = fixture();
        p.request(1, 0, 8);
        p.observe(StreamEvent::Content { chunk: "private", accumulated_content: "private", full_thinking: "" });
        let mut output = crate::llm::StreamResult {
            full_response: "private".into(), thinking_text: String::new(), finish_reason: None,
            usage: Some(crate::llm::RawUsage { completion_tokens: 100, ..Default::default() }),
            ttft_ms: 3000, total_duration_ms: 5000, tokens_per_second: 999.0, attempt_count: 1,
        };
        p.response_completed(&output);
        let sample = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(sample["last_request_round"], 1);
        assert_eq!(sample["last_request_tokens_per_second"], 50.0);
        assert_eq!(sample["last_request_output_tokens"], 100);
        assert_eq!(sample["last_request_ttft_ms"], 3000);
        p.request(2, 0, 8);
        assert_eq!(events.lock().unwrap().last().unwrap()["last_request_round"], 1);
        output.usage = None; // Transport may still carry an estimated nonzero rate.
        p.response_completed(&output);
        let missing = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(missing["last_request_round"], 2);
        assert!(missing["last_request_tokens_per_second"].is_null());
        assert!(missing["last_request_output_tokens"].is_null());
        assert!(missing["last_request_ttft_ms"].is_null(), "no observed fragment in this request");
        output.usage = Some(crate::llm::RawUsage { completion_tokens: 100, ..Default::default() });
        output.total_duration_ms = output.ttft_ms + 20;
        p.response_completed(&output);
        assert!(events.lock().unwrap().last().unwrap()["last_request_tokens_per_second"].is_null());
        p.finish("passed");
        assert!(!serde_json::to_string(&*events.lock().unwrap()).unwrap().contains("private"));
    }

    #[test]
    fn stream_kind_is_observed_not_guessed_and_resets_per_request() {
        let (p, events) = fixture();
        p.request(1, 0, 8);
        p.observe(StreamEvent::Thinking { chunk: "secret", accumulated: "secret" });
        p.observe(StreamEvent::Thinking { chunk: "more", accumulated: "secretmore" });
        assert_eq!(events.lock().unwrap().len(), 2);
        p.observe(StreamEvent::Content { chunk: "answer", accumulated_content: "answer", full_thinking: "secretmore" });
        assert_eq!(events.lock().unwrap().last().unwrap()["stream_kind"], "content");
        p.request(2, 0, 8);
        assert_eq!(events.lock().unwrap().last().unwrap()["stream_kind"], "");
        p.finish("passed");
        assert!(!serde_json::to_string(&*events.lock().unwrap()).unwrap().contains("secret"));
    }

    #[test]
    fn retry_notice_survives_next_request_and_failure_is_terminal() {
        let (p, events) = fixture();
        p.request(1, 0, 8);
        p.phase("retrying", "等待超时，正在重试");
        p.request(2, 0, 8);
        p.fail("request", "审查请求失败，未获得有效结论");
        drop(p);
        let e = events.lock().unwrap();
        assert_eq!(e[2]["last_notice"], "等待超时，正在重试");
        assert_eq!(e.last().unwrap()["phase"], "failed");
        assert_eq!(e.last().unwrap()["failure_kind"], "request");
        assert_eq!(e.last().unwrap()["timeout_retries"], 1);
    }

    #[test]
    fn terminal_diagnostics_keep_counters_and_distinguish_cancellation() {
        for kind in ["timeout", "request", "format", "configuration", "cancelled"] {
            let (p, events) = fixture();
            p.request(1, 0, 8);
            p.phase("format_retry", "纠正格式");
            p.request(2, 0, 8);
            p.tool_completed(1);
            p.fail(kind, "固定诊断说明");
            drop(p);
            let e = events.lock().unwrap();
            let last = e.last().unwrap();
            assert_eq!(last["phase"], if kind == "cancelled" { "cancelled" } else { "failed" });
            assert_eq!(last["failure_kind"], kind);
            assert_eq!(last["format_corrections"], 1);
            assert_eq!(last["tool_steps"], 1);
            assert_eq!(last["round"], 2);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn silent_request_has_heartbeat_without_restarting_future() {
        let (p, events) = fixture();
        p.request(1, 0, 8);
        p.wait(tokio::time::sleep(std::time::Duration::from_secs(12)))
            .await;
        p.finish("rejected");
        let e = events.lock().unwrap();
        assert!(e.iter().filter(|v| v["phase"] == "waiting").count() >= 3);
        assert_eq!(e.last().unwrap()["phase"], "rejected");
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_request_closes_progress_without_orphan_heartbeat() {
        let (p, events) = fixture();
        p.request(1, 0, 8);
        {
            let waiting = p.wait(std::future::pending::<()>());
            tokio::pin!(waiting);
            tokio::select! { _ = &mut waiting => unreachable!(), _ = tokio::time::sleep(std::time::Duration::from_secs(6)) => {} }
        }
        drop(p);
        let count = events.lock().unwrap().len();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let e = events.lock().unwrap();
        assert_eq!(e.len(), count);
        assert_eq!(e.last().unwrap()["phase"], "incomplete");
    }

    #[test]
    fn shared_terminal_progress_blocks_late_events_and_drop_incomplete() {
        let (p, events) = fixture();
        let shared = std::sync::Arc::new(p);
        shared.phase("preparing", "正在准备");
        shared.request(1, 0, 8);
        shared.observe_stream_activity("thinking", 12);
        shared.phase("tool", "正在取证：list_child_runs");
        shared.tool_completed(1);
        shared.finish("passed");
        let after_finish = events.lock().unwrap().len();
        shared.phase("waiting", "late event after terminal");
        shared.observe_stream_activity("content", 10);
        shared.tool_completed(2);
        drop(shared);
        let e = events.lock().unwrap();
        assert_eq!(e.len(), after_finish);
        assert_eq!(e.last().unwrap()["phase"], "passed");
        assert_eq!(e.last().unwrap()["tool_steps"], 1);
    }
}
