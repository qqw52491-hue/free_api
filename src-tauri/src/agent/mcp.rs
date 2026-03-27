use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
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
    #[allow(dead_code)]
    pub id: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
}

pub struct McpClient {
    pub config: PluginConfig,
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn(config: PluginConfig) -> Result<Self, String> {
        let mut cmd = std::process::Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| format!("[MCP:{}] spawn 失败: {}", config.name, e))?;
        let stdin = child.stdin.take().ok_or_else(|| format!("[MCP:{}] 无法获取 stdin", config.name))?;
        let stdout_raw = child.stdout.take().ok_or_else(|| format!("[MCP:{}] 无法获取 stdout", config.name))?;

        Ok(Self { config, _child: child, stdin, stdout: BufReader::new(stdout_raw), next_id: 1 })
    }

    pub fn send_request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let req = JsonRpcRequest { jsonrpc: "2.0", id: self.next_id, method: method.to_string(), params };
        self.next_id += 1;
        let req_str = serde_json::to_string(&req).map_err(|e| format!("[MCP:{}] 序列化失败: {}", self.config.name, e))?;
        writeln!(self.stdin, "{}", req_str).map_err(|e| format!("[MCP:{}] 写入失败: {}", self.config.name, e))?;
        self.stdin.flush().map_err(|e| format!("[MCP:{}] flush 失败: {}", self.config.name, e))?;

        let mut line = String::new();
        self.stdout.read_line(&mut line).map_err(|e| format!("[MCP:{}] 读取失败: {}", self.config.name, e))?;
        let resp: JsonRpcResponse = serde_json::from_str(line.trim()).map_err(|e| format!("[MCP:{}] 反序列化失败: {}", self.config.name, e))?;
        if let Some(err) = resp.error { return Err(format!("[MCP:{}] RPC 错误: {}", self.config.name, err)); }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }

    pub fn call_tool(&mut self, tool_name: &str, arguments: serde_json::Value) -> Result<serde_json::Value, String> {
        self.send_request("tools/call", json!({ "name": tool_name, "arguments": arguments }))
    }
}

pub struct PluginRegistry {
    clients: std::collections::HashMap<String, McpClient>,
}

impl PluginRegistry {
    pub fn new() -> Self { Self { clients: std::collections::HashMap::new() } }

    pub fn load_from_dir(plugins_dir: &std::path::Path) -> Self {
        let mut registry = Self::new();
        let Ok(entries) = std::fs::read_dir(plugins_dir) else { return registry; };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") && path.extension().and_then(|e| e.to_str()) != Some("yml") { continue; }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let config: PluginConfig = serde_yaml::from_str(&content).unwrap();
            let name = config.name.clone();
            if let Ok(client) = McpClient::spawn(config) { registry.clients.insert(name, client); }
        }
        registry
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut McpClient> { self.clients.get_mut(name) }
    pub fn plugin_names(&self) -> Vec<&str> { self.clients.keys().map(String::as_str).collect() }
}
