use crate::agent::mcp::types::{
    parse_mcp_response_text, resolve_env_value, JsonRpcRequest, JsonRpcResponse, McpToolDef,
    PluginConfig, RequestEnvelope,
};
use crate::task_registry::{TaskKind, TaskRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

pub struct McpClient {
    pub config: PluginConfig,
    request_tx: Option<mpsc::Sender<RequestEnvelope>>,
    http_client: reqwest::Client,
    session_pool: Arc<RwLock<HashMap<String, String>>>,
    global_session_id: Arc<RwLock<Option<String>>>,
    session_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    global_init_lock: Arc<Mutex<()>>,
    // stdio 进程生命周期内只允许一次 initialize 握手；ready 标志让并发调用方
    // （目录预热、后台保活、工具调用）串行等待首次握手，而不是各自重发 initialize。
    stdio_ready: Arc<std::sync::atomic::AtomicBool>,
    next_id: Arc<AtomicU64>,
    pub cached_tools: Arc<std::sync::RwLock<Vec<McpToolDef>>>,
}

impl McpClient {
    pub fn spawn(config: PluginConfig) -> Result<Self, String> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("[MCP:{}] 构建 HTTP 客户端失败: {}", config.name, e))?;

        if let Some(ref url) = config.url {
            if !url.trim().is_empty() {
                println!(
                    "🌐 [MCP:{}] 初始化原生 HTTP 直连客户端: {}",
                    config.name, url
                );
                return Ok(Self {
                    config,
                    request_tx: None,
                    http_client,
                    session_pool: Arc::new(RwLock::new(HashMap::new())),
                    global_session_id: Arc::new(RwLock::new(None)),
                    session_locks: Arc::new(RwLock::new(HashMap::new())),
                    global_init_lock: Arc::new(Mutex::new(())),
                    stdio_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    next_id: Arc::new(AtomicU64::new(1)),
                    cached_tools: Arc::new(std::sync::RwLock::new(Vec::new())),
                });
            }
        }

        if config.command.trim().is_empty() {
            return Err(format!(
                "[MCP:{}] 既未指定 url 也未指定 command",
                config.name
            ));
        }

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("PYTHONUNBUFFERED", "1")
            .env("NODE_NO_WARNINGS", "1");

        for (k, v) in &config.env {
            cmd.env(k, resolve_env_value(v)?);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("[MCP:{}] spawn 失败: {}", config.name, e))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("[MCP:{}] 无法获取 stdin", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("[MCP:{}] 无法获取 stdout", config.name))?;

        let (request_tx, mut request_rx) = mpsc::channel::<RequestEnvelope>(64);
        let plugin_name = config.name.clone();

        let mut shutdown_rx = TaskRegistry::global().shutdown_signal();
        let plugin_name_for_task = plugin_name.clone();
        TaskRegistry::global().spawn(
            TaskKind::McpActor,
            format!("mcp-actor:{}", plugin_name),
            async move {
                let mut async_stdin = stdin;
                let mut async_stdout = BufReader::new(stdout);
                let mut line_buf = String::new();
                let mut pending: HashMap<
                    u64,
                    oneshot::Sender<Result<serde_json::Value, String>>,
                > = HashMap::new();

                loop {
                    tokio::select! {
                        shutdown_res = shutdown_rx.changed() => {
                            if shutdown_res.is_ok() && *shutdown_rx.borrow() {
                                println!("[MCP:{}] 收到停机信号，Actor 退出", plugin_name_for_task);
                                break;
                            }
                        }
                        req_opt = request_rx.recv() => {
                            match req_opt {
                                Some((req, resp_tx)) => {
                                    let id = req.id;
                                    pending.insert(id, resp_tx);
                                    match serde_json::to_vec(&req) {
                                        Ok(mut json_bytes) => {
                                            json_bytes.push(b'\n');
                                            if let Err(e) = async_stdin.write_all(&json_bytes).await {
                                                if let Some(tx) = pending.remove(&id) {
                                                    let _ = tx.send(Err(format!("[MCP:{}] stdin 写入失败: {}", plugin_name, e)));
                                                }
                                            }
                                            let _ = async_stdin.flush().await;
                                        }
                                        Err(e) => {
                                            if let Some(tx) = pending.remove(&id) {
                                                let _ = tx.send(Err(format!("[MCP:{}] JSON 序列化失败: {}", plugin_name, e)));
                                            }
                                        }
                                    }
                                }
                                None => break,
                            }
                        }
                        read_res = async_stdout.read_line(&mut line_buf) => {
                            match read_res {
                                Ok(0) => {
                                    println!("[MCP:{}] 子进程输出流已关闭 (EOF)", plugin_name);
                                    break;
                                }
                                Ok(_) => {
                                    let trimmed = line_buf.trim();
                                    if !trimmed.is_empty() {
                                        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                                            let resp_id = match resp.id {
                                                Some(serde_json::Value::Number(n)) => n.as_u64(),
                                                Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
                                                _ => None,
                                            };
                                            if let Some(id) = resp_id {
                                                if let Some(tx) = pending.remove(&id) {
                                                    if let Some(err) = resp.error {
                                                        let _ = tx.send(Err(format!("[MCP:{}] RPC 错误: {}", plugin_name, err)));
                                                    } else {
                                                        let _ = tx.send(Ok(resp.result.unwrap_or(serde_json::Value::Null)));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    line_buf.clear();
                                }
                                Err(e) => {
                                    eprintln!("[MCP:{}] 读取 stdout 出错: {}", plugin_name, e);
                                    break;
                                }
                            }
                        }
                    }
                }

                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err(format!("[MCP:{}] 插件进程已退出/断开", plugin_name)));
                }
                let _ = child.kill().await;
            },
        );

        Ok(Self {
            config,
            request_tx: Some(request_tx),
            http_client,
            session_pool: Arc::new(RwLock::new(HashMap::new())),
            global_session_id: Arc::new(RwLock::new(None)),
            session_locks: Arc::new(RwLock::new(HashMap::new())),
            global_init_lock: Arc::new(Mutex::new(())),
            stdio_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next_id: Arc::new(AtomicU64::new(1)),
            cached_tools: Arc::new(std::sync::RwLock::new(Vec::new())),
        })
    }

    pub async fn get_session_lock(&self, chat_session_id: Option<&str>) -> Arc<Mutex<()>> {
        match chat_session_id {
            Some(sid) if !sid.trim().is_empty() => {
                let r = self.session_locks.read().await;
                if let Some(lock) = r.get(sid) {
                    return lock.clone();
                }
                drop(r);
                let mut w = self.session_locks.write().await;
                w.entry(sid.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            }
            _ => self.global_init_lock.clone(),
        }
    }

    pub async fn get_session_id(&self, chat_session_id: Option<&str>) -> Option<String> {
        if let Some(sid) = chat_session_id {
            if !sid.trim().is_empty() {
                if let Some(mcp_sid) = self.session_pool.read().await.get(sid) {
                    return Some(mcp_sid.clone());
                }
            }
        }
        self.global_session_id.read().await.clone()
    }

    pub async fn set_session_id(&self, chat_session_id: Option<&str>, new_sid: Option<String>) {
        if let Some(sid) = chat_session_id {
            if !sid.trim().is_empty() {
                let mut w = self.session_pool.write().await;
                if let Some(ref val) = new_sid {
                    w.insert(sid.to_string(), val.clone());
                } else {
                    w.remove(sid);
                }
            }
        }
        let mut g = self.global_session_id.write().await;
        *g = new_sid;
    }

    pub async fn ensure_session_initialized(
        &self,
        chat_session_id: Option<&str>,
    ) -> Result<(), String> {
        if self.config.url.is_some() {
            if self.get_session_id(chat_session_id).await.is_some() {
                return Ok(());
            }

            let lock = self.get_session_lock(chat_session_id).await;
            let _guard = lock.lock().await;
            if self.get_session_id(chat_session_id).await.is_some() {
                return Ok(());
            }

            self.initialize_for_session(chat_session_id).await?;
            return Ok(());
        }
        // stdio 插件：握手只做一次，所有并发调用方在此串行等待。
        // 若不等待，目录预热会在后台保活任务握手完成前发出 tools/list，
        // 服务端按协议拒绝未初始化请求，模型可见工具目录将定格为空。
        if !self
            .stdio_ready
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let _guard = self.global_init_lock.lock().await;
            if !self
                .stdio_ready
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.initialize_for_session(chat_session_id).await?;
                self.stdio_ready
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        Ok(())
    }

    pub async fn send_request_for_session(
        &self,
        chat_session_id: Option<&str>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        if method != "initialize" {
            let _ = self.ensure_session_initialized(chat_session_id).await;
        }
        let first_res = self
            .send_raw_request_with_session(chat_session_id, method, params.clone())
            .await;

        if method != "initialize" && self.config.url.is_some() {
            if let Err(ref err_msg) = first_res {
                let is_session_lost = err_msg.contains("Session not found")
                    || err_msg.contains("No valid session ID")
                    || err_msg.contains("session expired")
                    || err_msg.contains("Invalid session")
                    || err_msg.contains("session destroyed")
                    || err_msg.contains("401 Unauthorized");

                if is_session_lost {
                    println!(
                        "🔄 [MCP:{}] 会话 {:?} Session 真正失效，持锁重新握手自愈...",
                        self.config.name, chat_session_id
                    );
                    let lock = self.get_session_lock(chat_session_id).await;
                    let _guard = lock.lock().await;
                    self.set_session_id(chat_session_id, None).await;
                    if let Ok(()) = self.initialize_for_session(chat_session_id).await {
                        return self
                            .send_raw_request_with_session(chat_session_id, method, params)
                            .await;
                    }
                }
            }
        }

        first_res
    }

    pub async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.send_request_for_session(None, method, params).await
    }

    pub async fn send_raw_request_with_session(
        &self,
        chat_session_id: Option<&str>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params: params.clone(),
        };

        if let Some(ref raw_url) = self.config.url {
            let mut req_builder = self
                .http_client
                .post(raw_url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .json(&req);

            for (k, v) in &self.config.headers {
                let val = resolve_env_value(v).unwrap_or_else(|_| v.clone());
                req_builder = req_builder.header(k, val);
            }

            let current_sid = self.get_session_id(chat_session_id).await;
            if let Some(ref sid) = current_sid {
                req_builder = req_builder.header("mcp-session-id", sid);
            }
            println!(
                "🚀 [MCP:{}] 发送 HTTP 请求 ({:?}): method={}, id={}, has_sid={:?}",
                self.config.name, chat_session_id, method, id, current_sid
            );

            let tool_name_in_params = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let is_slow_tool = tool_name_in_params.contains("index_")
                || tool_name_in_params.contains("search_code")
                || tool_name_in_params.contains("build_")
                || tool_name_in_params.contains("clone")
                || tool_name_in_params.contains("compile");
            let timeout_secs = if is_slow_tool { 180 } else { 60 };

            let mut resp =
                match tokio::time::timeout(Duration::from_secs(timeout_secs), req_builder.send())
                    .await
                {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        return Err(format!("[MCP:{}] HTTP 请求失败: {}", self.config.name, e))
                    }
                    Err(_) => {
                        return Err(format!(
                            "[MCP:{}] HTTP 请求超时 ({}s 熔断)",
                            self.config.name, timeout_secs
                        ))
                    }
                };

            let resp_status = resp.status();
            let resp_headers = resp.headers().clone();

            for hname in &[
                "mcp-session-id",
                "mcp_session_id",
                "x-session-id",
                "session-id",
            ] {
                if let Some(sid_header) = resp_headers.get(*hname) {
                    if let Ok(sid_str) = sid_header.to_str() {
                        let clean = sid_str.trim().to_string();
                        if !clean.is_empty() {
                            println!(
                                "🔑 [MCP:{}] 捕获到 Session ID ({:?}): {}",
                                self.config.name, chat_session_id, clean
                            );
                            self.set_session_id(chat_session_id, Some(clean)).await;
                            break;
                        }
                    }
                }
            }

            let mut raw_body_bytes = Vec::new();
            let max_bytes = 10 * 1024 * 1024;
            let mut truncated = false;

            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| format!("[MCP:{}] 读取块失败: {}", self.config.name, e))?
            {
                raw_body_bytes.extend_from_slice(&chunk);
                if raw_body_bytes.len() > max_bytes {
                    println!(
                        "⚠️ [MCP:{}] 响应体过大 (超过 10MB)，触发安全截断!",
                        self.config.name
                    );
                    truncated = true;
                    break;
                }
            }
            let mut raw_body = String::from_utf8_lossy(&raw_body_bytes).into_owned();
            if truncated {
                raw_body.push_str("\n\n[ERROR: MCP 响应超过 10MB 大小限制，已被截断以保护内存，请分段读取或检查工具调用]");
            }

            let body_preview: String = raw_body.chars().take(120).collect();
            println!(
                "📥 [MCP:{}] 收到 HTTP 响应 (Status {}): len={}, body_preview={}",
                self.config.name,
                resp_status,
                raw_body.len(),
                body_preview
            );

            let json_resp = parse_mcp_response_text(&raw_body)
                .map_err(|e| format!("[MCP:{}] {}", self.config.name, e))?;

            if let Some(res_obj) = json_resp.result.as_ref().and_then(|r| r.as_object()) {
                if let Some(sid_val) = res_obj.get("sessionId").and_then(|v| v.as_str()) {
                    self.set_session_id(chat_session_id, Some(sid_val.to_string()))
                        .await;
                } else if let Some(meta_sid) = res_obj
                    .get("_meta")
                    .and_then(|m| m.get("sessionId"))
                    .and_then(|v| v.as_str())
                {
                    self.set_session_id(chat_session_id, Some(meta_sid.to_string()))
                        .await;
                }
            }

            if let Some(err) = json_resp.error {
                return Err(format!("[MCP:{}] RPC 错误: {}", self.config.name, err));
            }
            return Ok(json_resp.result.unwrap_or(serde_json::Value::Null));
        }

        if let Some(ref tx) = self.request_tx {
            let tool_name_in_params = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let is_slow_tool = tool_name_in_params.contains("index_")
                || tool_name_in_params.contains("search_code")
                || tool_name_in_params.contains("build_")
                || tool_name_in_params.contains("clone")
                || tool_name_in_params.contains("compile");
            let timeout_secs = if is_slow_tool { 180 } else { 60 };

            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((req, resp_tx))
                .await
                .map_err(|_| format!("[MCP:{}] 插件后台通信通道已断开", self.config.name))?;

            match tokio::time::timeout(Duration::from_secs(timeout_secs), resp_rx).await {
                Ok(Ok(res)) => res,
                Ok(Err(_)) => Err(format!("[MCP:{}] 响应通道在接收前断开", self.config.name)),
                Err(_) => Err(format!(
                    "[MCP:{}] 请求超时 ({}s 熔断)",
                    self.config.name, timeout_secs
                )),
            }
        } else {
            Err(format!("[MCP:{}] 未找到可用通信通道", self.config.name))
        }
    }

    pub async fn send_raw_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.send_raw_request_with_session(None, method, params)
            .await
    }

    pub async fn call_tool_for_session(
        &self,
        chat_session_id: Option<&str>,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.send_request_for_session(
            chat_session_id,
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        )
        .await
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.call_tool_for_session(None, tool_name, arguments).await
    }

    pub async fn ping_for_session(&self, chat_session_id: Option<&str>) -> Result<(), String> {
        self.send_request_for_session(chat_session_id, "ping", json!({}))
            .await
            .map(|_| ())
    }

    pub async fn ping(&self) -> Result<(), String> {
        self.ping_for_session(None).await
    }

    pub async fn initialize(&self) -> Result<(), String> {
        self.initialize_for_session(None).await
    }

    pub async fn initialize_for_session(
        &self,
        chat_session_id: Option<&str>,
    ) -> Result<(), String> {
        let res = self
            .send_raw_request_with_session(
                chat_session_id,
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "clientInfo": { "name": "free-api-agent", "version": "0.1.0" }
                }),
            )
            .await;

        match res {
            Ok(v) => {
                println!("✅ [MCP:{}] 握手成功: {:?}", self.config.name, v);
                let _ = self
                    .send_raw_request_with_session(
                        chat_session_id,
                        "notifications/initialized",
                        json!({}),
                    )
                    .await;
                Ok(())
            }
            Err(e) => {
                println!("❌ [MCP:{}] 握手失败: {}", self.config.name, e);
                Err(e)
            }
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, String> {
        let result = self.send_request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let defs: Vec<McpToolDef> = tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let schema = t.get("inputSchema").cloned().unwrap_or(json!({}));
                Some(McpToolDef {
                    name,
                    description,
                    schema,
                })
            })
            .collect();
        *self
            .cached_tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = defs.clone();
        crate::agent::mcp_catalog::remember_tools(&self.config, &defs);
        Ok(defs)
    }

    /// 目录缓存 TTL 过期后的兜底：返回本进程内最近一次成功 tools/list 的结果。
    /// 它不代表当前即时状态，只用于让说明书/菜单在长会话中不退化为空。
    pub fn last_known_tools(&self) -> Vec<McpToolDef> {
        self.cached_tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
