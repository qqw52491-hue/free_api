//! MCP 官方 Registry（registry.modelcontextprotocol.io）在线检索与一键安装。
//!
//! Registry REST API 公开可搜（无需鉴权），每条 server 记录给出两种接入形态：
//! - `remotes[]`（streamable-http）→ 零安装直连，写入 mcp_plugins.url
//! - `packages[]`（pypi/npm/mcpb）→ 本地 stdio 包；pypi 走 `uv tool run`
//!   （uv 运行时由 uv_runtime 自动就绪），npm 走 `npx`
//!
//! 安装侧只负责把解析好的接入方式落库（与手动"接入 MCP"同表），
//! 不启动进程——spawn 发生在下个会话的 session_bootstrap 同步点。

use super::mcp::McpPluginRow;
use crate::agent::mcp::{McpClient, PluginConfig};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use tauri::State;

const REGISTRY_API: &str = "https://registry.modelcontextprotocol.io/v0/servers";

/// 前端卡片展示用的归一化条目。
#[derive(Debug, Clone, Serialize)]
pub struct McpRegistryEntry {
    /// registry 全名（io.github.x/y），去重主键
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    /// active / deprecated
    pub status: String,
    pub repository_url: Option<String>,
    /// 首选安装形态：http / uvx / npx / mcpb / unsupported
    pub install_kind: String,
    /// http 形态的远端 URL
    pub remote_url: Option<String>,
    /// 本地包形态的 registry 标识符（pypi 包名 / npm 包名）
    pub package: Option<String>,
    /// 包自带的 runtimeArguments（按序拼到包名之后）
    pub runtime_args: Vec<String>,
    /// 必填的 env/header 名列表（安装时向用户收集）
    pub required_secrets: Vec<String>,
}

/// 安装结果：插件行 + 握手实拉到的工具数（前端提示"已安装（N 个工具）"）。
#[derive(Debug, Clone, Serialize)]
pub struct McpRegistryInstallResult {
    #[serde(flatten)]
    pub row: McpPluginRow,
    pub tool_count: usize,
}

#[tauri::command]
pub async fn search_mcp_registry(query: String) -> Result<Vec<McpRegistryEntry>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}?search={}", REGISTRY_API, urlencoding(q));
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Registry 请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Registry 返回 HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Registry 响应解析失败: {e}"))?;

    let servers = body
        .get("servers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // 同名条目按版本重复返回——只保留 isLatest 的 active 版本
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in servers {
        let server = item.get("server").cloned().unwrap_or_default();
        let meta = item
            .pointer("/_meta/io.modelcontextprotocol.registry~1official")
            .cloned()
            .unwrap_or_default();
        let status = meta
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            .to_string();
        let is_latest = meta
            .get("isLatest")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if status != "active" || !is_latest {
            continue;
        }
        let name = server
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        out.push(normalize_entry(&server, status));
    }
    Ok(out)
}

/// 把 registry 原始 server JSON 归一化为前端卡片条目，并选出首选安装形态。
fn normalize_entry(server: &serde_json::Value, status: String) -> McpRegistryEntry {
    let str_field = |key: &str| {
        server
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let repository_url = server
        .get("repository")
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 本地包优先（离线可用、可操作本地文件）；其次远程直连；npm 需 Node；
    // mcpb 暂不支持自动装（卡片标注）
    let packages = server
        .get("packages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let remotes = server
        .get("remotes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let pick_pkg = |kind: &str| {
        packages.iter().find(|p| {
            p.get("registryType").and_then(|v| v.as_str()) == Some(kind)
        })
    };

    let mut required_secrets: Vec<String> = Vec::new();
    let (install_kind, remote_url, package, runtime_args) = if let Some(pkg) =
        pick_pkg("pypi")
    {
        let (id, args, secrets) = package_parts(pkg);
        required_secrets = secrets;
        ("uvx".to_string(), None, Some(id), args)
    } else if let Some(pkg) = pick_pkg("npm") {
        let (id, args, secrets) = package_parts(pkg);
        required_secrets = secrets;
        ("npx".to_string(), None, Some(id), args)
    } else if let Some(remote) = remotes.iter().find(|r| {
        r.get("type").and_then(|v| v.as_str()) == Some("streamable-http")
    }) {
        let url = remote
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        for h in remote
            .get("headers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let required = h
                .get("isRequired")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if required {
                if let Some(n) = h.get("name").and_then(|v| v.as_str()) {
                    required_secrets.push(n.to_string());
                }
            }
        }
        ("http".to_string(), url, None, Vec::new())
    } else if pick_pkg("mcpb").is_some() {
        ("mcpb".to_string(), None, None, Vec::new())
    } else {
        ("unsupported".to_string(), None, None, Vec::new())
    };

    McpRegistryEntry {
        name: str_field("name"),
        title: {
            let t = str_field("title");
            if t.is_empty() {
                str_field("name")
            } else {
                t
            }
        },
        description: str_field("description"),
        version: str_field("version"),
        status,
        repository_url,
        install_kind,
        remote_url,
        package,
        runtime_args,
        required_secrets,
    }
}

/// 提取包的 identifier + runtimeArguments + 必填环境变量名。
fn package_parts(pkg: &serde_json::Value) -> (String, Vec<String>, Vec<String>) {
    let id = pkg
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = pkg
        .get("runtimeArguments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    a.get("value")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let secrets = pkg
        .get("environmentVariables")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| {
                    e.get("isRequired").and_then(|v| v.as_bool()).unwrap_or(false)
                })
                .filter_map(|e| {
                    e.get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    (id, args, secrets)
}

/// percent-encode（application/x-www-form-urlencoded 简化版）：
/// 必须按 UTF-8 字节编码——中文等码位直接 %XXXX 是错的
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 一键安装：把 registry 条目落成 mcp_plugins 行。
/// - uvx → ensure_uv 就绪后写 command=uvx（spawn 层自动改写为内置 uv）
/// - npx → 直写 npx（依赖用户 PATH 上的 Node）
/// - http → 直写 url（零子进程）
/// secrets：env_values 为本地包环境变量；http 形态的 bearer_token 进 Authorization 头。
#[tauri::command]
pub async fn install_mcp_registry_entry(
    app: tauri::AppHandle,
    state: State<'_, DbState>,
    name: String,
    install_kind: String,
    package: Option<String>,
    runtime_args: Option<Vec<String>>,
    remote_url: Option<String>,
    bearer_token: Option<String>,
    env_values: Option<std::collections::HashMap<String, String>>,
) -> Result<McpRegistryInstallResult, String> {
    let plugin_name = {
        let n = name.trim();
        if n.is_empty() {
            return Err("插件名不能为空".to_string());
        }
        // registry 全名（org/pkg）截最后一段做本地名，避免斜杠进 YAML/日志
        n.rsplit('/').next().unwrap_or(n).to_string()
    };

    let (command, args, url, headers, env) = match install_kind.as_str() {
        "uvx" => {
            let pkg = package
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| "缺少 pypi 包标识符".to_string())?;
            // uv 运行时自动就绪（首次联网下载官方 release + sha256 校验）
            crate::agent::uv_runtime::ensure_uv(&app)
                .await
                .map_err(|e| format!("uv 运行时就绪失败: {e}"))?;
            let mut a = vec![pkg];
            a.extend(runtime_args.unwrap_or_default());
            ("uvx".to_string(), a, None, Default::default(), env_values.unwrap_or_default())
        }
        "npx" => {
            let pkg = package
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| "缺少 npm 包标识符".to_string())?;
            let mut a = vec!["-y".to_string(), pkg];
            a.extend(runtime_args.unwrap_or_default());
            ("npx".to_string(), a, None, Default::default(), env_values.unwrap_or_default())
        }
        "http" => {
            let u = remote_url
                .filter(|u| !u.trim().is_empty())
                .ok_or_else(|| "缺少远端 URL".to_string())?;
            let mut h = std::collections::HashMap::new();
            if let Some(t) = bearer_token.filter(|t| !t.trim().is_empty()) {
                h.insert("Authorization".to_string(), format!("Bearer {}", t.trim()));
            }
            (String::new(), Vec::new(), Some(u), h, Default::default())
        }
        other => return Err(format!("暂不支持自动安装形态: {other}")),
    };

    // 真机验证安装：spawn + initialize + list_tools——uvx 形态在此实际
    // 下载受管 Python 与包（首次数十秒）；失败直接报错、不落库，
    // 避免"装了但根本跑不起来"的半成品污染插件表。
    let config = PluginConfig {
        name: plugin_name.clone(),
        command: command.clone(),
        args: args.clone(),
        url: url.clone(),
        headers: headers.clone(),
        env: env.clone(),
        capability_card: None,
    };
    let client = McpClient::spawn(config.clone())
        .map_err(|e| format!("启动 MCP 进程失败: {e}"))?;
    let tools = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        client.initialize().await?;
        client.list_tools().await
    })
    .await
    .map_err(|_| "MCP 握手/拉包超时（120s）".to_string())?
    .map_err(|e| format!("MCP 初始化失败: {e}"))?;
    crate::agent::mcp_catalog::remember_tools(&config, &tools);

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
         VALUES (?1, ?2, ?3, ?4, ?5, '', 1, ?6, ?7, ?8)
         ON CONFLICT(name) DO UPDATE SET
            command = excluded.command,
            args = excluded.args,
            env = excluded.env,
            enabled = 1,
            url = excluded.url,
            headers = excluded.headers",
        rusqlite::params![
            id,
            plugin_name,
            command,
            serde_json::to_string(&args).map_err(|e| e.to_string())?,
            serde_json::to_string(&env).map_err(|e| e.to_string())?,
            now,
            url.unwrap_or_default(),
            serde_json::to_string(&headers).map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| format!("保存插件失败: {e}"))?;
    drop(conn);

    crate::agent::mcp_catalog::invalidate_plugin(&plugin_name);

    Ok(McpRegistryInstallResult {
        row: McpPluginRow {
            id,
            name: plugin_name,
            command,
            args,
            env,
            capability_card: String::new(),
            enabled: true,
            created_at: now,
        },
        tool_count: tools.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用官方 Registry 真实响应形态（节选）回归归一化逻辑
    #[test]
    fn normalize_real_registry_shapes() {
        let pypi_server = serde_json::json!({
            "name": "io.github.dosev-ai/mcp-office-excel",
            "title": "MCP Office Excel",
            "description": "65-tool Excel MCP server.",
            "version": "0.7.0",
            "packages": [{
                "registryType": "pypi",
                "identifier": "mcp-office",
                "version": "0.7.0",
                "runtimeHint": "uvx",
                "transport": {"type": "stdio"},
                "runtimeArguments": [{"type": "positional", "value": "stdio"}],
                "environmentVariables": [{"name": "EXCEL_HOME", "isRequired": true}]
            }]
        });
        let e = normalize_entry(&pypi_server, "active".into());
        assert_eq!(e.install_kind, "uvx");
        assert_eq!(e.package.as_deref(), Some("mcp-office"));
        assert_eq!(e.runtime_args, vec!["stdio"]);
        assert_eq!(e.required_secrets, vec!["EXCEL_HOME"]);

        let http_server = serde_json::json!({
            "name": "com.stagenth/excel-chart",
            "title": "Excel 可视化",
            "version": "1.1.0",
            "remotes": [{
                "type": "streamable-http",
                "url": "https://stagenth.com/mcp/excel-chart/",
                "headers": [{"name": "Authorization", "isRequired": true, "isSecret": true}]
            }]
        });
        let e = normalize_entry(&http_server, "active".into());
        assert_eq!(e.install_kind, "http");
        assert_eq!(
            e.remote_url.as_deref(),
            Some("https://stagenth.com/mcp/excel-chart/")
        );
        assert_eq!(e.required_secrets, vec!["Authorization"]);

        // npm 包 → npx 形态
        let npm_server = serde_json::json!({
            "name": "x/fs",
            "version": "1.0.0",
            "packages": [{"registryType": "npm", "identifier": "@modelcontextprotocol/server-filesystem"}]
        });
        assert_eq!(normalize_entry(&npm_server, "active".into()).install_kind, "npx");

        // 无任何可接入形态 → unsupported
        let bare = serde_json::json!({"name": "x/none", "version": "0.1"});
        assert_eq!(normalize_entry(&bare, "active".into()).install_kind, "unsupported");
    }

    #[test]
    fn urlencoding_utf8_bytes() {
        // 中文必须按 UTF-8 字节编码（%E4%B8%AD），不是码位 %4E2D
        assert_eq!(urlencoding("中"), "%E4%B8%AD");
        assert_eq!(urlencoding("excel mcp"), "excel%20mcp");
        assert_eq!(urlencoding("a-b_c.d~e"), "a-b_c.d~e");
    }
}
