//! 统一能力索引与检索（capability_search）：把内置工具、MCP 工具、文件系统 Skill、
//! 自定义指令与角色卡汇总成一张轻量索引，模型用关键词检索而非凭目录猜名。
//!
//! 设计动机：目录型注入在规模增大时必然截断（Skill 目录 60 条、MCP 菜单每插件
//! 25 条、大插件手册 40 条），截断后的条目对模型完全不可见——名字都不知道就
//! 无法 next_tool_hint 点名。本模块提供"搜索全部能力"的发现层，与
//! Anthropic tool search / OpenAI defer_loading 同构，但完全本地实现、
//! 不依赖 provider 特性、保持前缀缓存友好（目录仍是静态文本，检索是显式调用）。
//!
//! 索引每次调用现算：各源（registry 缓存、skills 目录、DB 表）本身就是
//! 热缓存/小表，重建成本远低于维护失效逻辑。评分规则是确定性的纯函数。

use crate::agent::mcp::PluginRegistry;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::{json, Value};

/// 单次返回的命中上限；再多的候选对模型也只是噪声。
const MAX_SEARCH_RESULTS: usize = 15;
const DEFAULT_SEARCH_RESULTS: usize = 5;
/// 索引内 description 的展示上限，避免超长字段撑爆结果。
const DESCRIPTION_PREVIEW_CHARS: usize = 120;

#[derive(Debug)]
struct CapabilityEntry {
    kind: &'static str,
    namespace: String,
    /// 可被模型直接引用的标识：MCP 为 "plugin/tool"，Skill 为文件名，角色为角色标识
    id: String,
    description: String,
    /// 参与评分的次要语料：参数名、动作签名等（不进 description 的信息）
    extra_text: String,
    load_hint: String,
}

/// 汇总所有能力源。每源失败只跳过该源，绝不因单一源故障让整个搜索不可用。
fn collect_entries(
    db: &DbState,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> Vec<CapabilityEntry> {
    let mut entries = Vec::new();

    // 1. 内置工具（静态契约）
    for spec in crate::agent::prompt_assembly::builtin_tool_specs() {
        entries.push(CapabilityEntry {
            kind: "builtin",
            namespace: "core".to_string(),
            id: spec.name.to_string(),
            description: spec.summary.to_string(),
            extra_text: spec.actions.join(" "),
            load_hint: "已在内置工具清单中常驻，可直接调用".to_string(),
        });
    }

    // 2. MCP 工具（含 annotations，参与评分但 hint 文案只作提示）
    for (plugin, tool) in registry.list_all_tools() {
        let arg_names = tool
            .schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|props| props.keys().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let hint_flags = ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"]
            .iter()
            .filter(|key| {
                tool.annotations
                    .get(**key)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        entries.push(CapabilityEntry {
            kind: "mcp_tool",
            namespace: plugin.clone(),
            id: format!("{}/{}", plugin, tool.name),
            description: tool.description.lines().next().unwrap_or("").to_string(),
            extra_text: format!("{} {}", arg_names, hint_flags),
            load_hint: format!("next_tool_hint: \"{}/{}\"", plugin, tool.name),
        });
    }

    // 3. 文件系统 Skill（应用资源目录候选路径 + 工作区 skills/ 目录）
    let mut skill_dirs = vec![
        std::path::PathBuf::from("src-tauri/prompts/skills"),
        std::path::PathBuf::from("prompts/skills"),
        std::path::PathBuf::from("../src-tauri/prompts/skills"),
    ];
    if let Some(root) = workspace_root {
        skill_dirs.push(root.join("skills"));
    }
    for dir in skill_dirs {
        for (name, summary) in crate::agent::skill_context::skill_index_entries(&dir) {
            entries.push(CapabilityEntry {
                kind: "skill",
                namespace: dir.display().to_string(),
                id: name.clone(),
                description: summary,
                extra_text: String::new(),
                load_hint: format!("next_tool_hint: \"{}\"", name),
            });
        }
    }

    // 4. 数据库自定义指令/Skill（能力管理页维护的条目）
    if let Ok(conn) = db.0.lock() {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT name, description, kind FROM agent_custom_instructions WHERE enabled != 0",
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, String>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    entries.push(CapabilityEntry {
                        kind: "instruction",
                        namespace: row.2,
                        id: row.0.clone(),
                        description: row.1,
                        extra_text: String::new(),
                        load_hint: "已随系统提示词的自定义指令区生效，无需加载".to_string(),
                    });
                }
            }
        }
    }

    // 5. 角色卡（仅启用项；禁用项不参与检索结果）
    if let Ok(cards) = crate::agent::tools::role_manager::list_cards(db, false) {
        for card in cards {
            if !card.enabled {
                continue;
            }
            entries.push(CapabilityEntry {
                kind: "role",
                namespace: "roles".to_string(),
                id: card.name.clone(),
                description: format!("{}｜模型 {}｜{}", card.label, card.model_id, card.scope_hints),
                extra_text: card.persona_prompt.chars().take(200).collect(),
                load_hint: format!(
                    "派发 child_worker/child_writer/child_reviewer 时传 role: \"{}\"",
                    card.name
                ),
            });
        }
    }

    entries
}

fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '/' && !c.is_ascii())
        .filter(|s| !s.is_empty())
        .flat_map(|part| {
            // CJK 串无空格分词：整段作为 token 参与子串匹配，同时 ASCII 部分拆开
            part.to_lowercase()
                .split(|c: char| c.is_ascii() && !c.is_alphanumeric() && c != '_' && c != '/')
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn score_entry(entry: &CapabilityEntry, tokens: &[String], whole_query: &str) -> i64 {
    let id = entry.id.to_lowercase();
    let desc = entry.description.to_lowercase();
    let extra = entry.extra_text.to_lowercase();
    let ns = entry.namespace.to_lowercase();
    let mut score = 0i64;

    if !whole_query.is_empty() && id.contains(whole_query) {
        score += 60;
    }
    for token in tokens {
        if id == *token {
            score += 100;
        } else if id.contains(token.as_str()) {
            score += 40;
        }
        if desc.contains(token.as_str()) {
            score += 12;
        }
        if extra.contains(token.as_str()) {
            score += 6;
        }
        if ns.contains(token.as_str()) {
            score += 8;
        }
    }
    score
}

/// capability_search 的统一入口：按关键词在全部能力源中检索。
/// 参数：query(string) 必填；kind? = builtin|mcp|skill|instruction|role；
/// namespace? = 插件名/目录名子串；limit? 默认 5，上限 15。
pub fn search(
    db: &DbState,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    params: &Value,
) -> DispatchResult {
    let query = params
        .get("query")
        .or_else(|| params.get("q"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return DispatchResult {
            view: None,
            stdout: String::new(),
            stderr: "capability_search 缺少 query 参数。\n💡 正确示例: {\"tool\": \"capability_search\", \"command\": {\"query\": \"文件搜索\"}}；可按 kind=mcp|skill|builtin|instruction|role 或 namespace=插件名 过滤。".to_string(),
            success: false,
            route: "capability_search:invalid".to_string(),
        };
    }
    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let namespace_filter = params
        .get("namespace")
        .or_else(|| params.get("plugin"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_SEARCH_RESULTS as u64)
        .clamp(1, MAX_SEARCH_RESULTS as u64) as usize;

    let entries = collect_entries(db, registry, workspace_root);
    let total_scanned = entries.len();
    let whole_query = query.to_lowercase();
    let tokens = tokenize(&query);

    let mut scored: Vec<(i64, &CapabilityEntry)> = entries
        .iter()
        .filter(|e| {
            kind_filter
                .as_deref()
                .map(|k| match k {
                    "mcp" => e.kind == "mcp_tool",
                    "builtin" => e.kind == "builtin",
                    "skill" => e.kind == "skill",
                    "instruction" | "custom" => e.kind == "instruction",
                    "role" => e.kind == "role",
                    _ => true,
                })
                .unwrap_or(true)
                && namespace_filter
                    .as_deref()
                    .map(|n| e.namespace.to_lowercase().contains(n))
                    .unwrap_or(true)
        })
        .map(|e| (score_entry(e, &tokens, &whole_query), e))
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));

    let matches: Vec<Value> = scored
        .iter()
        .take(limit)
        .map(|(score, e)| {
            json!({
                "id": e.id,
                "kind": e.kind,
                "namespace": e.namespace,
                "description": e.description.chars().take(DESCRIPTION_PREVIEW_CHARS).collect::<String>(),
                "load_hint": e.load_hint,
                "score": score,
            })
        })
        .collect();

    let note = if matches.is_empty() {
        "无命中。可换关键词（工具名/动作/中文描述均可），或去掉 kind/namespace 过滤再试。"
    } else {
        "命中项按相关性排序。Skill/MCP 工具用 load_hint 的 next_tool_hint 加载完整手册或 Schema；内置工具与角色已可直接使用。"
    };

    DispatchResult {
        view: None,
        stdout: serde_json::to_string_pretty(&json!({
            "query": query,
            "total_scanned": total_scanned,
            "matches": matches,
            "note": note,
        }))
        .unwrap_or_else(|_| "{}".to_string()),
        stderr: String::new(),
        success: true,
        route: "capability_search:search".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_custom_instructions (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                description TEXT, content TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE agent_role_cards (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, label TEXT NOT NULL DEFAULT '',
                model_id TEXT NOT NULL DEFAULT '', persona_prompt TEXT NOT NULL DEFAULT '',
                scope_hints TEXT NOT NULL DEFAULT '', default_test_command TEXT NOT NULL DEFAULT '',
                allowed_kinds TEXT NOT NULL DEFAULT '', hard_scope TEXT NOT NULL DEFAULT '',
                max_concurrency INTEGER NOT NULL DEFAULT 0, enabled INTEGER NOT NULL DEFAULT 1,
                custom_schema TEXT NOT NULL DEFAULT '', focus_domains TEXT NOT NULL DEFAULT '',
                input_schema TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn tokenize_splits_ascii_and_keeps_cjk_runs() {
        let tokens = tokenize("File Search 文件检索");
        assert!(tokens.contains(&"file".to_string()));
        assert!(tokens.contains(&"search".to_string()));
        assert!(tokens.contains(&"文件检索".to_string()));
    }

    #[test]
    fn search_finds_builtin_tool_by_english_and_chinese() {
        let db = test_db();
        let registry = PluginRegistry::default();
        let res = search(&db, &registry, None, &json!({"query": "search"}));
        assert!(res.success);
        let out: Value = serde_json::from_str(&res.stdout).unwrap();
        let ids: Vec<&str> = out["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(ids.contains(&"search"), "应命中内置 search 工具: {:?}", ids);

        let res_cn = search(&db, &registry, None, &json!({"query": "网页正文"}));
        let out_cn: Value = serde_json::from_str(&res_cn.stdout).unwrap();
        assert!(out_cn["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "web_fetch"));
    }

    #[test]
    fn search_respects_kind_and_namespace_filters_and_limit() {
        let db = test_db();
        let registry = PluginRegistry::default();
        let all = search(&db, &registry, None, &json!({"query": "list"}));
        let builtins_only = search(
            &db,
            &registry,
            None,
            &json!({"query": "list", "kind": "builtin"}),
        );
        let b: Value = serde_json::from_str(&builtins_only.stdout).unwrap();
        assert!(b["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["kind"] == "builtin"));
        let capped = search(&db, &registry, None, &json!({"query": "a", "limit": 3}));
        let c: Value = serde_json::from_str(&capped.stdout).unwrap();
        assert!(c["matches"].as_array().unwrap().len() <= 3);
        let _ = all;
    }

    #[test]
    fn empty_query_is_rejected_with_guidance() {
        let db = test_db();
        let registry = PluginRegistry::default();
        let res = search(&db, &registry, None, &json!({}));
        assert!(!res.success);
        assert!(res.stderr.contains("query"));
    }
}
