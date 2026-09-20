use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub capability_card: Option<String>,
}

/// 允许插件 YAML 使用 `${ENV_VAR}` 引用桌面进程环境中的密钥，
/// 避免把 Bearer token 写入可提交的配置文件。
pub fn resolve_env_value(value: &str) -> Result<String, String> {
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
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: Option<String>,
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
    /// 服务端声明的标准 annotations（readOnlyHint/destructiveHint/idempotentHint/
    /// openWorldHint 等）：仅作参考提示保存与展示，不作为宿主安全裁决依据。
    #[serde(default)]
    pub annotations: serde_json::Value,
}

pub type RequestEnvelope = (
    JsonRpcRequest,
    oneshot::Sender<Result<serde_json::Value, String>>,
);

pub fn parse_mcp_response_text(raw_text: &str) -> Result<JsonRpcResponse, String> {
    let trimmed = raw_text.trim();
    if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
        return Ok(resp);
    }
    // 处理 SSE 事件流格式: "data: { ... }"
    for line in trimmed.lines() {
        let line_trimmed = line.trim();
        if let Some(data_content) = line_trimmed.strip_prefix("data:") {
            let json_str = data_content.trim();
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(json_str) {
                return Ok(resp);
            }
        }
    }
    let preview: String = trimmed.chars().take(200).collect();
    Err(format!("无法解析 MCP 响应内容: {}", preview))
}

/// 精简 JSON Schema，剔除 $schema/title/additionalProperties 等无意义冗余字段，大幅削减 Token 消耗
pub fn clean_tool_schema(schema: &serde_json::Value) -> String {
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
