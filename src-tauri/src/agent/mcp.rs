use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// 允许插件 YAML 使用 `${ENV_VAR}` 引用桌面进程环境中的密钥，
/// 避免把 Bearer token 写入可提交的配置文件。
fn resolve_env_value(value: &str) -> Result<String, String> {
    match value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    {
        Some(key) => std::env::var(key)
            .map_err(|_| format!("MCP 插件要求环境变量 '{}'，但当前桌面进程未设置它", key)),
        None => Ok(value.to_string()),
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    pub id: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
}

/// 工具定义，从 MCP Server 查回来的
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

type RequestEnvelope = (
    JsonRpcRequest,
    oneshot::Sender<Result<serde_json::Value, String>>,
);

/// 🚀 基于 Tokio 的真异步多路复用 MCP 客户端
/// 支持多 Agent 并发调用、单请求 ID 异步派发与 30s 熔断保护
pub struct McpClient {
    pub config: PluginConfig,
    request_tx: mpsc::Sender<RequestEnvelope>,
    next_id: Arc<AtomicU64>,
    pub cached_tools: Arc<RwLock<Vec<McpToolDef>>>,
}

impl McpClient {
    pub fn spawn(config: PluginConfig) -> Result<Self, String> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

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

        // 启动后台多路复用协调任务 (Actor)
        tokio::spawn(async move {
            let mut async_stdin = stdin;
            let mut async_stdout = BufReader::new(stdout);
            let mut line_buf = String::new();
            let mut pending: HashMap<
                u64,
                oneshot::Sender<Result<serde_json::Value, String>>,
            > = HashMap::new();

            loop {
                tokio::select! {
                    // 1. 发送端消息入队并原子写入子进程 stdin
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
                            None => break, // 所有发送端被 drop，正常退出
                        }
                    }
                    // 2. 接收端按行解析并按 id 多路派发响应
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

            // 子进程退出时，唤醒所有滞留的 pending 请求
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(format!("[MCP:{}] 插件进程已退出/断开", plugin_name)));
            }
            let _ = child.kill().await;
        });

        Ok(Self {
            config,
            request_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            cached_tools: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (resp_tx, resp_rx) = oneshot::channel();
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        self.request_tx
            .send((req, resp_tx))
            .await
            .map_err(|_| format!("[MCP:{}] 插件后台通信通道已断开", self.config.name))?;

        // 默认 60s 超时熔断保护，防止任何 MCP 子进程无限期挂起主线程
        match tokio::time::timeout(Duration::from_secs(60), resp_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(format!("[MCP:{}] 响应通道在接收前断开", self.config.name)),
            Err(_) => Err(format!("[MCP:{}] 请求超时 (60s 熔断)", self.config.name)),
        }
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.send_request(
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        )
        .await
    }

    pub async fn initialize(&self) -> Result<(), String> {
        let _ = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "clientInfo": { "name": "free-api-agent", "version": "0.1.0" }
                }),
            )
            .await;
        Ok(())
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
        *self.cached_tools.write().await = defs.clone();
        crate::agent::mcp_catalog::remember_tools(&self.config, &defs);
        Ok(defs)
    }
}

#[derive(Default, Clone)]
pub struct PluginRegistry {
    pub clients: HashMap<String, Arc<McpClient>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    pub fn load_from_dir(plugins_dir: &std::path::Path) -> Self {
        let mut registry = Self::new();
        let Ok(entries) = std::fs::read_dir(plugins_dir) else {
            return registry;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml")
                && path.extension().and_then(|e| e.to_str()) != Some("yml")
            {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let config: PluginConfig = match serde_yaml::from_str(&content) {
                Ok(c) => c,
                Err(e) => {
                    println!("加载插件配置失败 [{}]: {}", path.display(), e);
                    continue;
                }
            };
            let name = config.name.clone();
            if let Ok(client) = McpClient::spawn(config) {
                let client_arc = Arc::new(client);
                // 异步握手与工具探测
                let c = client_arc.clone();
                let n = name.clone();
                tokio::spawn(async move {
                    let _ = c.initialize().await;
                    if let Ok(tools) = c.list_tools().await {
                        println!("🚀 插件就绪: {} (共 {} 个工具)", n, tools.len());
                    }
                });
                registry.clients.insert(name, client_arc);
            } else {
                println!("❌ 插件启动失败: {}", name);
            }
        }
        registry
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.get(name).cloned()
    }

    pub fn plugin_names(&self) -> Vec<&str> {
        self.clients.keys().map(String::as_str).collect()
    }

    pub fn list_all_tools(&self) -> Vec<(String, McpToolDef)> {
        let mut result = Vec::new();
        for (plugin_name, client) in &self.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .unwrap_or_default();
            for tool in tools {
                result.push((plugin_name.clone(), tool));
            }
        }
        result
    }

    /// 【黄金平衡版】工具映射索引 (极简 Token，明确能力，只读无锁零等待)
    pub fn format_tools_menu(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push("\n## 🛠️ MCP 插件箱 (如需参数细节请返回 next_tool_hint: \"插件名\")".to_string());

        let mut catalogs = Vec::new();
        for (name, client) in &self.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .unwrap_or_default();
            catalogs.push((name.clone(), tools));
        }
        lines.extend(
            crate::agent::mcp_catalog::concise_menu(catalogs)
                .trim_start_matches("\n## MCP 工具目录（摘要缓存；需要参数规格时使用 next_tool_hint）")
                .lines()
                .map(str::to_string),
        );

        lines.push("- 调用格式: {\"tool\": \"插件名/工具名\", \"command\": {参数对象}}".to_string());
        lines.join("\n")
    }

    pub fn format_tool_detail(&self, requested_name: &str) -> String {
        let all = self.list_all_tools();
        let (target_plugin, target_tool) = if requested_name.contains('/') {
            let parts: Vec<&str> = requested_name.split('/').collect();
            (parts[0], Some(parts[1]))
        } else {
            (requested_name, None)
        };

        let matched: Vec<_> = all
            .iter()
            .filter(|(p, t)| {
                if p != target_plugin {
                    return false;
                }
                if let Some(tool_name) = target_tool {
                    t.name == tool_name
                } else {
                    true
                }
            })
            .collect();

        if matched.is_empty() {
            return format!("\n❌ 未找到工具/插件 [{}] 的说明书。", requested_name);
        }

        let mut detail = format!(
            "\n## 📦 [{}] 工具规格 (严格遵守 JSON Schema)\n",
            requested_name
        );
        for (plugin, tool) in &matched {
            let compact_schema = clean_tool_schema(&tool.schema);
            detail.push_str(&format!(
                "### {}/{}\n- 描述: {}\n- 参数: {}\n",
                plugin, tool.name, tool.description.trim(), compact_schema
            ));
        }
        detail
    }
}

/// 精简 JSON Schema，剔除 $schema/title/additionalProperties 等无意义冗余字段，大幅削减 Token 消耗
fn clean_tool_schema(schema: &serde_json::Value) -> String {
    if let serde_json::Value::Object(map) = schema {
        let mut cleaned = serde_json::Map::new();
        for (k, v) in map {
            if k == "$schema" || k == "title" {
                continue;
            }
            if k == "additionalProperties" && v.as_bool() == Some(false) {
                continue;
            }
            cleaned.insert(k.clone(), v.clone());
        }
        serde_json::to_string(&serde_json::Value::Object(cleaned)).unwrap_or_default()
    } else {
        schema.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_client_parallel_multiplexing() {
        // 创建一个把请求 id 原样回声的 mock shell 脚本
        let mock_cmd = r#"while IFS= read -r line; do id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2); echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"echo\":$id}}"; done"#;
        let config = PluginConfig {
            name: "test-echo".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), mock_cmd.to_string()],
            env: HashMap::new(),
        };

        let client = Arc::new(McpClient::spawn(config).expect("spawn mock"));

        let mut handles = Vec::new();
        for i in 0..10 {
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                let res = c.send_request("test_method", json!({ "req_idx": i })).await;
                assert!(res.is_ok(), "Request {} failed: {:?}", i, res);
                let val = res.unwrap();
                assert!(val.get("echo").is_some(), "No echo for {}", i);
            }));
        }

        for h in handles {
            h.await.expect("join handle");
        }
    }
}

