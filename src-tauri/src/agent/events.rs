//! 引擎事件统一出口。
//!
//! 主循环、工具派发、子任务与审查向外推送的 `agent-*-{session_id}` 事件全部经由这里：
//! - 桌面前端：照旧经 Tauri `emit` 下发，事件名与载荷与改造前逐字节一致；
//! - 其他下游（通道 SSE、ACP 桥接、评测监视等）：经 [`subscribe`] 取得同一份事件，
//!   不必再对每个会话的四五个字符串通道分别 `listen` 后反解 JSON。
//!
//! 本模块只做搬运，不解释载荷语义：哪一种事件代表什么由各发送点与消费方负责。
//! 订阅侧序列化失败只披露到 stderr，不影响桌面下发（fail-open）。

use serde::Serialize;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::broadcast;

/// 引擎对外事件的通道种类；事件名为 `{prefix}{session_id}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineChannel {
    /// 纯文本运行日志（`agent-log-*`）。
    Log,
    /// 以 `type` 字段区分的进度事件（`agent-progress-*`）。
    Progress,
    /// 工具生命周期事件（`agent-tool-event-*`）。
    ToolEvent,
    /// 运行状态机快照（`agent-run-state-*`）。
    RunState,
    /// 上下文快照（`agent-context-*`）。
    Context,
}

impl EngineChannel {
    pub const ALL: [EngineChannel; 5] = [
        EngineChannel::Log,
        EngineChannel::Progress,
        EngineChannel::ToolEvent,
        EngineChannel::RunState,
        EngineChannel::Context,
    ];

    pub fn prefix(self) -> &'static str {
        match self {
            EngineChannel::Log => "agent-log-",
            EngineChannel::Progress => "agent-progress-",
            EngineChannel::ToolEvent => "agent-tool-event-",
            EngineChannel::RunState => "agent-run-state-",
            EngineChannel::Context => "agent-context-",
        }
    }

    pub fn event_name(self, session_id: &str) -> String {
        format!("{}{}", self.prefix(), session_id)
    }
}

/// 经进程内广播分发给非桌面下游的一条事件。
#[derive(Debug, Clone)]
pub struct EngineEvent {
    pub session_id: String,
    pub channel: EngineChannel,
    /// 与桌面端收到的 JSON 载荷相同（日志为 JSON 字符串）。
    pub payload: serde_json::Value,
}

const BUS_CAPACITY: usize = 1024;

fn bus() -> &'static broadcast::Sender<EngineEvent> {
    static BUS: OnceLock<broadcast::Sender<EngineEvent>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(BUS_CAPACITY).0)
}

/// 订阅全部会话的引擎事件。消费方按 `session_id` 自行过滤；
/// 处理过慢时会收到 `RecvError::Lagged`，应按丢失事件披露而非静默跳过。
pub fn subscribe() -> broadcast::Receiver<EngineEvent> {
    bus().subscribe()
}

fn publish<S: Serialize>(channel: EngineChannel, session_id: &str, payload: &S) {
    let bus = bus();
    // 无订阅者时不做额外序列化，桌面单端运行的开销与改造前相同。
    if bus.receiver_count() == 0 {
        return;
    }
    match serde_json::to_value(payload) {
        Ok(payload) => {
            let _ = bus.send(EngineEvent {
                session_id: session_id.to_string(),
                channel,
                payload,
            });
        }
        Err(error) => eprintln!(
            "[engine-events] {} 事件序列化失败，未转发给订阅者: {error}",
            channel.event_name(session_id)
        ),
    }
}

/// 统一出口：先按原事件名下发桌面，再转发订阅者；返回值即 Tauri `emit` 的结果，
/// 调用点原有的 `.ok()` / `let _ =` / `?` 处理保持不变。
pub fn emit<R: Runtime, S: Serialize + Clone>(
    app: &AppHandle<R>,
    channel: EngineChannel,
    session_id: &str,
    payload: S,
) -> tauri::Result<()> {
    let result = app.emit(&channel.event_name(session_id), payload.clone());
    publish(channel, session_id, &payload);
    result
}

/// 发送点使用的扩展方法；对 `AppHandle`、`&AppHandle` 等调用写法一致。
pub trait EngineEmit {
    fn emit_agent_log<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()>;
    fn emit_agent_progress<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()>;
    fn emit_agent_tool_event<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()>;
    fn emit_agent_run_state<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()>;
    fn emit_agent_context<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()>;
}

impl<R: Runtime> EngineEmit for AppHandle<R> {
    fn emit_agent_log<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()> {
        emit(self, EngineChannel::Log, session_id, payload)
    }
    fn emit_agent_progress<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()> {
        emit(self, EngineChannel::Progress, session_id, payload)
    }
    fn emit_agent_tool_event<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()> {
        emit(self, EngineChannel::ToolEvent, session_id, payload)
    }
    fn emit_agent_run_state<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()> {
        emit(self, EngineChannel::RunState, session_id, payload)
    }
    fn emit_agent_context<S: Serialize + Clone>(&self, session_id: &str, payload: S) -> tauri::Result<()> {
        emit(self, EngineChannel::Context, session_id, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    fn record(app: &tauri::App<tauri::test::MockRuntime>, event: String) -> Arc<Mutex<Vec<String>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let out = seen.clone();
        app.listen(event, move |e| out.lock().unwrap().push(e.payload().to_string()));
        seen
    }

    /// 统一出口与直接 `emit` 在桌面端收到的事件名和载荷完全相同。
    #[test]
    fn desktop_payload_is_identical_to_direct_emit() {
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let sid = format!("events-{}", uuid::Uuid::new_v4());
        let payload = json!({"run_id":"r","type":"content_stream","chunk":"π\"\n","step_id":3});
        for channel in EngineChannel::ALL {
            let seen = record(&app, channel.event_name(&sid));
            Emitter::emit(&handle, &channel.event_name(&sid), payload.clone()).unwrap();
            emit(&handle, channel, &sid, payload.clone()).unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{channel:?}");
            assert_eq!(seen[0], seen[1], "{channel:?}");
        }
        let log = record(&app, EngineChannel::Log.event_name(&sid));
        Emitter::emit(&handle, &EngineChannel::Log.event_name(&sid), format!("⏸️ 等待 {}", "人类")).unwrap();
        handle.emit_agent_log(&sid, format!("⏸️ 等待 {}", "人类")).unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log[0], log[1]);
    }

    /// 订阅者收到与桌面相同的载荷，并带会话与通道标识。
    #[tokio::test]
    async fn subscribers_receive_same_payload_with_channel_identity() {
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let sid = format!("events-sub-{}", uuid::Uuid::new_v4());
        let mut rx = subscribe();
        let desktop = record(&app, EngineChannel::Progress.event_name(&sid));
        let payload = json!({"type":"agent_paused","message":"批准？","form":{"kind":"plan_approval"}});
        handle.emit_agent_progress(&sid, payload.clone()).unwrap();
        handle.emit_agent_log(&sid, "日志").unwrap();
        let mut mine = Vec::new();
        while mine.len() < 2 {
            let event = rx.recv().await.unwrap();
            if event.session_id == sid {
                mine.push(event);
            }
        }
        assert_eq!(mine[0].channel, EngineChannel::Progress);
        assert_eq!(mine[0].payload, payload);
        let desktop_value: Value = serde_json::from_str(&desktop.lock().unwrap()[0]).unwrap();
        assert_eq!(mine[0].payload, desktop_value);
        assert_eq!(mine[1].channel, EngineChannel::Log);
        assert_eq!(mine[1].payload, json!("日志"));
    }

    /// 事实边界守卫：引擎非测试代码不得绕过统一出口直接按字符串拼 `agent-*` 事件名下发，
    /// 否则订阅者（通道/ACP）会静默漏掉这类事件。
    #[test]
    fn engine_code_has_no_raw_agent_event_emits() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let raw = regex_lite_find(&root);
        assert!(raw.is_empty(), "以下位置绕过了 agent::events 直接 emit：\n{}", raw.join("\n"));
    }

    fn regex_lite_find(root: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                if !name.ends_with(".rs") || name.ends_with("tests.rs") || name.ends_with("test.rs") {
                    continue;
                }
                if path.ends_with("agent/events.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                // 去掉全部空白后匹配，覆盖跨行书写的 `app\n.emit(\n&format!("agent-…` 形态。
                let compact: String = strip_test_modules(&text).split_whitespace().collect();
                for channel in EngineChannel::ALL {
                    let prefix = channel.prefix();
                    // 只守下发点；引擎内部仍用 app.listen 收这些事件的 3 处（child_task/runtime、
                    // evaluation_runner、finish_diff 测试）属于后续迁移步骤，不在此拦。
                    for needle in [
                        format!(".emit(&format!(\"{prefix}"),
                        format!(".emit(format!(\"{prefix}"),
                        format!(".emit(\"{prefix}"),
                    ] {
                        let hits = compact.matches(needle.as_str()).count();
                        if hits > 0 {
                            found.push(format!("{} — `{needle}` × {hits}", path.display()));
                        }
                    }
                }
            }
        }
        found
    }

    /// 去掉 `#[cfg(test)] mod x { … }` 块（按花括号配对），保留行号对应关系不重要，只看是否存在。
    fn strip_test_modules(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(pos) = rest.find("#[cfg(test)]") {
            out.push_str(&rest[..pos]);
            let after = &rest[pos..];
            let header_end = after.find('{');
            let is_mod = header_end.is_some_and(|end| after[..end].contains("mod "));
            match (is_mod, header_end) {
                (true, Some(end)) => {
                    let mut depth = 0usize;
                    let mut cut = after.len();
                    for (i, ch) in after[end..].char_indices() {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    cut = end + i + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    rest = &after[cut..];
                }
                _ => {
                    out.push_str("#[cfg(test)]");
                    rest = &after["#[cfg(test)]".len()..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}
