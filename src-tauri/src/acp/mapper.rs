//! 引擎事件 → ACP `session/update` 的纯映射。
//!
//! 只搬运字段、不做语义判断：哪条是正文、哪条是思考、哪条是工具调用，都按事件的
//! 通道与 `type` 字段机械决定。没有映射的事件类型默认不转发（避免把内部协议 JSON
//! 刷给用户），但会计数；`SANDWICH_ACP_VERBOSE=1` 时以思考流原样披露。

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use serde_json::Value;
use std::collections::HashMap;

/// 工具结果写回客户端时的字符上限；完整原文仍在桌面端可查。
const TOOL_RESULT_CHARS: usize = 4_000;

#[derive(Debug)]
pub(crate) enum Step {
    Update(SessionUpdate),
    /// 引擎进入人工等待（ask_human）。
    Paused {
        wait_id: String,
        message: String,
        form: Value,
    },
    /// 运行到达终态：`succeeded` / `failed` / `cancelled`。
    Terminal {
        state: String,
        message: Option<String>,
    },
    /// 事件流丢失了若干条事件。
    Lagged(u64),
}

pub(crate) struct Mapper {
    run_id: String,
    verbose: bool,
    thinking_sent: HashMap<String, usize>,
    summary_sent: HashMap<String, usize>,
    open_tools: HashMap<String, Vec<String>>,
    tool_seq: u64,
    /// 最后一次完整的交付摘要正文（summary_stream 为累计值）。
    pub(crate) summary_text: String,
    pub(crate) unmapped: HashMap<String, u64>,
}

pub(crate) fn text(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text.into()))
}

pub(crate) fn message(body: impl Into<String>) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(text(body)))
}

pub(crate) fn thought(body: impl Into<String>) -> SessionUpdate {
    SessionUpdate::AgentThoughtChunk(ContentChunk::new(text(body)))
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn step_key(payload: &Value) -> String {
    match payload.get("step_id") {
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

/// 累计文本的增量：只发尚未发送的后缀；累计值变短视为新一段重新开始。
fn delta(sent: &mut usize, full: &str) -> Option<String> {
    let total = full.chars().count();
    if total < *sent {
        *sent = 0;
    }
    if total == *sent {
        return None;
    }
    let tail: String = full.chars().skip(*sent).collect();
    *sent = total;
    Some(tail)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        format!(
            "{}…（已截断，完整输出见桌面端）",
            text.chars().take(limit).collect::<String>()
        )
    }
}

/// 只用于客户端选图标与展示，不参与任何授权判断。
pub(crate) fn tool_kind(tool: &str, action: &str) -> ToolKind {
    let (tool, action) = (tool.to_ascii_lowercase(), action.to_ascii_lowercase());
    if tool.contains("shell") || tool.contains("command") || tool.contains("osascript") {
        ToolKind::Execute
    } else if tool.contains("search") || tool.contains("grep") {
        if tool.contains("web") {
            ToolKind::Fetch
        } else {
            ToolKind::Search
        }
    } else if tool.contains("web") || tool.contains("fetch") || tool.contains("http") {
        ToolKind::Fetch
    } else if action.contains("delete") || action.contains("remove") {
        ToolKind::Delete
    } else if action.contains("move") || action.contains("rename") {
        ToolKind::Move
    } else if ["edit", "write", "create", "append", "patch", "replace"]
        .iter()
        .any(|a| action.contains(a))
    {
        ToolKind::Edit
    } else if ["read", "view", "list", "get", "open"]
        .iter()
        .any(|a| action.contains(a))
    {
        ToolKind::Read
    } else {
        ToolKind::Other
    }
}

pub(crate) fn is_terminal_state(state: &str) -> bool {
    matches!(state, "succeeded" | "done" | "failed" | "cancelled")
}

impl Mapper {
    pub(crate) fn new(run_id: &str, verbose: bool) -> Self {
        Self {
            run_id: run_id.to_string(),
            verbose,
            thinking_sent: HashMap::new(),
            summary_sent: HashMap::new(),
            open_tools: HashMap::new(),
            tool_seq: 0,
            summary_text: String::new(),
            unmapped: HashMap::new(),
        }
    }

    pub(crate) fn handle(&mut self, channel: &str, payload: &Value) -> Vec<Step> {
        if channel == "lagged" {
            return vec![Step::Lagged(
                payload.get("missed").and_then(Value::as_u64).unwrap_or(0),
            )];
        }
        // 同一会话里其他运行（旧运行晚到的事件）不属于本轮。
        if let Some(run) = str_of(payload, "run_id") {
            if run != self.run_id {
                return Vec::new();
            }
        }
        match channel {
            "progress" => self.progress(payload),
            "tool_event" => self.tool_event(payload),
            "run_state" => self.run_state(payload),
            "log" => match (self.verbose, payload.as_str()) {
                (true, Some(line)) => vec![Step::Update(thought(format!("{line}\n")))],
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn run_state(&mut self, payload: &Value) -> Vec<Step> {
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let message = str_of(payload, "message").map(str::to_owned);
        if is_terminal_state(&state) {
            return vec![Step::Terminal { state, message }];
        }
        if self.verbose {
            return vec![Step::Update(thought(format!(
                "· 状态：{state} {}\n",
                message.unwrap_or_default()
            )))];
        }
        Vec::new()
    }

    fn progress(&mut self, payload: &Value) -> Vec<Step> {
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "thinking" => {
                let Some(content) = str_of(payload, "content") else {
                    return Vec::new();
                };
                let sent = self.thinking_sent.entry(step_key(payload)).or_default();
                delta(sent, content)
                    .map(|d| vec![Step::Update(thought(d))])
                    .unwrap_or_default()
            }
            "summary_stream" => {
                let Some(content) = str_of(payload, "content") else {
                    return Vec::new();
                };
                self.summary_text = content.to_string();
                let sent = self.summary_sent.entry(step_key(payload)).or_default();
                delta(sent, content)
                    .map(|d| vec![Step::Update(message(d))])
                    .unwrap_or_default()
            }
            "action_phase" => str_of(payload, "description")
                .map(|d| vec![Step::Update(thought(format!("· {d}\n")))])
                .unwrap_or_default(),
            "step_error" => {
                let what = str_of(payload, "description").unwrap_or("执行步骤");
                vec![Step::Update(thought(format!("⚠️ 步骤未成功：{what}\n")))]
            }
            "llm_parse_error" => vec![Step::Update(thought(
                "⚠️ 模型响应格式异常，引擎正在按既有规则重试\n",
            ))],
            "agent_paused" => str_of(payload, "wait_id")
                .map(|id| {
                    vec![Step::Paused {
                        wait_id: id.to_string(),
                        message: str_of(payload, "message")
                            .unwrap_or("任务请求人工协助")
                            .to_string(),
                        form: payload.get("form").cloned().unwrap_or(Value::Null),
                    }]
                })
                .unwrap_or_default(),
            // 已由工具事件 / 运行状态 / 摘要流覆盖，或是模型原始指令流，不重复转发。
            "complete"
            | "step_new"
            | "step_start"
            | "step_done"
            | "content_stream"
            | "token_usage"
            | "thinking_chunk"
            | "session_title_updated" => Vec::new(),
            other => {
                *self.unmapped.entry(other.to_string()).or_default() += 1;
                if !self.verbose {
                    return Vec::new();
                }
                let detail = str_of(payload, "description")
                    .or_else(|| str_of(payload, "message"))
                    .unwrap_or("");
                vec![Step::Update(thought(format!("[{other}] {detail}\n")))]
            }
        }
    }

    fn tool_event(&mut self, payload: &Value) -> Vec<Step> {
        let tool = str_of(payload, "tool").unwrap_or("tool");
        let action = str_of(payload, "action").unwrap_or("");
        let event_type = str_of(payload, "event_type").unwrap_or("");
        let key = format!("{}:{tool}:{action}", step_key(payload));
        let title = str_of(payload, "description")
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if action.is_empty() {
                    tool.to_string()
                } else {
                    format!("{tool}.{action}")
                }
            });
        let mut steps = Vec::new();
        if event_type == "started" {
            self.tool_seq += 1;
            let id = format!("{}:tool:{}", self.run_id, self.tool_seq);
            self.open_tools.entry(key).or_default().push(id.clone());
            steps.push(Step::Update(SessionUpdate::ToolCall(
                ToolCall::new(id, title)
                    .kind(tool_kind(tool, action))
                    .status(ToolCallStatus::InProgress),
            )));
            return steps;
        }
        let id = match self.open_tools.get_mut(&key).and_then(Vec::pop) {
            Some(id) => id,
            None => {
                // 没收到开始事件（如拒绝在执行前发生）：先补一条工具调用再更新状态。
                self.tool_seq += 1;
                let id = format!("{}:tool:{}", self.run_id, self.tool_seq);
                steps.push(Step::Update(SessionUpdate::ToolCall(
                    ToolCall::new(id.clone(), title)
                        .kind(tool_kind(tool, action))
                        .status(ToolCallStatus::InProgress),
                )));
                id
            }
        };
        let failed = event_type == "failed"
            || payload.get("success").and_then(Value::as_bool) == Some(false);
        let mut fields = ToolCallUpdateFields::new().status(if failed {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        });
        if let Some(result) = str_of(payload, "result_summary") {
            fields = fields.content(vec![ToolCallContent::from(text(truncate(
                result,
                TOOL_RESULT_CHARS,
            )))]);
        }
        steps.push(Step::Update(SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(id, fields),
        )));
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn texts(steps: &[Step]) -> Vec<(String, String)> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Update(SessionUpdate::AgentMessageChunk(c)) => match &c.content {
                    ContentBlock::Text(t) => Some(("message".into(), t.text.clone())),
                    _ => None,
                },
                Step::Update(SessionUpdate::AgentThoughtChunk(c)) => match &c.content {
                    ContentBlock::Text(t) => Some(("thought".into(), t.text.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cumulative_streams_become_deltas_and_other_runs_are_ignored() {
        let mut m = Mapper::new("run-1", false);
        let a = m.handle(
            "progress",
            &json!({"run_id":"run-1","type":"summary_stream","step_id":3,"content":"已完成"}),
        );
        let b = m.handle(
            "progress",
            &json!({"run_id":"run-1","type":"summary_stream","step_id":3,"content":"已完成修复π"}),
        );
        let c = m.handle(
            "progress",
            &json!({"run_id":"run-1","type":"summary_stream","step_id":3,"content":"已完成修复π"}),
        );
        let other = m.handle(
            "progress",
            &json!({"run_id":"old","type":"summary_stream","step_id":3,"content":"旧运行"}),
        );
        assert_eq!(texts(&a), vec![("message".into(), "已完成".into())]);
        assert_eq!(texts(&b), vec![("message".into(), "修复π".into())]);
        assert!(c.is_empty() && other.is_empty());
        assert_eq!(m.summary_text, "已完成修复π");
        let t = m.handle(
            "progress",
            &json!({"run_id":"run-1","type":"thinking","step_id":4,"content":"先读"}),
        );
        assert_eq!(texts(&t), vec![("thought".into(), "先读".into())]);
    }

    #[test]
    fn tool_events_pair_start_and_finish_and_failure_is_reported() {
        let mut m = Mapper::new("r", false);
        let start = m.handle("tool_event", &json!({"run_id":"r","event_type":"started","step_id":2,"tool":"local_file","action":"read","description":"读取 main.rs"}));
        let done = m.handle("tool_event", &json!({"run_id":"r","event_type":"completed","step_id":2,"tool":"local_file","action":"read","success":true,"result_summary":"fn main(){}"}));
        let (
            Step::Update(SessionUpdate::ToolCall(call)),
            Step::Update(SessionUpdate::ToolCallUpdate(update)),
        ) = (&start[0], &done[0])
        else {
            panic!("unexpected {start:?} {done:?}")
        };
        assert_eq!(call.tool_call_id, update.tool_call_id);
        assert_eq!(call.kind, ToolKind::Read);
        assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
        // 无开始事件的失败：补一条调用再标失败
        let failed = m.handle("tool_event", &json!({"run_id":"r","event_type":"failed","step_id":5,"tool":"shell","action":"run","success":false,"result_summary":"exit 1"}));
        assert_eq!(failed.len(), 2);
        let Step::Update(SessionUpdate::ToolCallUpdate(update)) = &failed[1] else {
            panic!()
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
    }

    #[test]
    fn pause_terminal_and_unmapped_events_follow_channel_and_type_only() {
        let mut m = Mapper::new("r", false);
        let paused = m.handle("progress", &json!({"run_id":"r","type":"agent_paused","wait_id":"w","message":"批准？","form":{"kind":"plan_approval"}}));
        assert!(
            matches!(&paused[0], Step::Paused { wait_id, message, form } if wait_id == "w" && message == "批准？" && form["kind"] == "plan_approval")
        );
        let running = m.handle("run_state", &json!({"run_id":"r","state":"executing"}));
        assert!(running.is_empty());
        let end = m.handle(
            "run_state",
            &json!({"run_id":"r","state":"failed","message":"API 401"}),
        );
        assert!(
            matches!(&end[0], Step::Terminal { state, message } if state == "failed" && message.as_deref() == Some("API 401"))
        );
        assert!(m
            .handle(
                "progress",
                &json!({"run_id":"r","type":"review_member_stopped"})
            )
            .is_empty());
        assert_eq!(m.unmapped.get("review_member_stopped"), Some(&1));
        let mut verbose = Mapper::new("r", true);
        let shown = verbose.handle(
            "progress",
            &json!({"run_id":"r","type":"watch_event","description":"文件变化"}),
        );
        assert_eq!(
            texts(&shown),
            vec![("thought".into(), "[watch_event] 文件变化\n".into())]
        );
        assert!(matches!(
            m.handle("lagged", &json!({"missed":7}))[0],
            Step::Lagged(7)
        ));
    }
}
