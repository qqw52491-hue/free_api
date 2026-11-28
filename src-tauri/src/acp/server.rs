//! ACP Agent 端：`initialize` / `session/new` / `session/prompt` / `session/cancel`。
//!
//! 一个 ACP 会话 = 桌面版的一个项目会话（cwd 即项目根目录）。每轮 prompt 交给桌面版执行，
//! 本端只把桌面事件转成 `session/update`，把审批卡转成 `session/request_permission`。
//! 状态分流只看桌面端返回的事实（运行号、是否在等回答、HTTP 状态码），不解析用户文本含义。

use super::desktop::{Desktop, DesktopError};
use super::mapper::{message, text, thought, Mapper, Step};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::*;
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{ByteStreams, Client};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// 发出取消后，最多等待桌面端确认终态的时间；超时仍按协议以 cancelled 结束本轮。
const CANCEL_GRACE: Duration = Duration::from_secs(30);
/// 运行终态事件先于运行账本收尾，取结果时最多等这么久。
const RECORD_WAIT: Duration = Duration::from_secs(10);

const APPROVE_TEXT: &str = "【方案已批准】批准方案，请开始执行后续写入与测试步骤。";
const REJECT_TEXT: &str = "【方案打回修改】请重新审视并按以下意见修改方案后再提交审查：\n（ACP 客户端打回，未附具体意见；如有修改意见会在后续消息中补充。）";

#[derive(Default)]
struct Slot {
    /// 上一轮以普通提问结束，桌面端任务仍在等回答。
    awaiting_answer: Option<(String, String)>,
    busy: bool,
    cancel: Option<watch::Sender<bool>>,
    ignored_mcp: usize,
}

pub(crate) struct Bridge {
    sessions: Mutex<HashMap<String, Slot>>,
    verbose: bool,
    plan_mode: Option<String>,
}

enum Pause {
    Continue,
    EndTurn,
    Cancel,
}

fn send(cx: &ConnectionTo<Client>, sid: &str, update: SessionUpdate) {
    if let Err(error) = cx.send_notification(SessionNotification::new(sid.to_string(), update)) {
        eprintln!("[acp] 推送 session/update 失败：{error}");
    }
}

fn prompt_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::ResourceLink(link) => {
                parts.push(format!("[引用] {} ({})", link.name, link.uri))
            }
            _ => {}
        }
    }
    parts.join("\n")
}

impl Bridge {
    pub(crate) fn new(verbose: bool) -> Arc<Self> {
        let plan_mode = std::env::var("SANDWICH_ACP_PLAN_MODE")
            .ok()
            .filter(|m| !m.trim().is_empty());
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            verbose,
            plan_mode,
        })
    }

    fn slot<T>(&self, sid: &str, f: impl FnOnce(&mut Slot) -> T) -> Option<T> {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        map.get_mut(sid).map(f)
    }

    async fn new_session(&self, req: &NewSessionRequest) -> Result<String, String> {
        let desktop = Desktop::from_env()?;
        if !req.cwd.is_absolute() {
            return Err(format!("cwd 必须是绝对路径：{}", req.cwd.display()));
        }
        let name = req
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| req.cwd.display().to_string());
        let id = desktop
            .create_session(&req.cwd, &format!("ACP · {name}"))
            .await
            .map_err(|e| e.to_string())?;
        let ignored = req.mcp_servers.len();
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Slot {
                    ignored_mcp: ignored,
                    ..Slot::default()
                },
            );
        Ok(id)
    }

    fn cancel(&self, sid: &str) {
        self.slot(sid, |s| {
            if let Some(tx) = &s.cancel {
                let _ = tx.send(true);
            }
        });
    }

    fn claim(&self, sid: &str) -> Result<watch::Receiver<bool>, &'static str> {
        self.slot(sid, |s| {
            if s.busy {
                return Err("该会话上一轮 prompt 尚未结束");
            }
            let (tx, rx) = watch::channel(false);
            s.busy = true;
            s.cancel = Some(tx);
            Ok(rx)
        })
        .unwrap_or(Err("当前 ACP 连接没有该会话，请先新建会话"))
    }

    async fn prompt(
        self: Arc<Self>,
        cx: ConnectionTo<Client>,
        req: PromptRequest,
        responder: Responder<PromptResponse>,
        cancel_rx: watch::Receiver<bool>,
    ) {
        let sid = req.session_id.to_string();
        let goal = prompt_text(&req.prompt);
        let result = if goal.trim().is_empty() {
            Err("请输入文本或文件引用。此版本不接收图片、音频或嵌入资源。".to_string())
        } else {
            if req.prompt.iter().any(|block| {
                !matches!(block, ContentBlock::Text(_) | ContentBlock::ResourceLink(_))
            }) {
                send(&cx, &sid, thought("附带的图片、音频或嵌入资源未转发。\n"));
            }
            self.turn(&cx, &sid, &goal, cancel_rx).await
        };
        self.slot(&sid, |s| {
            s.busy = false;
            s.cancel = None;
        });
        let sent = match result {
            Ok(stop) => responder.respond(PromptResponse::new(stop)),
            Err(error) => responder.respond_with_internal_error(error),
        };
        if let Err(error) = sent {
            eprintln!("[acp] 回复 session/prompt 失败：{error}");
        }
    }

    async fn turn(
        &self,
        cx: &ConnectionTo<Client>,
        sid: &str,
        goal: &str,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> Result<StopReason, String> {
        let desktop = Desktop::from_env()?;
        let (awaiting, ignored) = self
            .slot(sid, |s| {
                (
                    s.awaiting_answer.clone(),
                    std::mem::take(&mut s.ignored_mcp),
                )
            })
            .ok_or("ACP 会话不存在")?;
        if ignored > 0 {
            send(cx, sid, thought(format!(
                "ℹ️ 客户端提供了 {ignored} 个 MCP 服务器，当前版本尚未接入，已忽略；任务使用桌面版已配置的工具。\n"
            )));
        }
        // 先订阅事件流，再派任务/送回答，起始事件不会漏。
        let mut stream = tokio::select! {
            biased;
            _ = cancel_rx.changed() => return Ok(StopReason::Cancelled),
            stream = desktop.open_stream(sid) => stream.map_err(|e| e.to_string())?,
        };
        if *cancel_rx.borrow() {
            return Ok(StopReason::Cancelled);
        }
        let run_id = if let Some((run, wait_id)) = awaiting {
            if crate::agent::plan_gate::extract_plan_decision(goal).is_some() {
                return Err("请使用审批按钮提交结构化审批结论".into());
            }
            match desktop.answer(&run, &wait_id, goal).await {
                Ok(()) => {
                    self.slot(sid, |s| s.awaiting_answer = None);
                }
                Err(DesktopError::Rejected {
                    status: 404 | 409, ..
                }) => {
                    self.slot(sid, |s| s.awaiting_answer = None);
                    return Err(
                        "桌面端已处理或更换这条人工问题，本条回答未提交。请查看桌面端的当前任务。"
                            .into(),
                    );
                }
                Err(error) => return Err(error.to_string()),
            }
            run
        } else {
            desktop
                .start_task(sid, goal, self.plan_mode.as_deref())
                .await
                .map_err(|e| e.to_string())?
        };

        let mut mapper = Mapper::new(&run_id, self.verbose);
        let mut cancelling = false;
        let mut deadline: Option<tokio::time::Instant> = None;
        let mut seen_wait = None;
        let mut poll = tokio::time::interval(Duration::from_secs(2));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        poll.tick().await;
        loop {
            let grace = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            let steps = tokio::select! {
                event = stream.next() => {
                    let (channel, payload) = match event {
                        Some(Ok(event)) => event,
                        Some(Err(error)) => {
                            send(cx, sid, message(format!("\n⚠️ 与桌面版的事件连接出错（{error}），任务可能仍在桌面端运行。\n")));
                            return Err("桌面事件流读取失败，未确认任务终态".into());
                        }
                        None => {
                            send(cx, sid, message("\n⚠️ 与桌面版的事件连接已断开，任务可能仍在桌面端运行，可在桌面端查看。\n"));
                            return Err("桌面事件流已断开，未确认任务终态".into());
                        }
                    };
                    mapper.handle(&channel, &payload)
                }
                _ = cancel_rx.changed(), if !cancelling => {
                    cancelling = true;
                    deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
                    request_cancel(&desktop, cx, sid, &run_id).await;
                    continue;
                }
                _ = grace, if cancelling => {
                    send(cx, sid, message("\nACP 已结束本轮，桌面端的停止状态尚未确认。\n"));
                    return Ok(StopReason::Cancelled);
                }
                _ = poll.tick() => match desktop.run_record(&run_id).await {
                    Ok(record) => record_steps(&record),
                    Err(DesktopError::Rejected { status: 404, .. }) => return Err("桌面版已无法查到本轮运行，请查看桌面会话".into()),
                    Err(_) => continue,
                },
            };
            for step in steps {
                match step {
                    Step::Update(update) => send(cx, sid, update),
                    Step::Lagged(missed) => send(
                        cx,
                        sid,
                        thought(format!(
                            "⚠️ 有 {missed} 条进度事件未能转发，完整记录见桌面端。\n"
                        )),
                    ),
                    Step::Terminal {
                        state,
                        message: reason,
                    } => {
                        return Ok(self
                            .finish(
                                &desktop,
                                cx,
                                sid,
                                &run_id,
                                &state,
                                reason,
                                cancelling || *cancel_rx.borrow(),
                                &mapper,
                            )
                            .await);
                    }
                    Step::Paused {
                        wait_id,
                        message: question,
                        form,
                    } => {
                        if cancelling || seen_wait.as_ref() == Some(&wait_id) {
                            continue;
                        }
                        seen_wait = Some(wait_id.clone());
                        match self
                            .on_paused(
                                &desktop,
                                cx,
                                sid,
                                &run_id,
                                &wait_id,
                                &question,
                                &form,
                                &mut cancel_rx,
                            )
                            .await?
                        {
                            Pause::Continue => {}
                            Pause::EndTurn => return Ok(StopReason::EndTurn),
                            Pause::Cancel => {
                                cancelling = true;
                                deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
                                request_cancel(&desktop, cx, sid, &run_id).await;
                            }
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        desktop: &Desktop,
        cx: &ConnectionTo<Client>,
        sid: &str,
        run_id: &str,
        state: &str,
        reason: Option<String>,
        cancelling: bool,
        mapper: &Mapper,
    ) -> StopReason {
        if !mapper.unmapped.is_empty() {
            eprintln!("[acp] run {run_id} 未映射的进度事件：{:?}", mapper.unmapped);
        }
        let record = wait_record(desktop, run_id).await;
        let field = |key: &str| {
            record
                .as_ref()
                .and_then(|r| r.get(key))
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        };
        let recorded_state = field("status");
        match recorded_state.as_deref().unwrap_or(state) {
            "cancelled" => {
                if !cancelling {
                    send(cx, sid, message("\n任务已在桌面端停止。\n"));
                }
                StopReason::Cancelled
            }
            "failed" => {
                let why = field("error")
                    .or(reason)
                    .unwrap_or_else(|| "未取得失败原因".to_string());
                send(cx, sid, message(format!("\n❌ 任务未完成：{why}\n")));
                if cancelling {
                    StopReason::Cancelled
                } else {
                    StopReason::EndTurn
                }
            }
            _ => {
                let streamed = mapper.summary_text.trim().to_string();
                match field("result") {
                    Some(result) => {
                        let result = result.trim();
                        if streamed.is_empty() {
                            send(cx, sid, message(result.to_string()));
                        } else if result != streamed {
                            match result.strip_prefix(streamed.as_str()) {
                                Some(rest) if rest.trim().is_empty() => {}
                                Some(rest) => send(cx, sid, message(rest.to_string())),
                                None => send(cx, sid, message(format!("\n\n{result}"))),
                            }
                        }
                    }
                    None if streamed.is_empty() => send(
                        cx,
                        sid,
                        message("任务已结束。未取得桌面端的交付正文，可在桌面会话查看。"),
                    ),
                    None => {}
                }
                if cancelling {
                    StopReason::Cancelled
                } else {
                    StopReason::EndTurn
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_paused(
        &self,
        desktop: &Desktop,
        cx: &ConnectionTo<Client>,
        sid: &str,
        run_id: &str,
        wait_id: &str,
        question: &str,
        form: &Value,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> Result<Pause, String> {
        let kind = form
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(kind.as_str(), "plan_approval" | "plan" | "approval") {
            return Ok(self.ask_as_message(cx, sid, run_id, wait_id, question, form));
        }
        let title = form
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("方案审批")
            .to_string();
        let body = form
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(question)
            .to_string();
        let call_id = format!("{run_id}:approval:{}", uuid::Uuid::new_v4());
        send(
            cx,
            sid,
            SessionUpdate::ToolCall(
                ToolCall::new(call_id.clone(), title.clone())
                    .kind(ToolKind::Think)
                    .status(ToolCallStatus::Pending)
                    .content(vec![ToolCallContent::from(text(body))]),
            ),
        );
        let request = RequestPermissionRequest::new(
            sid.to_string(),
            ToolCallUpdate::new(call_id.clone(), ToolCallUpdateFields::new().title(title)),
            vec![
                PermissionOption::new(
                    "approve",
                    "批准方案并开始执行",
                    PermissionOptionKind::AllowOnce,
                ),
                PermissionOption::new("reject", "打回修改", PermissionOptionKind::RejectOnce),
            ],
        );
        let permission = cx.send_request(request).block_task();
        tokio::pin!(permission);
        let mut poll = tokio::time::interval(Duration::from_secs(2));
        let response = loop {
            tokio::select! {
                biased;
                _ = cancel_rx.changed() => {
                    finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                    return Ok(Pause::Cancel);
                }
                response = &mut permission => break response,
                _ = poll.tick() => {
                    if let Ok(record) = desktop.run_record(run_id).await {
                        if record.get("wait_id").and_then(Value::as_str) != Some(wait_id) {
                            finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                            return Ok(Pause::Continue);
                        }
                    }
                }
            }
        };
        let outcome = match response {
            Ok(response) => response.outcome,
            Err(error) => {
                // 客户端处理不了授权请求：退回为普通提问，由用户用下一条消息回答。
                eprintln!("[acp] request_permission 失败，改为文字提问：{error}");
                finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                return Ok(self.ask_as_message(cx, sid, run_id, wait_id, question, form));
            }
        };
        // 只认结构化 optionId；取消视为本轮取消，其他任何结果都不算批准。
        let (reply, status) = match outcome {
            RequestPermissionOutcome::Selected(selected)
                if selected.option_id.to_string() == "approve" =>
            {
                (
                    crate::agent::plan_gate::encode_plan_decision(APPROVE_TEXT, true),
                    ToolCallStatus::Completed,
                )
            }
            RequestPermissionOutcome::Selected(selected)
                if selected.option_id.to_string() == "reject" =>
            {
                (
                    crate::agent::plan_gate::encode_plan_decision(REJECT_TEXT, false),
                    ToolCallStatus::Failed,
                )
            }
            RequestPermissionOutcome::Cancelled => {
                finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                return Ok(Pause::Cancel);
            }
            _ => {
                finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                send(
                    cx,
                    sid,
                    message("客户端未返回可识别的审批选择。可以在桌面端处理这张审批卡。\n"),
                );
                return Ok(Pause::EndTurn);
            }
        };
        match desktop.answer(run_id, wait_id, &reply).await {
            Ok(()) => {
                finish_approval(cx, sid, call_id, status);
                Ok(Pause::Continue)
            }
            Err(DesktopError::Rejected {
                status: 404 | 409, ..
            }) => {
                finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                send(cx, sid, thought("审批卡已处理或更换，本次选择未提交。\n"));
                Ok(Pause::Continue)
            }
            Err(error) => {
                finish_approval(cx, sid, call_id, ToolCallStatus::Failed);
                Err(error.to_string())
            }
        }
    }

    /// 普通提问：把问题作为正文发出并结束本轮；桌面端任务保持等待，下一条 prompt 原文作为回答。
    fn ask_as_message(
        &self,
        cx: &ConnectionTo<Client>,
        sid: &str,
        run_id: &str,
        wait_id: &str,
        question: &str,
        form: &Value,
    ) -> Pause {
        let mut body = format!("\n❓ {question}\n");
        if let Some(description) = form.get("description").and_then(Value::as_str) {
            if !description.trim().is_empty() && description.trim() != question.trim() {
                body.push_str(&format!("\n{description}\n"));
            }
        }
        if let Some(options) = form.get("options").and_then(Value::as_array) {
            for option in options {
                let label = option
                    .as_str()
                    .or_else(|| option.get("label").and_then(Value::as_str))
                    .or_else(|| option.get("id").and_then(Value::as_str));
                if let Some(label) = label {
                    body.push_str(&format!("- {label}\n"));
                }
            }
        }
        body.push_str("\n（直接回复即可把回答交给任务；任务在桌面端保持等待。）\n");
        send(cx, sid, message(body));
        self.slot(sid, |s| {
            s.awaiting_answer = Some((run_id.to_string(), wait_id.to_string()))
        });
        Pause::EndTurn
    }
}

fn finish_approval(cx: &ConnectionTo<Client>, sid: &str, call_id: String, status: ToolCallStatus) {
    send(
        cx,
        sid,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            call_id,
            ToolCallUpdateFields::new().status(status),
        )),
    );
}

fn record_steps(record: &Value) -> Vec<Step> {
    match record.get("status").and_then(Value::as_str) {
        Some(state @ ("done" | "failed" | "cancelled")) => vec![Step::Terminal {
            state: state.into(),
            message: None,
        }],
        Some("waiting_human") => record
            .get("wait_id")
            .and_then(Value::as_str)
            .map(|id| {
                vec![Step::Paused {
                    wait_id: id.into(),
                    message: record
                        .get("paused_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("任务请求人工协助")
                        .into(),
                    form: record.get("paused_form").cloned().unwrap_or(Value::Null),
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn request_cancel(desktop: &Desktop, cx: &ConnectionTo<Client>, sid: &str, run_id: &str) {
    if let Err(error) = desktop.cancel(run_id).await {
        eprintln!("[acp] 请求桌面端取消 {run_id} 失败：{error}");
        send(
            cx,
            sid,
            thought(format!("⚠️ 取消请求未送达桌面端：{error}\n")),
        );
    }
}

async fn wait_record(desktop: &Desktop, run_id: &str) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + RECORD_WAIT;
    loop {
        if let Ok(record) = desktop.run_record(run_id).await {
            let status = record.get("status").and_then(Value::as_str).unwrap_or("");
            if matches!(status, "done" | "failed" | "cancelled") {
                return Some(record);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(crate) async fn serve<W, R>(
    out: W,
    inp: R,
    bridge: Arc<Bridge>,
) -> agent_client_protocol::Result<()>
where
    W: futures::io::AsyncWrite + Send + 'static,
    R: futures::io::AsyncRead + Send + 'static,
{
    let (b_new, b_prompt, b_cancel) = (bridge.clone(), bridge.clone(), bridge);
    agent_client_protocol::Agent
        .v1()
        .name("sandwich-agent")
        .on_receive_request(
            async move |_req: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(AgentCapabilities::new())
                        .agent_info(Implementation::new(
                            "sandwich-agent",
                            env!("CARGO_PKG_VERSION"),
                        )),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        cx: ConnectionTo<Client>| {
                let bridge = b_new.clone();
                cx.spawn(async move {
                    match bridge.new_session(&req).await {
                        Ok(id) => responder.respond(NewSessionResponse::new(id)),
                        Err(error) => responder.respond_with_internal_error(error),
                    }
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                let bridge = b_prompt.clone();
                let cancel_rx = match bridge.claim(&req.session_id.to_string()) {
                    Ok(rx) => rx,
                    Err(error) => return responder.respond_with_internal_error(error),
                };
                let connection = cx.clone();
                cx.spawn(async move {
                    bridge.prompt(connection, req, responder, cancel_rx).await;
                    Ok(())
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, _cx| {
                b_cancel.cancel(&notification.session_id.to_string());
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(ByteStreams::new(out, inp))
        .await
}
