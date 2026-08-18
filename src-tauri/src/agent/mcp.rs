use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
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
    #[allow(dead_code)]
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
            cmd.env(k, resolve_env_value(v)?);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("[MCP:{}] spawn 失败: {}", config.name, e))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("[MCP:{}] 无法获取 stdin", config.name))?;
        let stdout_raw = child
            .stdout
            .take()
            .ok_or_else(|| format!("[MCP:{}] 无法获取 stdout", config.name))?;

        Ok(Self {
            config,
            _child: child,
            stdin,
            stdout: BufReader::new(stdout_raw),
            next_id: 1,
        })
    }

    pub fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: self.next_id,
            method: method.to_string(),
            params,
        };
        self.next_id += 1;
        let req_str = serde_json::to_string(&req)
            .map_err(|e| format!("[MCP:{}] 序列化失败: {}", self.config.name, e))?;
        writeln!(self.stdin, "{}", req_str)
            .map_err(|e| format!("[MCP:{}] 写入失败: {}", self.config.name, e))?;
        self.stdin
            .flush()
            .map_err(|e| format!("[MCP:{}] flush 失败: {}", self.config.name, e))?;

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|e| format!("[MCP:{}] 读取失败: {}", self.config.name, e))?;
        let resp: JsonRpcResponse = serde_json::from_str(line.trim())
            .map_err(|e| format!("[MCP:{}] 反序列化失败: {}", self.config.name, e))?;
        if let Some(err) = resp.error {
            return Err(format!("[MCP:{}] RPC 错误: {}", self.config.name, err));
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }

    pub fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.ensure_alive()?;
        self.send_request(
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        )
    }

    /// 检查并确保子进程存活，如果已退出则尝试重启
    pub fn ensure_alive(&mut self) -> Result<(), String> {
        let needs_restart = match self._child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        };
        if needs_restart {
            println!("[MCP:{}] 进程已退出，正在重启...", self.config.name);
            let new_client = Self::spawn(self.config.clone())?;
            self._child = new_client._child;
            self.stdin = new_client.stdin;
            self.stdout = new_client.stdout;
            crate::agent::mcp_catalog::invalidate_plugin(&self.config.name);
            let _ = self.initialize();
        }
        Ok(())
    }

    /// MCP 握手（有些 Server 强依赖这一步）
    pub fn initialize(&mut self) -> Result<(), String> {
        let result = self.send_request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "clientInfo": { "name": "free-api-agent", "version": "0.1.0" }
            }),
        );
        // 握手可能有的 server 不需要，所以忽略错误
        let _ = result;
        Ok(())
    }

    /// 查询 MCP Server 提供的所有工具（说明书）
    pub fn list_tools(&mut self) -> Result<Vec<McpToolDef>, String> {
        self.ensure_alive()?;
        let result = self.send_request("tools/list", json!({}))?;
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
        crate::agent::mcp_catalog::remember_tools(&self.config, &defs);
        Ok(defs)
    }
}

pub struct PluginRegistry {
    pub clients: std::collections::HashMap<String, McpClient>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            clients: std::collections::HashMap::new(),
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
            if let Ok(mut client) = McpClient::spawn(config) {
                // 加载时顺便握手并查询一次工具，确保存活
                let _ = client.initialize();
                let tool_count = client.list_tools().map(|t| t.len()).unwrap_or(0);
                println!("🚀 插件加载成功: {} (共 {} 个工具)", name, tool_count);
                registry.clients.insert(name, client);
            } else {
                println!("❌ 插件启动失败: {}", name);
            }
        }
        registry
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut McpClient> {
        self.clients.get_mut(name)
    }
    pub fn plugin_names(&self) -> Vec<&str> {
        self.clients.keys().map(String::as_str).collect()
    }

    /// 查询所有已加载的 MCP Server 的工具列表
    pub fn list_all_tools(&mut self) -> Vec<(String, McpToolDef)> {
        let mut result = Vec::new();
        for (plugin_name, client) in &mut self.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .or_else(|| client.list_tools().ok())
                .unwrap_or_default();
            for tool in tools {
                result.push((plugin_name.clone(), tool));
            }
        }
        result
    }

    /// 【黄金平衡版】工具映射索引 (极简 Token，明确能力)
    pub fn format_tools_menu(&mut self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines
            .push("\n## 🛠️ MCP 插件箱 (如需参数细节请返回 next_tool_hint: \"插件名\")".to_string());

        let mut catalogs = Vec::new();
        for (name, client) in &mut self.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .or_else(|| client.list_tools().ok())
                .unwrap_or_default();
            catalogs.push((name.clone(), tools));
        }
        lines.extend(
            crate::agent::mcp_catalog::concise_menu(catalogs)
                .trim_start_matches(
                    "\n## MCP 工具目录（摘要缓存；需要参数规格时使用 next_tool_hint）",
                )
                .lines()
                .map(str::to_string),
        );

        lines
            .push("- 调用格式: {\"tool\": \"插件名/工具名\", \"command\": {参数对象}}".to_string());
        lines.join("\n")
    }

    /// 核心参数规格（按需加载，支持按插件全量加载或按单个工具精准加载，并压缩冗余 Schema 元数据）
    pub fn format_tool_detail(&mut self, requested_name: &str) -> String {
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
