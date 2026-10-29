use crate::agent::mcp::{McpToolDef, PluginConfig};
use crate::llm::ModelConfig;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CATALOG_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
struct CachedCatalog {
    fingerprint: String,
    tools: Vec<McpToolDef>,
    capability_card: Option<String>,
    refreshed_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCatalogStatus {
    pub name: String,
    pub tool_count: usize,
    pub tools: Vec<McpToolDef>,
    pub capability_card: Option<String>,
    pub cached: bool,
    pub refreshed_at_ms: Option<u128>,
    /// 持久健康列（DB 落库真相，跨会话保留）——空字符串=无记录
    #[serde(default)]
    pub last_ok_at: String,
    #[serde(default)]
    pub last_error: String,
}

static CATALOGS: OnceLock<Mutex<HashMap<String, CachedCatalog>>> = OnceLock::new();

fn catalogs() -> &'static Mutex<HashMap<String, CachedCatalog>> {
    CATALOGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(config: &PluginConfig) -> String {
    let env = serde_json::to_string(&config.env).unwrap_or_default();
    let headers = serde_json::to_string(&config.headers).unwrap_or_default();
    format!(
        "{:?}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        config.url,
        config.command,
        config.args.join("\u{1f}"),
        headers,
        env
    )
}

/// 默认的 0 依赖规则型 MCP 能力卡生成器（从各工具描述中提取核心摘要，100% 确定性、零等待）
pub fn generate_rule_based_mcp_card(tools: &[McpToolDef]) -> String {
    if tools.is_empty() {
        return "无可用工具".to_string();
    }
    let mut parts = Vec::new();
    for tool in tools.iter().take(5) {
        let first_line = tool.description.lines().next().unwrap_or("").trim();
        let short_desc: String = first_line.chars().take(36).collect();
        if !short_desc.is_empty() {
            parts.push(format!("{}: {}", tool.name, short_desc));
        } else {
            parts.push(tool.name.clone());
        }
    }
    parts.join(" | ")
}

/// 异步调用 LLM 为 MCP 插件说明书生成超高信息密度的简短能力卡片（限制 100 字内）
pub async fn generate_llm_mcp_card(
    plugin_name: &str,
    tools: &[McpToolDef],
    config: &ModelConfig,
) -> Result<String, String> {
    if tools.is_empty() {
        return Ok("无可用工具".to_string());
    }

    let client = config.build_client()?;
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let mut tools_desc = String::new();
    for t in tools.iter().take(15) {
        let schema_str = serde_json::to_string(&t.schema).unwrap_or_default();
        let schema_capped: String = schema_str.chars().take(240).collect();
        tools_desc.push_str(&format!(
            "- 工具名: {}\n  描述: {}\n  参数摘要: {}\n",
            t.name, t.description, schema_capped
        ));
    }

    let prompt = format!(
        "你是一个 AI Agent 工具架构专家。请根据以下 MCP 插件 [{}] 提供的工具说明，为 Agent 规划器提炼一份高信息密度的【能力卡】（严格控制在 2~3 句话、100 字以内）。\n\
        提炼重点：\n\
        1. 核心定位与适用业务场景（如：Excel 数据与公式处理、本地/远程文件读写与搜索、网页检索等）；\n\
        2. 常用核心动作及对应工具名（如：用 read_text_file/write_file 读写，用 ripgrep_search 检索）；\n\
        3. 关键调用时机或注意事项。\n\
        风格要求：精炼直观、突出业务动作、禁止废话与 Markdown 代码块，直接输出纯文本。\n\n\
        【插件工具列表】\n{}",
        plugin_name, tools_desc
    );

    let messages = vec![
        serde_json::json!({
            "role": "system",
            "content": "你是资深的 AI Agent 工具索引专家，擅长将复杂的 API 文档提炼为高密度的能力卡片。"
        }),
        serde_json::json!({
            "role": "user",
            "content": prompt
        }),
    ];

    let mut body = serde_json::json!({
        "model": config.model_name,
        "messages": messages,
        "max_tokens": 1500,
        "temperature": 0.3,
        "stream": false
    });

    config.apply_provider_capabilities(&mut body, "off");

    println!(
        "🤖 [LLM 能力卡生成] 正在为插件 [{}] 调用模型 [{}]...",
        plugin_name, config.model_name
    );

    let resp = tokio::time::timeout(
        Duration::from_secs(120),
        client.post(&url).headers(headers).json(&body).send(),
    )
    .await
    .map_err(|_| "LLM 生成 MCP 能力卡超时 (120s)".to_string())?
    .map_err(|e| format!("LLM 生成 MCP 能力卡网络请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!("LLM 接口返回错误状态 {}: {}", status, err_body));
    }

    let json_resp: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let message = json_resp
        .get("choices")
        .and_then(|arr| arr.get(0))
        .and_then(|c| c.get("message"));

    let content_opt = message
        .and_then(|m| {
            if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
                Some(s.to_string())
            } else if let Some(arr) = m.get("content").and_then(|v| v.as_array()) {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if !parts.is_empty() {
                    Some(parts.join("\n"))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let content = if let Some(c) = content_opt {
        c
    } else if let Some(reasoning) = message
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|v| v.as_str())
    {
        let r = reasoning.trim();
        if !r.is_empty() {
            r.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(r)
                .to_string()
        } else {
            return Err("LLM 生成能力卡返回内容为空".to_string());
        }
    } else {
        return Err(format!(
            "LLM 响应中缺少 content 字段: {}",
            serde_json::to_string(&json_resp).unwrap_or_default()
        ));
    };

    println!(
        "✨ [LLM 能力卡生成] [{}] 提炼成功: {}",
        plugin_name, content
    );
    Ok(content)
}

pub fn remember_tools(config: &PluginConfig, tools: &[McpToolDef]) {
    let fp = fingerprint(config);
    let mut capability_card = None;

    if let Some(ref explicit_card) = config.capability_card {
        let trimmed = explicit_card.trim();
        if !trimmed.is_empty() {
            capability_card = Some(trimmed.to_string());
        }
    }

    if capability_card.is_none() {
        if let Ok(cache) = catalogs().lock() {
            if let Some(existing) = cache.get(&config.name) {
                if existing.fingerprint == fp && existing.capability_card.is_some() {
                    capability_card = existing.capability_card.clone();
                }
            }
        }
    }

    let entry = CachedCatalog {
        fingerprint: fp,
        tools: tools.to_vec(),
        capability_card,
        refreshed_at: Instant::now(),
    };
    if let Ok(mut cache) = catalogs().lock() {
        cache.insert(config.name.clone(), entry);
    }
}

pub fn cached_tools(config: &PluginConfig) -> Option<Vec<McpToolDef>> {
    let cache = catalogs().lock().ok()?;
    let entry = cache.get(&config.name)?;
    if entry.fingerprint != fingerprint(config) || entry.refreshed_at.elapsed() > CATALOG_TTL {
        return None;
    }
    Some(entry.tools.clone())
}

pub fn cached_capability_card(config: &PluginConfig) -> Option<String> {
    if let Some(ref explicit_card) = config.capability_card {
        let trimmed = explicit_card.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let cache = catalogs().lock().ok()?;
    let entry = cache.get(&config.name)?;
    if entry.fingerprint != fingerprint(config) || entry.refreshed_at.elapsed() > CATALOG_TTL {
        return None;
    }
    entry.capability_card.clone()
}

pub fn set_cached_capability_card(config: &PluginConfig, card: String) {
    if let Ok(mut cache) = catalogs().lock() {
        if let Some(entry) = cache.get_mut(&config.name) {
            if entry.fingerprint == fingerprint(config) {
                entry.capability_card = Some(card);
            }
        }
    }
}

pub fn status(config: &PluginConfig) -> McpCatalogStatus {
    match cached_tools(config) {
        Some(tools) => {
            let card = cached_capability_card(config);
            McpCatalogStatus {
                name: config.name.clone(),
                tool_count: tools.len(),
                tools,
                capability_card: card,
                cached: true,
                refreshed_at_ms: None,
                last_ok_at: String::new(),
                last_error: String::new(),
            }
        }
        None => McpCatalogStatus {
            name: config.name.clone(),
            tool_count: 0,
            tools: Vec::new(),
            capability_card: None,
            cached: false,
            refreshed_at_ms: None,
            last_ok_at: String::new(),
            last_error: String::new(),
        },
    }
}

pub fn invalidate_plugin(name: &str) {
    if let Ok(mut cache) = catalogs().lock() {
        cache.remove(name);
    }
}

pub(crate) fn concise_schema_fields(schema: &serde_json::Value) -> String {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return "参数未声明".to_string();
    };
    if properties.is_empty() {
        return "无参数".to_string();
    }

    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut names = properties.keys().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    // 菜单行字段上限：单工具声明几十个 properties 时不再撑爆目录行；
    // 必填字段优先展示，可选字段按字母序补足，完整 Schema 经 next_tool_hint 取回。
    const MAX_SCHEMA_FIELDS: usize = 12;
    let total_fields = names.len();
    let (required_names, optional_names): (Vec<&str>, Vec<&str>) = names
        .iter()
        .copied()
        .partition(|name| required.contains(name));
    let mut rendered: Vec<String> = required_names
        .iter()
        .take(MAX_SCHEMA_FIELDS)
        .map(|name| format!("{}*", name))
        .collect();
    if rendered.len() < MAX_SCHEMA_FIELDS {
        rendered.extend(
            optional_names
                .iter()
                .take(MAX_SCHEMA_FIELDS - rendered.len())
                .map(|name| format!("{}?", name)),
        );
    }
    if total_fields > rendered.len() {
        rendered.push(format!("…+{}", total_fields - rendered.len()));
    }
    rendered.join(", ")
}

/// 服务端声明 hint 的紧凑旗标：仅作提示展示，不参与宿主安全/读写裁决。
fn annotation_flags(tool: &McpToolDef) -> String {
    let hint = |key: &str| {
        tool.annotations
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let mut flags = Vec::new();
    if hint("destructiveHint") {
        flags.push("destructive");
    }
    if hint("readOnlyHint") {
        flags.push("readOnly");
    }
    if hint("idempotentHint") {
        flags.push("idempotent");
    }
    if hint("openWorldHint") {
        flags.push("openWorld");
    }
    if flags.is_empty() {
        String::new()
    } else {
        format!("[{}]", flags.join(","))
    }
}

pub fn concise_menu(
    items: impl IntoIterator<Item = (String, Vec<McpToolDef>, Option<String>)>,
) -> String {
    let mut sorted_items: Vec<(String, Vec<McpToolDef>, Option<String>)> =
        items.into_iter().collect();
    // 严格按插件名称字母序排序，消除 HashMap 遍历导致的随机性与前缀缓存击穿
    sorted_items.sort_by(|a, b| a.0.cmp(&b.0));

    // 全局上限：每插件 25 条只防"单个大插件"，插件数量一多目录仍会线性膨胀。
    // 总工具数超阈值时整体降级为插件级索引（与 Anthropic tool search /
    // OpenAI defer_loading 的 namespace 常驻同构）：常驻只剩名称+数量+能力卡，
    // 具体工具经 capability_search 检索或 next_tool_hint 点名取回。
    const GLOBAL_MENU_TOOL_CAP: usize = 80;
    let grand_total: usize = sorted_items.iter().map(|(_, t, _)| t.len()).sum();
    if grand_total > GLOBAL_MENU_TOOL_CAP {
        let mut lines = vec![format!(
            "\n## MCP 工具目录（共 {} 个工具，超阈值降级为插件级索引；用 capability_search(query) 检索全部工具，或 next_tool_hint: \"插件名\"/\"插件名/工具名\" 取回详情）",
            grand_total
        )];
        let mut valid_count = 0;
        for (name, tools, card_opt) in sorted_items {
            if tools.is_empty() {
                continue;
            }
            valid_count += 1;
            let card_line = card_opt
                .as_deref()
                .and_then(|c| c.lines().find(|l| !l.trim().is_empty()))
                .map(|l| l.trim().chars().take(100).collect::<String>())
                .unwrap_or_else(|| generate_rule_based_mcp_card(&tools).chars().take(100).collect());
            if card_line.is_empty() {
                lines.push(format!("- {}: {} 个工具", name, tools.len()));
            } else {
                lines.push(format!("- {}: {} 个工具 — {}", name, tools.len(), card_line));
            }
        }
        if valid_count == 0 {
            return String::new();
        }
        lines.push("- 按插件浏览：capability_search(query: \"插件名\", kind: \"mcp\")；按动作搜索可带 namespace 限定插件。结果含 input_schema 时可直接调用，目录已有足够字段时也可直接使用；仅缺参数细节时再点名加载。调用格式: {\"tool\": \"插件名/工具名\", \"command\": {参数对象}}。".to_string());
        return lines.join("\n");
    }

    let mut lines = vec![
        "\n## MCP 工具目录（字段来自服务声明的 JSON Schema；* 为必填，? 为可选；[ ] 内为服务端声明 hint，仅供参考不作安全裁决；需要约束细节请使用 next_tool_hint）"
            .to_string(),
    ];
    let mut valid_count = 0;

    // 大插件只列插件概要，避免字母序前 25 项让后面的动作隐形。
    const MAX_TOOLS_PER_PLUGIN_MENU: usize = 25;

    for (name, mut tools, card_opt) in sorted_items {
        // 🚨 核心过滤：若该插件工具列表为空（连接失败、离线或无可用动作），绝不注入工具目录，杜绝诱导模型调用幽灵工具
        if tools.is_empty() {
            continue;
        }
        valid_count += 1;
        // 稳定排序保护前缀缓存；插件摘要只是定位线索，实际调用仍以服务声明字段为准。
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let total = tools.len();
        if total > MAX_TOOLS_PER_PLUGIN_MENU {
            let card = card_opt.filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| generate_rule_based_mcp_card(&tools));
            let brief: String = card.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
            lines.push(format!("- {}: {} 个工具 — {}（按插件名查询可分页浏览全部动作）", name, total, brief));
            continue;
        }
        let tool_summaries = tools
            .iter()
            .map(|tool| {
                format!(
                    "{}({}){} — {}",
                    tool.name,
                    concise_schema_fields(&tool.schema),
                    annotation_flags(tool),
                    tool.description.lines().next().unwrap_or("").trim().chars().take(80).collect::<String>()
                )
            })
            .collect::<Vec<_>>();
        lines.push(format!("- {}: [{}]", name, tool_summaries.join("; ")));
    }
    if valid_count == 0 {
        return String::new();
    }
    lines.push("- 调用格式: {\"tool\": \"插件名/工具名\", \"command\": {参数对象}}；仅使用上列服务声明字段。".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> PluginConfig {
        PluginConfig {
            name: "demo".to_string(),
            command: "demo-server".to_string(),
            args: vec!["--stdio".to_string()],
            url: None,
            headers: HashMap::new(),
            env: HashMap::new(),
            capability_card: Some("SQLite 持久化能力卡".to_string()),
        }
    }

    #[test]
    fn cache_reuses_matching_config_and_invalidates_on_change() {
        let mut cfg = config();
        remember_tools(
            &cfg,
            &[McpToolDef {
                name: "search".to_string(),
                description: "search code and files".to_string(),
                schema: json!({}),
                annotations: json!({"readOnlyHint": true}),
            }],
        );
        assert_eq!(cached_tools(&cfg).unwrap().len(), 1);
        let card = cached_capability_card(&cfg);
        assert_eq!(card.as_deref(), Some("SQLite 持久化能力卡"));

        // 更新自定义能力卡
        set_cached_capability_card(&cfg, "这是自定义 LLM 能力卡".to_string());
        cfg.capability_card = None;
        assert_eq!(
            cached_capability_card(&cfg).as_deref(),
            Some("这是自定义 LLM 能力卡")
        );

        cfg.args.push("--changed".to_string());
        assert!(cached_tools(&cfg).is_none());
        assert!(cached_capability_card(&cfg).is_none());
        invalidate_plugin("demo");
        assert!(cached_tools(&config()).is_none());
    }

    #[test]
    fn concise_menu_projects_declared_schema_fields_without_inventing_ones() {
        let menu = concise_menu(vec![(
            "workspace".to_string(),
            vec![McpToolDef {
                name: "ripgrep_search".to_string(),
                description: "Search files".to_string(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"},
                        "cwd": {"type": "string"},
                        "glob": {"type": "string"}
                    },
                    "required": ["pattern"]
                }),
                annotations: json!({"readOnlyHint": true, "openWorldHint": true}),
            }],
            None,
        )]);

        // 必填字段优先展示（截断时必填项不被字母序挤掉）
        assert!(menu.contains("ripgrep_search(pattern*, cwd?, glob?)[readOnly,openWorld]"));
        assert!(!menu.contains("path?"));
        assert!(!menu.contains("include?"));
    }

    #[test]
    fn concise_menu_skips_empty_or_failed_plugins() {
        // 插件列表为空或插件内部工具为空时，绝不渲染 - plugin: []
        let empty_menu = concise_menu(vec![
            ("offline_plugin".to_string(), vec![], None),
        ]);
        assert_eq!(empty_menu, "");

        let mixed_menu = concise_menu(vec![
            ("offline_plugin".to_string(), vec![], None),
            (
                "active_plugin".to_string(),
                vec![McpToolDef {
                    name: "do_something".to_string(),
                    description: "active tool".to_string(),
                    schema: json!({ "properties": {} }),
                    annotations: serde_json::Value::Null,
                }],
                None,
            ),
        ]);
        assert!(!mixed_menu.contains("offline_plugin"));
        assert!(mixed_menu.contains("active_plugin: [do_something(无参数) — active tool]"));
    }

    #[test]
    fn single_large_plugin_uses_browseable_summary_instead_of_partial_alphabetical_actions() {
        let tools = (0..55).map(|i| McpToolDef {
            name: format!("action_{i:02}"), description: "Workbook operation".into(),
            schema: json!({"properties":{}}), annotations: serde_json::Value::Null,
        }).collect();
        let menu = concise_menu(vec![("workbooks".into(),tools,Some("表格读写与分析".into()))]);
        assert!(menu.contains("55 个工具 — 表格读写与分析"));
        assert!(menu.contains("分页浏览全部动作"));
        assert!(!menu.contains("action_00("));
    }

    #[test]
    fn concise_menu_degrades_to_plugin_index_beyond_global_cap() {
        // 总量 >80 工具时降级为插件级索引：名称+数量+能力卡，不逐条列工具
        let make_tools = |count: usize| {
            (0..count)
                .map(|i| McpToolDef {
                    name: format!("tool_{:02}", i),
                    description: "d".to_string(),
                    schema: json!({"properties": {"q": {"type": "string"}}}),
                    annotations: serde_json::Value::Null,
                })
                .collect::<Vec<_>>()
        };
        let menu = concise_menu(vec![
            ("alpha".to_string(), make_tools(30), Some("GitHub 仓库操作能力卡".to_string())),
            ("beta".to_string(), make_tools(30), None),
            ("gamma".to_string(), make_tools(30), None),
        ]);
        assert!(menu.contains("共 90 个工具"), "降级索引应显示总量: {}", menu);
        assert!(menu.contains("- alpha: 30 个工具 — GitHub 仓库操作能力卡"));
        assert!(menu.contains("- beta: 30 个工具"));
        assert!(menu.contains("capability_search"));
        assert!(!menu.contains("tool_00("), "降级后不应平铺工具名");
    }

    #[test]
    fn concise_schema_fields_caps_and_prioritizes_required() {
        let mut props = serde_json::Map::new();
        // 15 个可选字段 + 1 个必填字段（字母序靠后，验证必填不被截掉）
        for i in 0..15 {
            props.insert(format!("opt_{:02}", i), json!({"type": "string"}));
        }
        props.insert("zzz_required".to_string(), json!({"type": "string"}));
        let schema = json!({"properties": props, "required": ["zzz_required"]});
        let rendered = concise_schema_fields(&schema);
        assert!(rendered.contains("zzz_required*"), "必填字段必须保留: {}", rendered);
        assert!(rendered.contains("…+"), "超限应显示剩余数: {}", rendered);
        let shown = rendered.split(", ").count();
        assert!(shown <= 13, "最多 12 字段 + 余量标记: {}", rendered);
    }

    #[test]
    fn concise_schema_fields_caps_when_all_fields_required() {
        // 20 个字段全部必填：必填优先不能让上限失效，仍限 12 + 余量标记
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for i in 0..20 {
            let name = format!("req_{:02}", i);
            props.insert(name.clone(), json!({"type": "string"}));
            required.push(name);
        }
        let schema = json!({"properties": props, "required": required});
        let rendered = concise_schema_fields(&schema);
        let shown = rendered.split(", ").count();
        assert!(shown <= 13, "全必填场景也必须受字段上限约束: {}", rendered);
        assert!(rendered.contains("…+8"), "应标记剩余 8 个未列出: {}", rendered);
    }
}
