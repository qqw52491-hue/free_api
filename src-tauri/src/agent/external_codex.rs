use super::tools::local_shell::{get_enriched_path_env, kill_process_group, sanitize_debugger_env};
use serde_json::{json, Map, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{sleep, timeout},
};

const PROFILE: &str = "free-api-external-project-read";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const WAITING_FOR_APPROVAL: &str = "Codex 请求权限审批，当前连接会拒绝";
const WAITING_FOR_INPUT: &str = "Codex 请求补充输入，当前连接未接入答复";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    Running,
    Waiting { reason: String },
}

pub(crate) async fn run(
    command: &Path,
    cwd: &Path,
    prompt: &str,
    cancelled: impl Fn() -> bool,
    mut progress: impl FnMut(Progress),
) -> Result<String, String> {
    if cancelled() {
        return Err("外部 Codex 已取消".into());
    }
    if !command.is_absolute() || !cwd.is_absolute() {
        return Err("外部 Codex 的启动命令和工作目录必须是绝对路径".into());
    }
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("无法读取 Codex 工作目录：{error}"))?;
    if !cwd.is_dir() || cwd.parent().is_none() {
        return Err("外部 Codex 需要一个具体的项目目录".into());
    }
    let cwd = cwd
        .to_str()
        .ok_or("外部 Codex 工作目录包含不受支持的字符")?;

    let mut server = AppServer::spawn(command, cwd)?;
    let result = tokio::select! {
        biased;
        _ = wait_for_cancel(&cancelled) => Err("外部 Codex 已取消".into()),
        result = async {
            timeout(START_TIMEOUT, server.start_thread(cwd))
                .await
                .map_err(|_| "启动外部 Codex 会话超时")??;
            timeout(TURN_TIMEOUT, server.run_turn(prompt, &mut progress))
                .await
                .map_err(|_| "外部 Codex 执行超时")?
        } => result,
    };
    if result.is_err() {
        let _ = timeout(CLOSE_TIMEOUT, server.interrupt()).await;
    }
    server.stop().await;
    result
}

async fn wait_for_cancel(cancelled: &impl Fn() -> bool) {
    while !cancelled() {
        sleep(Duration::from_millis(150)).await;
    }
}

struct AppServer {
    child: Child,
    process_id: Option<u32>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    thread_id: String,
    turn_id: String,
}

impl AppServer {
    fn spawn(executable: &Path, cwd: &str) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command.args(["app-server", "--stdio"]);
        for feature in [
            "hooks",
            "plugins",
            "apps",
            "multi_agent",
            "memories",
            "goals",
            "skill_mcp_dependency_install",
        ] {
            command.args(["--disable", feature]);
        }
        command
            .current_dir(cwd)
            .env("PATH", get_enriched_path_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        sanitize_debugger_env(&mut command);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动外部 Codex：{error}"))?;
        let stdin = child.stdin.take().ok_or("无法打开外部 Codex 的标准输入")?;
        let stdout = child.stdout.take().ok_or("无法打开外部 Codex 的标准输出")?;
        Ok(Self {
            process_id: child.id(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            thread_id: String::new(),
            turn_id: String::new(),
        })
    }

    async fn start_thread(&mut self, cwd: &str) -> Result<(), String> {
        self.request(
            1,
            "initialize",
            json!({
                "clientInfo": {"name": "free_api_chat", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true},
            }),
        )
        .await?;
        self.send(json!({"method": "initialized"})).await?;

        let configuration = self.request(2, "config/read", json!({"cwd": cwd})).await?;
        let configured_servers = configuration
            .get("config")
            .and_then(Value::as_object)
            .ok_or("外部 Codex 未返回可读取的配置")?;
        if configured_servers
            .get("permissions")
            .and_then(|profiles| profiles.get(PROFILE))
            .is_some()
        {
            return Err("外部 Codex 配置已经占用了项目只读权限名，任务未发送".into());
        }
        let disabled_servers = match configured_servers.get("mcp_servers") {
            Some(Value::Object(servers)) => servers
                .keys()
                .map(|name| (name.clone(), json!({"enabled": false})))
                .collect::<Map<String, Value>>(),
            None | Some(Value::Null) => Map::new(),
            _ => return Err("外部 Codex 返回了无法识别的 MCP 配置".into()),
        };
        let cleared_environment = configuration
            .pointer("/config/shell_environment_policy/set")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|values| values.keys())
            .map(|name| (name.clone(), json!("")))
            .collect::<Map<String, Value>>();

        // An absolute rule confines reads to this project even when the user's Codex
        // configuration defines additional workspace roots. Use no inherited profile.
        let started = self
            .request(
                3,
                "thread/start",
                json!({
                    "cwd": cwd,
                    "ephemeral": true,
                    "approvalPolicy": "never",
                    "permissions": PROFILE,
                    "config": {
                        "default_permissions": PROFILE,
                        "permissions": {
                            (PROFILE): {
                                "filesystem": {":root": "deny", ":minimal": "read", (cwd): "read"},
                                "network": {"enabled": false},
                            },
                        },
                        "mcp_servers": disabled_servers,
                        "web_search": "disabled",
                        "notify": [],
                        "allow_login_shell": false,
                        "shell_environment_policy": {
                            "inherit": "none", "ignore_default_excludes": false,
                            "experimental_use_profile": false, "set": cleared_environment,
                        },
                        "skills": {"include_instructions": false, "bundled": {"enabled": false}},
                    },
                }),
            )
            .await?;

        if started
            .pointer("/activePermissionProfile/id")
            .and_then(Value::as_str)
            != Some(PROFILE)
            || started
                .pointer("/activePermissionProfile/extends")
                .is_some_and(|value| !value.is_null())
            || started.get("approvalPolicy").and_then(Value::as_str) != Some("never")
            || started.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
            || started
                .pointer("/sandbox/networkAccess")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return Err(
                "外部 Codex 未确认项目只读权限，任务未发送。请检查 Codex 版本和权限配置".into(),
            );
        }
        self.thread_id = required_id(started.pointer("/thread/id"), "会话 ID")?;
        Ok(())
    }

    async fn run_turn(
        &mut self,
        prompt: &str,
        progress: &mut impl FnMut(Progress),
    ) -> Result<String, String> {
        let mut current_progress = Progress::Running;
        progress(current_progress.clone());
        self.send(json!({
            "id": 4,
            "method": "turn/start",
            "params": {"threadId": self.thread_id, "input": [{"type": "text", "text": prompt}]},
        }))
        .await?;

        let mut reply = AgentReply::default();
        let mut completed_turn = None;
        let mut start_confirmed = false;
        let mut thread_progress = Progress::Running;
        let mut pending_requests: Vec<(Value, &'static str)> = Vec::new();
        loop {
            let message = self.receive().await?;
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                let params = &message["params"];
                if let Some(id) = message.get("id") {
                    if params.get("threadId").and_then(Value::as_str)
                        == Some(self.thread_id.as_str())
                    {
                        if let Some(reason) = wait_reason(method) {
                            pending_requests.push((id.clone(), reason));
                            update_progress(
                                &mut current_progress,
                                Progress::Waiting {
                                    reason: reason.into(),
                                },
                                progress,
                            );
                        }
                    }
                    self.reject_request(id, method).await?;
                    continue;
                }
                if params.get("threadId").and_then(Value::as_str) != Some(self.thread_id.as_str()) {
                    continue;
                }
                let event_turn = params.get("turnId").or_else(|| params.pointer("/turn/id"));
                if let Some(event_turn) = event_turn.and_then(Value::as_str) {
                    if self.turn_id.is_empty() {
                        self.turn_id = event_turn.to_owned();
                    } else if event_turn != self.turn_id {
                        continue;
                    }
                }
                match method {
                    "thread/status/changed" => {
                        if let Some(next) = status_progress(&params["status"]) {
                            thread_progress = next;
                        }
                    }
                    "serverRequest/resolved" => {
                        pending_requests.retain(|(id, _)| id != &params["requestId"]);
                    }
                    "item/completed" => reply.observe(&params["item"]),
                    "turn/completed" => completed_turn = Some(params["turn"].clone()),
                    _ => {}
                }
            } else if message.get("id").and_then(Value::as_u64) == Some(4) {
                let result = response_payload(message, "turn/start")?;
                let id = required_id(result.pointer("/turn/id"), "轮次 ID")?;
                if !self.turn_id.is_empty() && self.turn_id != id {
                    return Err("外部 Codex 返回了不一致的轮次 ID".into());
                }
                self.turn_id = id;
                start_confirmed = true;
            }

            let next = pending_requests.last().map_or_else(
                || thread_progress.clone(),
                |(_, reason)| Progress::Waiting {
                    reason: (*reason).into(),
                },
            );
            update_progress(&mut current_progress, next, progress);

            if let Some(turn) = completed_turn.take_if(|_| start_confirmed) {
                return reply.finish(&turn);
            }
        }
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        self.send(json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let response = self
                .receive()
                .await
                .map_err(|error| format!("{error}（等待 {method} 响应）"))?;
            let response_method = response.get("method").and_then(Value::as_str);
            match (response.get("id"), response_method) {
                (Some(request_id), Some(request_method)) => {
                    self.reject_request(request_id, request_method).await?
                }
                (Some(response_id), None) if response_id.as_u64() == Some(id) => {
                    return response_payload(response, method)
                }
                _ => {}
            }
        }
    }

    async fn reject_request(&mut self, id: &Value, method: &str) -> Result<(), String> {
        let result = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => json!({"decision": "decline"}),
            "item/permissions/requestApproval" => json!({"permissions": {}, "scope": "turn"}),
            "item/tool/requestUserInput" => json!({"answers": {}}),
            "mcpServer/elicitation/request" => json!({"action": "decline"}),
            _ => return self.send(json!({"id": id, "error": {"code": -32601, "message": "Unsupported request on a delegated Codex connection"}})).await,
        };
        self.send(json!({"id": id, "result": result})).await
    }

    async fn send(&mut self, message: Value) -> Result<(), String> {
        let mut frame = serde_json::to_vec(&message)
            .map_err(|error| format!("无法编码外部 Codex 消息：{error}"))?;
        frame.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("外部 Codex 的输入已关闭")?;
        stdin
            .write_all(&frame)
            .await
            .map_err(|error| format!("无法向外部 Codex 写入消息：{error}"))
    }

    async fn receive(&mut self) -> Result<Value, String> {
        let mut frame = Vec::new();
        let length = (&mut self.stdout)
            .take((MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)
            .await
            .map_err(|error| format!("无法读取外部 Codex 消息：{error}"))?;
        if length == 0 {
            return Err("外部 Codex 在返回完成回执前关闭了连接".into());
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err("外部 Codex 消息过大，无法读取完成回执".into());
        }
        serde_json::from_slice(&frame)
            .map_err(|error| format!("外部 Codex 返回了无法解析的消息：{error}"))
    }

    async fn interrupt(&mut self) -> Result<(), String> {
        if !self.thread_id.is_empty() && !self.turn_id.is_empty() {
            self.send(json!({"id": 5, "method": "turn/interrupt", "params": {
                "threadId": self.thread_id, "turnId": self.turn_id,
            }}))
            .await?;
        }
        Ok(())
    }

    async fn stop(&mut self) {
        drop(self.stdin.take());
        if timeout(CLOSE_TIMEOUT, self.child.wait())
            .await
            .is_ok_and(|exit| exit.is_ok())
        {
            #[cfg(unix)]
            if let Some(pid) = self
                .process_id
                .take()
                .filter(|pid| super::tools::local_shell::process_group_exists(*pid))
            {
                kill_process_group(pid, "KILL");
            }
            self.process_id = None;
        } else {
            if let Some(pid) = self.process_id.take() {
                kill_process_group(pid, "KILL");
            }
            let _ = self.child.kill().await;
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        if let Some(pid) = self.process_id.take() {
            kill_process_group(pid, "KILL");
        }
    }
}

fn response_payload(response: Value, method: &str) -> Result<Value, String> {
    if let Some(error) = response.pointer("/error/message").and_then(Value::as_str) {
        return Err(format!("外部 Codex 的 {method} 请求失败：{error}"));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| format!("外部 Codex 的 {method} 响应缺少结果"))
}

fn required_id(value: Option<&Value>, label: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("外部 Codex 未返回{label}"))
}

fn wait_reason(method: &str) -> Option<&'static str> {
    match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval" => Some(WAITING_FOR_APPROVAL),
        "item/tool/requestUserInput" | "mcpServer/elicitation/request" => Some(WAITING_FOR_INPUT),
        _ => None,
    }
}

fn status_progress(status: &Value) -> Option<Progress> {
    if status.get("type").and_then(Value::as_str) != Some("active") {
        return None;
    }
    let flags = status.get("activeFlags").and_then(Value::as_array)?;
    let reason = if flags
        .iter()
        .any(|flag| flag.as_str() == Some("waitingOnUserInput"))
    {
        WAITING_FOR_INPUT
    } else if flags
        .iter()
        .any(|flag| flag.as_str() == Some("waitingOnApproval"))
    {
        WAITING_FOR_APPROVAL
    } else {
        return Some(Progress::Running);
    };
    Some(Progress::Waiting {
        reason: reason.into(),
    })
}

fn update_progress(current: &mut Progress, next: Progress, progress: &mut impl FnMut(Progress)) {
    if *current != next {
        progress(next.clone());
        *current = next;
    }
}

#[derive(Default)]
struct AgentReply {
    final_answer: Option<String>,
    unknown_phase: Option<String>,
}

impl AgentReply {
    fn observe(&mut self, item: &Value) {
        if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
            return;
        }
        let Some(text) = item
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        else {
            return;
        };
        match item.get("phase").and_then(Value::as_str) {
            Some("final_answer") => self.final_answer = Some(text.to_owned()),
            None => self.unknown_phase = Some(text.to_owned()),
            _ => {}
        }
    }

    fn finish(mut self, turn: &Value) -> Result<String, String> {
        match turn.get("status").and_then(Value::as_str) {
            Some("completed") => {
                if let Some(items) = turn.get("items").and_then(Value::as_array) {
                    for item in items {
                        self.observe(item);
                    }
                }
                self.final_answer
                    .or(self.unknown_phase)
                    .ok_or_else(|| "外部 Codex 已结束，但没有返回最终答复".into())
            }
            Some("interrupted") => Err("外部 Codex 已中断".into()),
            Some("failed") => {
                let message = turn
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("未提供原因");
                Err(format!("外部 Codex 执行失败：{message}"))
            }
            _ => Err("外部 Codex 返回了未知的轮次终态".into()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicBool, Ordering},
    };

    const MOCK_SERVER: &str = r#"#!/usr/bin/env python3
import json
import pathlib
import sys

assert sys.argv[1:3] == ['app-server', '--stdio']
for feature in ['hooks', 'plugins', 'apps', 'multi_agent']:
    assert feature in sys.argv

def read():
    return json.loads(sys.stdin.readline())

def send(message):
    print(json.dumps(message), flush=True)

def request(method):
    message = read()
    assert message['method'] == method, message
    return message

def success(message, result):
    send({'id': message['id'], 'result': result})

def notify(method, **params):
    send({'method': method, 'params': {'threadId': 'thread-1', **params}})

initialized = request('initialize')
assert initialized['params']['capabilities']['experimentalApi']
success(initialized, {})
request('initialized')
success(request('config/read'), {'config': {
    'mcp_servers': {'ambient': {'enabled': True}},
    'shell_environment_policy': {'set': {'AMBIENT': 'original'}},
}})
started = request('thread/start')
params = started['params']
profile = params['permissions']
config = params['config']
assert params['ephemeral']
assert params['approvalPolicy'] == 'never'
assert config['permissions'][profile] == {
    'filesystem': {':root': 'deny', ':minimal': 'read', params['cwd']: 'read'},
    'network': {'enabled': False},
}
assert config['mcp_servers']['ambient']['enabled'] is False
assert config['shell_environment_policy']['set']['AMBIENT'] == ''
reject_profile = pathlib.Path('reject-profile').exists()
success(started, {
    'thread': {'id': 'thread-1'},
    'activePermissionProfile': {'id': ':workspace' if reject_profile else profile},
    'approvalPolicy': 'never',
    'sandbox': {'type': 'readOnly', 'networkAccess': False},
})
if reject_profile:
    if sys.stdin.readline():
        pathlib.Path('unexpected-turn').touch()
    sys.exit(0)

turn = request('turn/start')
assert turn['params']['threadId'] == 'thread-1'
notify('turn/started', turn={'id': 'turn-1', 'status': 'inProgress'})

if turn['params']['input'][0]['text'] == 'cancel me':
    success(turn, {'turn': {'id': 'turn-1'}})
    notify('thread/status/changed', status={'type': 'active', 'activeFlags': ['waitingOnUserInput']})
    interrupt = request('turn/interrupt')
    assert interrupt['params'] == {'threadId': 'thread-1', 'turnId': 'turn-1'}
    pathlib.Path('interrupted').touch()
    success(interrupt, {})
    sys.exit(0)

send({'id': 'approval-1', 'method': 'item/commandExecution/requestApproval', 'params': {
    'threadId': 'thread-1', 'turnId': 'turn-1',
}})
assert read() == {'id': 'approval-1', 'result': {'decision': 'decline'}}
notify('serverRequest/resolved', requestId='approval-1')
notify('thread/status/changed', status={'type': 'active', 'activeFlags': ['waitingOnUserInput']})
send({'id': 'input-1', 'method': 'item/tool/requestUserInput', 'params': {
    'threadId': 'thread-1', 'turnId': 'turn-1',
}})
assert read() == {'id': 'input-1', 'result': {'answers': {}}}
notify('serverRequest/resolved', requestId='input-1')
notify('thread/status/changed', status={'type': 'active', 'activeFlags': []})
notify('item/completed', turnId='turn-1', item={'type': 'agentMessage', 'phase': 'commentary', 'text': 'progress only'})
notify('item/completed', turnId='wrong turn', item={'type': 'agentMessage', 'phase': 'final_answer', 'text': 'wrong turn'})
notify('item/completed', turnId='turn-1', item={'type': 'agentMessage', 'phase': 'final_answer', 'text': '本地模拟结果\n第二行'})
notify('turn/completed', turn={'id': 'turn-1', 'status': 'completed', 'items': []})
success(turn, {'turn': {'id': 'turn-1'}})
"#;

    struct MockServer {
        directory: PathBuf,
        executable: PathBuf,
    }

    impl MockServer {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .join(format!("free-api-external-codex-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            let executable = directory.join("mock-codex");
            fs::write(&executable, MOCK_SERVER).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                directory,
                executable,
            }
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[tokio::test]
    async fn codex_stdio_reports_waits_and_returns_only_the_matching_final_answer() {
        let server = MockServer::new();
        let mut states = Vec::new();
        let answer = run(
            &server.executable,
            &server.directory,
            "local task",
            || false,
            |state| states.push(state),
        )
        .await
        .unwrap();
        assert_eq!(answer, "本地模拟结果\n第二行");
        assert_eq!(
            states,
            vec![
                Progress::Running,
                Progress::Waiting {
                    reason: WAITING_FOR_APPROVAL.into()
                },
                Progress::Running,
                Progress::Waiting {
                    reason: WAITING_FOR_INPUT.into()
                },
                Progress::Running,
            ]
        );
    }

    #[tokio::test]
    async fn codex_stdio_does_not_send_the_task_without_the_read_only_profile() {
        let server = MockServer::new();
        fs::write(server.directory.join("reject-profile"), "").unwrap();
        let error = run(
            &server.executable,
            &server.directory,
            "private task",
            || false,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.contains("未确认项目只读权限"));
        assert!(!server.directory.join("unexpected-turn").exists());
    }

    #[tokio::test]
    async fn codex_stdio_interrupts_an_active_turn_before_closing() {
        let server = MockServer::new();
        let cancelled = AtomicBool::new(false);
        let error = run(
            &server.executable,
            &server.directory,
            "cancel me",
            || cancelled.load(Ordering::SeqCst),
            |state| {
                if matches!(state, Progress::Waiting { .. }) {
                    cancelled.store(true, Ordering::SeqCst);
                }
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("已取消"));
        assert!(server.directory.join("interrupted").is_file());
    }
}
