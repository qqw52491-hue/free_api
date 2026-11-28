//! 与正在运行的桌面版通信：本机通道 HTTP API（`127.0.0.1:{port}`，X-Channel-Token 鉴权）。
//!
//! ACP 入口不打开数据库、不跑引擎，所有执行都交给桌面进程，因此不会与桌面版争抢
//! 数据库单实例锁；模型、Key、项目与历史也与桌面版共用同一份。

use futures_util::StreamExt;
use serde_json::{json, Value};
use std::fmt;
use std::io::Write;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

fn endpoint_path() -> std::path::PathBuf {
    crate::agent::channel::channel_config_path().with_extension("acp.json")
}

pub(crate) fn publish_endpoint(port: u16, token: String) -> anyhow::Result<()> {
    let path = endpoint_path();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let staged = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    let bytes = serde_json::to_vec(&Endpoint { port, token })?;
    let result = file.write_all(&bytes);
    drop(file);
    if let Err(error) = result.and_then(|()| std::fs::rename(&staged, path)) {
        let _ = std::fs::remove_file(staged);
        return Err(error.into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum DesktopError {
    /// 桌面版没有运行，或通道服务没有监听。
    Unreachable(String),
    /// 桌面版收到请求但拒绝（带 HTTP 状态码与原始错误说明）。
    Rejected {
        status: u16,
        message: String,
    },
    Other(String),
}

impl fmt::Display for DesktopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DesktopError::Unreachable(base) => write!(
                f,
                "无法连接桌面版 SandwichAgent（{base}）。请先打开 Free API Chat 桌面应用：ACP 入口只负责转发，模型、数据库与执行都在桌面进程里。"
            ),
            DesktopError::Rejected { status, message } => write!(f, "桌面版拒绝了请求（HTTP {status}）：{message}"),
            DesktopError::Other(message) => write!(f, "{message}"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Desktop {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Desktop {
    /// 只读取桌面进程发布的本机端点，或使用同时提供的端点和令牌覆盖。
    pub(crate) fn from_env() -> Result<Self, String> {
        let url = std::env::var("SANDWICH_ACP_CHANNEL_URL").ok();
        let token = std::env::var("SANDWICH_ACP_CHANNEL_TOKEN").ok();
        let (base, token) = match (url, token) {
            (Some(url), Some(token)) => (url, token),
            (None, None) => {
                let path = endpoint_path();
                let raw = std::fs::read(&path).map_err(|error| {
                    format!("无法读取桌面版 ACP 端点（{}）：{error}。请打开支持 ACP 的 Free API Chat 桌面版。", path.display())
                })?;
                let endpoint: Endpoint = serde_json::from_slice(&raw)
                    .map_err(|error| format!("桌面版 ACP 端点配置无效：{error}"))?;
                (
                    format!("http://127.0.0.1:{}", endpoint.port),
                    endpoint.token,
                )
            }
            _ => {
                return Err(
                    "请同时设置 SANDWICH_ACP_CHANNEL_URL 和 SANDWICH_ACP_CHANNEL_TOKEN".into(),
                )
            }
        };
        Self::new(&base, token)
    }

    pub(crate) fn new(base: &str, token: String) -> Result<Self, String> {
        let url = reqwest::Url::parse(base).map_err(|_| "桌面版 ACP 地址无效")?;
        let loopback = url
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        if !loopback
            || url.scheme() != "http"
            || url.path() != "/"
            || url.port() == Some(0)
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || token.trim().is_empty()
        {
            return Err("桌面版 ACP 地址须为本机 HTTP 地址，并提供非空令牌".into());
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            base: url.origin().ascii_serialization(),
            token,
        })
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, DesktopError> {
        let response = request
            .header("X-Channel-Token", &self.token)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    DesktopError::Unreachable(self.base.clone())
                } else {
                    DesktopError::Other(format!("请求桌面版失败：{e}"))
                }
            })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or(body);
        Err(DesktopError::Rejected {
            status: status.as_u16(),
            message,
        })
    }

    pub(crate) async fn post(&self, path: &str, body: Value) -> Result<Value, DesktopError> {
        let response = self
            .send(
                self.http
                    .post(format!("{}{path}", self.base))
                    .json(&body)
                    .timeout(Duration::from_secs(10)),
            )
            .await?;
        response
            .json::<Value>()
            .await
            .map_err(|e| DesktopError::Other(format!("桌面版响应无法解析：{e}")))
    }

    pub(crate) async fn get(&self, path: &str) -> Result<Value, DesktopError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}{path}", self.base))
                    .timeout(Duration::from_secs(5)),
            )
            .await?;
        response
            .json::<Value>()
            .await
            .map_err(|e| DesktopError::Other(format!("桌面版响应无法解析：{e}")))
    }

    pub(crate) async fn create_session(
        &self,
        root: &std::path::Path,
        title: &str,
    ) -> Result<String, DesktopError> {
        let value = self
            .post(
                "/channel/session",
                json!({ "root_path": root.to_string_lossy(), "title": title }),
            )
            .await?;
        value
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| DesktopError::Other("桌面版建会话未返回 session_id".into()))
    }

    pub(crate) async fn start_task(
        &self,
        session_id: &str,
        goal: &str,
        plan_mode: Option<&str>,
    ) -> Result<String, DesktopError> {
        let mut body = json!({ "session_id": session_id, "goal": goal, "caller_notifies": true });
        if let Some(mode) = plan_mode {
            body["plan_mode"] = json!(mode);
        }
        let value = self.post("/channel/task", body).await?;
        value
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| DesktopError::Other("桌面版派任务未返回 run_id".into()))
    }

    pub(crate) async fn answer(
        &self,
        run_id: &str,
        wait_id: &str,
        text: &str,
    ) -> Result<(), DesktopError> {
        self.post(
            &format!("/channel/run/{run_id}/answer"),
            json!({ "text": text, "wait_id": wait_id }),
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn cancel(&self, run_id: &str) -> Result<(), DesktopError> {
        let result = self
            .post(&format!("/channel/run/{run_id}/cancel"), json!({}))
            .await?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(DesktopError::Other("桌面版未确认投递停止请求".into()))
        }
    }

    pub(crate) async fn run_record(&self, run_id: &str) -> Result<Value, DesktopError> {
        self.get(&format!("/channel/run/{run_id}")).await
    }

    /// 订阅会话的完整事件流。返回时服务端已完成订阅，之后再派任务不会漏掉起始事件。
    pub(crate) async fn open_stream(&self, session_id: &str) -> Result<EventStream, DesktopError> {
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            self.send(
                self.http
                    .get(format!("{}/channel/session/{session_id}/stream", self.base)),
            ),
        )
        .await
        .map_err(|_| DesktopError::Other("连接桌面版事件流超时".into()))??;
        let inner = response
            .bytes_stream()
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()));
        Ok(EventStream {
            inner: Box::pin(inner),
            buffer: Vec::new(),
            data: Vec::new(),
        })
    }
}

type ByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Vec<u8>>> + Send>>;

/// 极简 SSE 解析：按空行分隔事件，只取 `data:` 行，注释行（心跳）忽略。
pub(crate) struct EventStream {
    inner: ByteStream,
    buffer: Vec<u8>,
    data: Vec<u8>,
}

impl EventStream {
    /// 返回 `(channel, payload)`；`None` 表示连接结束。
    pub(crate) async fn next(&mut self) -> Option<Result<(String, Value), String>> {
        loop {
            if let Some(event) = take_event(&mut self.buffer, &mut self.data) {
                return Some(event);
            }
            match self.inner.next().await? {
                Ok(chunk) => self.buffer.extend(chunk),
                Err(e) => return Some(Err(format!("事件流读取失败：{e}"))),
            }
        }
    }
}

/// 从缓冲区取出一个完整事件：外层 `None` = 还不完整；内层 `None` = 没有数据（心跳/空事件）。
fn take_event(buffer: &mut Vec<u8>, data: &mut Vec<u8>) -> Option<Result<(String, Value), String>> {
    while let Some(end) = buffer.iter().position(|&byte| byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        let line = &line[..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() && !data.is_empty() {
            let value = serde_json::from_slice::<Value>(&std::mem::take(data));
            return Some(
                value
                    .map_err(|e| format!("无法解析桌面事件：{e}"))
                    .and_then(|value| {
                        let channel = value
                            .get("channel")
                            .and_then(Value::as_str)
                            .ok_or("桌面事件缺少 channel")?
                            .to_string();
                        let payload = value.get("payload").ok_or("桌面事件缺少 payload")?.clone();
                        Ok((channel, payload))
                    }),
            );
        }
        if let Some(line) = line.strip_prefix(b"data:") {
            data.extend_from_slice(line.strip_prefix(b" ").unwrap_or(line));
            data.push(b'\n');
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_heartbeats_and_multiline_data() {
        let mut data = Vec::new();
        let mut buffer =
            b": keep-alive\r\n\r\ndata: {\"channel\":\"progress\",\"payload\":{\"content\":\""
                .to_vec();
        buffer.push("π".as_bytes()[0]);
        assert!(take_event(&mut buffer, &mut data).is_none());
        buffer.push("π".as_bytes()[1]);
        buffer.extend(b"\"}}\r\n\r");
        assert!(take_event(&mut buffer, &mut data).is_none());
        buffer.push(b'\n');
        let (channel, payload) = take_event(&mut buffer, &mut data).unwrap().unwrap();
        assert_eq!(channel, "progress");
        assert_eq!(payload["content"], "π");
        buffer.extend(b"data: {\"channel\":\"log\",\ndata: \"payload\":\"a\"}\n\n");
        let (channel, payload) = take_event(&mut buffer, &mut data).unwrap().unwrap();
        assert_eq!((channel.as_str(), payload.as_str()), ("log", Some("a")));
        assert!(buffer.is_empty());
    }
}
