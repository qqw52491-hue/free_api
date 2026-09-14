pub mod fts_engine;
pub mod memory_signpost;
pub mod types;
pub mod workspace_search;

pub use fts_engine::*;
pub use memory_signpost::*;
pub use types::*;
pub use workspace_search::*;

use crate::agent::types::DispatchResult;
use crate::db::DbState;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

/// kinds 标量归一化（"code" → ["code"]），从 dispatch_search 提取以便指纹复用
fn normalize_scalar_kinds(params_value: &Value) -> Value {
    match params_value {
        Value::Object(map) => {
            let mut new_map = map.clone();
            if let Some(Value::String(k_str)) = new_map.get("kinds") {
                let parts: Vec<Value> = k_str
                    .split(',')
                    .map(|s| Value::String(s.trim().to_string()))
                    .filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                    .collect();
                new_map.insert("kinds".to_string(), Value::Array(parts));
            }
            Value::Object(new_map)
        }
        other => other.clone(),
    }
}

/// 归一化参数的稳定指纹（经 SearchParams 结构体往返，键序确定）。
/// None = 参数无法解析为合法检索（不可比对）。
fn canonical_search_fingerprint(params_value: &Value) -> Option<String> {
    let normalized = normalize_scalar_kinds(params_value);
    let parsed: SearchParams = serde_json::from_value(normalized).ok()?;
    serde_json::to_string(&parsed).ok()
}

/// 会话级检索步索引：steps 改为逐行持久化（agent_run_steps）后，
/// 去重/同路径统计不再整载 steps_json 做 O(n) 反序列化——同步只解析
/// seq ≥ next_seq 的新行（O(delta)），索引在 run 生命周期内常驻内存。
/// 语义与原全量扫描严格等价：edit/write 使之前的缓存命中失效，
/// 同路径统计只计成功 search 步。
struct SessionSearchIndex {
    run_id: String,
    next_seq: i64,
    searches: Vec<IndexedSearch>,
    last_write_seq: Option<i64>,
    path_counts: std::collections::HashMap<String, usize>,
    last_accessed: std::time::Instant,
}

struct IndexedSearch {
    seq: i64,
    step_id: i64,
    fingerprint: Option<String>,
    output_id: Option<String>,
    output_nonempty: bool,
}

const MAX_CACHED_SESSION_INDEXES: usize = 128;

static SEARCH_STEP_INDEX: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, SessionSearchIndex>>,
> = std::sync::OnceLock::new();

fn search_step_index(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, SessionSearchIndex>> {
    SEARCH_STEP_INDEX
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 显式清理会话的检索缓存，在会话被物理删除时调用避免内存泄漏
pub fn clear_session_search_index(session_id: &str) {
    if let Ok(mut map) = search_step_index().lock() {
        map.remove(session_id);
    }
}

fn index_step(idx: &mut SessionSearchIndex, seq: i64, step: &Value) {
    match step.get("action").and_then(Value::as_str) {
        Some("search") => {
            if step.get("status").and_then(Value::as_str) != Some("succeeded") {
                return;
            }
            if let Some(path) = step
                .get("params")
                .and_then(|p| p.get("path"))
                .and_then(Value::as_str)
                .map(normalize_search_path)
                .filter(|p| !p.is_empty())
            {
                *idx.path_counts.entry(path).or_default() += 1;
            }
            idx.searches.push(IndexedSearch {
                seq,
                step_id: step.get("id").and_then(Value::as_i64).unwrap_or(0),
                fingerprint: step.get("params").and_then(canonical_search_fingerprint),
                output_id: step
                    .get("output_id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string()),
                output_nonempty: step
                    .get("output")
                    .and_then(Value::as_str)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
            });
        }
        // edit/write 步（不论成败）使此前的缓存命中失效——与原全量扫描的
        // INVALIDATING_ACTIONS 语义一致：cached 在写步之后清零。
        Some("edit") | Some("write") => {
            idx.last_write_seq = Some(idx.last_write_seq.map_or(seq, |w| w.max(seq)));
        }
        _ => {}
    }
}

/// 将本会话最新 run 的 agent_run_steps 增量并入索引。
/// 返回 false 表示行表为空（旧数据/直接播种 steps_json 的测试），
/// 调用方应回退 load_latest_run_steps 的整载路径。
/// 注意锁序：调用方先持有 db reader（可能是 db.0 降级互斥），本函数再取
/// 索引锁——全程只有 db→index 单向次序，不存在反向持锁路径。
fn sync_search_index(conn: &rusqlite::Connection, session_id: &str) -> bool {
    let run_id: String = match conn
        .query_row(
            "SELECT id FROM agent_run_traces WHERE session_id=?1 ORDER BY id DESC LIMIT 1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    {
        Some(id) => id,
        None => return false,
    };
    let has_rows = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_run_steps WHERE run_id=?1)",
            params![run_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    let mut map = match search_step_index().lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !has_rows {
        map.remove(session_id);
        return false;
    }
    if map.len() >= MAX_CACHED_SESSION_INDEXES && !map.contains_key(session_id) {
        if let Some(oldest_key) = map
            .iter()
            .filter(|(k, _)| k.as_str() != session_id)
            .min_by_key(|(_, v)| v.last_accessed)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_key);
        }
    }
    let now = std::time::Instant::now();
    let idx = map.entry(session_id.to_string()).or_insert_with(|| {
        SessionSearchIndex {
            run_id: run_id.clone(),
            next_seq: 0,
            searches: Vec::new(),
            last_write_seq: None,
            path_counts: std::collections::HashMap::new(),
            last_accessed: now,
        }
    });
    idx.last_accessed = now;
    if idx.run_id != run_id {
        *idx = SessionSearchIndex {
            run_id: run_id.clone(),
            next_seq: 0,
            searches: Vec::new(),
            last_write_seq: None,
            path_counts: std::collections::HashMap::new(),
            last_accessed: now,
        };
    }
    let mut stmt = match conn.prepare(
        "SELECT seq, step_json FROM agent_run_steps WHERE run_id=?1 AND seq>=?2 ORDER BY seq ASC",
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let rows: Vec<(i64, String)> = stmt
        .query_map(params![run_id, idx.next_seq], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .ok()
        .map(|mapped| mapped.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    drop(stmt);
    for (seq, raw) in rows {
        if let Ok(step) = serde_json::from_str::<Value>(&raw) {
            index_step(idx, seq, &step);
        }
        idx.next_seq = seq + 1;
    }
    true
}

/// 索引版 find_cached_search：最近一次同参且非空输出的成功检索，
/// 其 seq 须大于最后一次 edit/write（等价原扫描"写步后 cached 清零"）。
/// 命中时按 seq 单行回取 step_json 拿 output 文本（O(1) 解析单步）。
fn indexed_find_cached_search(
    conn: &rusqlite::Connection,
    session_id: &str,
    fp: &str,
) -> Option<(i64, String, Option<String>)> {
    let map = search_step_index().lock().ok()?;
    let idx = map.get(session_id)?;
    let candidate = idx.searches.iter().rev().find(|s| {
        s.fingerprint.as_deref() == Some(fp)
            && s.output_nonempty
            && s.seq > idx.last_write_seq.unwrap_or(-1)
    })?;
    let raw: String = conn
        .query_row(
            "SELECT step_json FROM agent_run_steps WHERE run_id=?1 AND seq=?2",
            params![idx.run_id, candidate.seq],
            |r| r.get(0),
        )
        .ok()?;
    let step = serde_json::from_str::<Value>(&raw).ok()?;
    let out = step
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if out.is_empty() {
        return None;
    }
    Some((candidate.step_id, out, candidate.output_id.clone()))
}

/// 索引版同路径计数；无索引（回退路径）返回 None，调用方改走整载统计。
fn indexed_same_path_count(session_id: &str, normalized_path: &str) -> Option<usize> {
    let map = search_step_index().lock().ok()?;
    map.get(session_id)
        .and_then(|idx| idx.path_counts.get(normalized_path).copied().or(Some(0)))
}

/// 载入本会话最新 run trace 的 steps_json；无快照或解析失败返回空 Vec。
/// 精确指纹去重与同路径检索计数共用同一份快照，避免一次检索两次反序列化。
/// 仅作为行表为空时的回退路径（旧快照、直接播种 steps_json 的测试）。
fn load_latest_run_steps(conn: &rusqlite::Connection, session_id: &str) -> Vec<Value> {
    conn.query_row(
        "SELECT steps_json FROM agent_run_traces WHERE session_id=?1 ORDER BY id DESC LIMIT 1",
        params![session_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|raw| serde_json::from_str::<Vec<Value>>(&raw).ok())
    .unwrap_or_default()
}

/// 检索路径归一化：去首尾空白与末尾斜杠，使 "./src" 与 "src/" 计数同口径。
fn normalize_search_path(path: &str) -> String {
    path.trim().trim_end_matches('/').to_string()
}

/// 在 steps 快照中定位最近一次同参成功检索（期间有 edit/write 则缓存失效）。
/// 返回 (step_id, 原输出文本, output_id)。
fn find_cached_search(
    steps: &[Value],
    fp: &str,
) -> Option<(i64, String, Option<String>)> {
    const INVALIDATING_ACTIONS: &[&str] = &["edit", "write"];
    let mut cached: Option<(i64, String, Option<String>)> = None;
    for s in steps {
        match s.get("action").and_then(Value::as_str) {
            Some("search") => {
                if s.get("status").and_then(Value::as_str) != Some("succeeded") {
                    continue;
                }
                let same = s
                    .get("params")
                    .and_then(canonical_search_fingerprint)
                    .map(|p| p == fp)
                    .unwrap_or(false);
                if same {
                    let id = s.get("id").and_then(Value::as_i64).unwrap_or(0);
                    let out = s
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let out_id = s
                        .get("output_id")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(|v| v.to_string());
                    if !out.is_empty() {
                        cached = Some((id, out, out_id));
                    }
                }
            }
            other => {
                if cached.is_some()
                    && other.map(|a| INVALIDATING_ACTIONS.contains(&a)).unwrap_or(false)
                {
                    cached = None;
                }
            }
        }
    }
    cached
}

/// 同参命中后的去重回执：只投「锚点 + output_id 句柄」，绝不二次灌全量
/// hits——全量回放在历史上等于把物理去重省下的 IO 原样烧回 Token；
/// 句柄指向 agent_step_logs 持久化原文，compaction 脱水后仍可经
/// task_store.get_step_output / search_step_output 取回。
fn cached_search_dispatch_result(
    conn: &rusqlite::Connection,
    session_id: &str,
    step_id: i64,
    output: String,
    mut output_id: Option<String>,
) -> DispatchResult {
    // 补 output_id 字段前的旧快照回退：按会话+步号从 agent_step_logs 取句柄
    if output_id.is_none() {
        output_id = conn
            .query_row(
                "SELECT output_id FROM agent_step_logs WHERE session_id=?1 AND step_id=?2 ORDER BY created_at DESC LIMIT 1",
                params![session_id, step_id],
                |r| r.get::<_, String>(0),
            )
            .ok();
    }

    let mut anchor = format!(
        "🎯【检索去重】本会话第 {} 步已执行完全同参的检索且期间无文件修改（未重新检索）。",
        step_id
    );
    let mut top_paths: Vec<String> = Vec::new();
    let mut total_hits = 0u64;
    if let Ok(payload) = serde_json::from_str::<Value>(&output) {
        total_hits = payload
            .get("total_hits")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        anchor.push_str(&format!(" 共 {} 条命中，完整结果已在上文呈现。", total_hits));
        top_paths = payload
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|hits| {
                hits.iter()
                    .filter_map(|h| h.get("id_or_path").and_then(Value::as_str))
                    .take(3)
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !top_paths.is_empty() {
            anchor.push_str(&format!(" 核心命中: {}。", top_paths.join(", ")));
        }
    } else {
        anchor.push_str(" 完整结果已在上文呈现。");
    }
    match &output_id {
        Some(id) => anchor.push_str(&format!(
            " 如需复核请用 task_store.search_step_output(output_id=\"{}\", pattern=...) 定向检索或 get_step_output 分页回查，切勿重复发起同参检索；建议直接基于上述命中推进分析，或对命中文件做定向行范围读取。",
            id
        )),
        None => anchor.push_str(
            " 请勿以改换关键词/路径约束的方式变相重搜；建议直接基于上文已呈现的结果推进分析，或对命中文件做定向行范围读取。"
        ),
    }

    let view = json!({
        "kind": "search-cache",
        "step_id": step_id,
        "output_id": output_id,
        "total_hits": total_hits,
        "top_paths": top_paths,
    });
    DispatchResult { view: Some(view),
        stdout: anchor,
        stderr: String::new(),
        success: true,
        route: "retrieval-cache".to_string(),
    }
}

/// 统计本会话此前在同一路径下已完成的成功检索次数（不限 query）。
/// 精确指纹去重只拦"完全同参复读"；"换词重搜同一目录"（实测同一路径
/// 反复检索数十次）参数不同会绕过——供放行后注入软引导 signpost。
/// 入参与快照路径双边归一化，调用方无需自行 trim。
fn count_prior_same_path_searches(steps: &[Value], path: &str) -> usize {
    let normalized_path = normalize_search_path(path);
    if normalized_path.is_empty() {
        return 0;
    }
    steps
        .iter()
        .filter(|s| {
            s.get("action").and_then(Value::as_str) == Some("search")
                && s.get("status").and_then(Value::as_str) == Some("succeeded")
                && s.get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(Value::as_str)
                    .map(|p| normalize_search_path(p) == normalized_path)
                    .unwrap_or(false)
        })
        .count()
}

/// 统一联邦检索调度入口
pub fn dispatch_search(
    db: &DbState,
    session_id: &str,
    params_value: &Value,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    // 语法层归一化：无歧义自动适配大模型常见的标量字符串输入（如 "kinds": "code" 自动转为 ["code"]）
    let normalized_value = normalize_scalar_kinds(params_value);

    let params: SearchParams = match serde_json::from_value(normalized_value) {
        Ok(p) => p,
        Err(e) => {
            if let Some(q) = params_value.as_str() {
                SearchParams {
                    query: q.to_string(),
                    kinds: None,
                    outcome: None,
                    since_step: None,
                    path: None,
                    limit: Some(10),
                }
            } else {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!(
                        "❌ [search] 参数格式解析失败: {}\n💡 正确调用示例:\n  - 检索代码: {{\"query\": \"关键词\", \"kinds\": [\"code\"], \"path\": \".\"}}\n  - 检索历史: {{\"query\": \"关键词\", \"kinds\": [\"history\"]}}",
                        e
                    ),
                    success: false,
                    route: "retrieval".to_string(),
                };
            }
        }
    };

    let limit = params.limit.unwrap_or(3).min(10);
    let kinds = params
        .kinds
        .clone()
        .unwrap_or_else(|| vec!["history".to_string()]);

    let mut all_hits = Vec::new();
    let mut errors = Vec::new();
    let mut notes = Vec::new();

    let trimmed_query = params.query.trim();
    if !trimmed_query.is_empty() && trimmed_query.chars().count() < 3 {
        notes.push(
            "查询词少于 3 个字符：trigram 分词下限为 3 字符，请输入更长的子串（如「数据库连接」而非「库」）"
                .to_string(),
        );
    }

    let wants_history = kinds.iter().any(|k| k == "history" || k == "all");
    let wants_code = kinds.iter().any(|k| k == "code" || k == "all");

    // 会话内去重：同参成功检索直接复用（edit/write 会使其失效，见 find_cached_search）。
    // 快路径走每会话步索引（agent_run_steps 增量同步，只解析新行）；
    // 行表为空（旧快照/测试直接播种 steps_json）回退整载——两条路径语义等价。
    // conn 作用域必须收窄到此块内：reader() 在连接池缺省时降级为 db.0 同一把
    // 互斥锁，跨块存活会让下方 history 分支的 db.0.lock() 自锁死。
    let prior_steps = {
        let conn = db.reader().ok();
        let mut fallback_steps: Vec<Value> = Vec::new();
        let mut indexed = false;
        if let Some(conn) = conn.as_ref() {
            if sync_search_index(conn, session_id) {
                indexed = true;
                if let Some(fp) = canonical_search_fingerprint(params_value) {
                    if let Some((step_id, output, out_id)) =
                        indexed_find_cached_search(conn, session_id, &fp)
                    {
                        return cached_search_dispatch_result(
                            conn, session_id, step_id, output, out_id,
                        );
                    }
                }
            }
        }
        if !indexed {
            let steps = conn
                .as_ref()
                .map(|c| load_latest_run_steps(c, session_id))
                .unwrap_or_default();
            if let (Some(conn), Some(fp)) =
                (conn.as_ref(), canonical_search_fingerprint(params_value))
            {
                if let Some((step_id, output, out_id)) = find_cached_search(&steps, &fp) {
                    return cached_search_dispatch_result(
                        conn, session_id, step_id, output, out_id,
                    );
                }
            }
            fallback_steps = steps;
        }
        fallback_steps
    };

    let mut history_ok = !wants_history;
    if wants_history {
        if let Ok(conn) = db.0.lock() {
            let mut history_branch_hits = Vec::new();
            if let Ok(hits) = search_history(
                &conn,
                session_id,
                &params.query,
                params.outcome.as_deref(),
                params.since_step,
                limit,
            ) {
                history_branch_hits.extend(hits);
            }

            // 联动查询三级记忆节点库 (Level 0~2 知识、Episode 与 Lesson)
            if let Ok(mem_hits) =
                search_memory_nodes_enriched(&conn, session_id, &params.query, None, limit)
            {
                history_branch_hits.extend(mem_hits);
            }

            // 去重并限制返回条数
            let mut seen_ids = std::collections::HashSet::new();
            for hit in history_branch_hits {
                if seen_ids.insert(hit.id_or_path.clone()) {
                    all_hits.push(hit);
                    if all_hits.len() >= limit {
                        break;
                    }
                }
            }
            history_ok = true;
        } else {
            errors.push("history 检索失败: 数据库锁获取失败".to_string());
        }
    }

    let mut code_ok = !wants_code;
    let mut resolved_root: Option<String> = None;
    if wants_code {
        match params
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            None => {
                errors.push("❌ [search] code 检索需要显式 path 参数（要检索的目录或文件路径，如 path: \".\" 或 path: \"src\"）。\n💡 正确示例: {\"query\": \"关键词\", \"kinds\": [\"code\"], \"path\": \".\"}".to_string());
            }
            Some(raw_path) => {
                // 【工作区隔离】相对路径以当前会话工作区为根解析，而不是进程 CWD——
                // 评测隔离工作区下按 CWD 解析会让模型检索到宿主项目目录（实测教训：
                // deepseek 实弹曾据此把宿主 docs/前端样式翻了个遍，预算烧在错误工作区）。
                let raw = Path::new(raw_path);
                let root = match (raw.is_absolute(), workspace_root) {
                    (false, Some(ws)) => std::path::PathBuf::from(ws).join(raw),
                    _ => std::path::PathBuf::from(raw),
                };
                if !root.is_dir() && !root.is_file() {
                    errors.push(format!(
                        "path 不是有效目录或文件: {}（已按当前工作区解析为 {}，不存在；注意 src/ 目录可能尚未生成，先运行 npm test 让宿主创建题目文件）",
                        raw_path,
                        root.display()
                    ));
                } else {
                    // 把真实解析出的检索根（绝对路径）投影进结果：资源作用域契约
                    // 依据它判定 inside/outside，相对路径不再永远落入 unknown。
                    resolved_root = Some(
                        root.canonicalize()
                            .unwrap_or_else(|_| root.clone())
                            .to_string_lossy()
                            .to_string(),
                    );
                    match search_code(&root, &params.query, limit) {
                        Ok((hits, literal_fallback)) => {
                            if literal_fallback {
                                notes.push(
                                    "query 含无效正则（如未转义的 ( 或 {），已自动降级为字面量匹配；如需正则语义请转义元字符"
                                        .to_string(),
                                );
                            }
                            all_hits.extend(hits);
                            code_ok = true;
                        }
                        Err(e) => errors.push(format!("code 检索失败: {}", e)),
                    }
                }
            }
        }
    }

    // 同路径重搜 signpost：同参指纹只拦完全复读；"换词重搜同一目录"参数不同
    // 会绕过（实测同一路径反复检索数十次）——放行后统计同路径历史检索次数，
    // ≥2 次即注入软引导，把模型推向定向读取/工件内检索而非继续盲搜。
    if let Some(path) = params
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let prior = indexed_same_path_count(session_id, &normalize_search_path(path))
            .unwrap_or_else(|| count_prior_same_path_searches(&prior_steps, path));
        if prior >= 2 {
            notes.push(format!(
                "检索 signpost：路径「{}」本会话此前已成功检索 {} 次（含不同关键词），换词重搜同一路径收益递减——建议改用 local_file 定向读取已命中文件，或用 task_store.search_step_output 在既有输出的 output_id 上做工件内检索。若剩余探索面横跨多个模块/目录，优先 child_worker dispatches 一步批量并行派发只读侦察，比主循环串行搜索省步数。",
                path, prior
            ));
        }
    }

    let success = history_ok && code_ok;

    let mut result_payload = json!({
        "query": params.query,
        "total_hits": all_hits.len(),
        "hits": all_hits
    });
    if let Some(root) = resolved_root {
        result_payload["resolved_root"] = json!(root);
    }
    if !errors.is_empty() {
        result_payload["errors"] = json!(errors);
    }
    if !notes.is_empty() {
        result_payload["notes"] = json!(notes);
    }

    // UI 结构化视图：模型拿 pretty JSON（上文 result_payload），前端拿类型化卡片数据。
    // 同一事实两路投影——hits 截断到 12 条、snippet 收敛到 160 字符防卡片膨胀。
    let view = json!({
        "kind": "search",
        "query": params.query,
        "resolved_root": result_payload.get("resolved_root").cloned().unwrap_or(Value::Null),
        "total_hits": all_hits.len(),
        "truncated": all_hits.len() > 12,
        "hits": all_hits.iter().take(12).map(|h| json!({
            "kind": h.kind,
            "path": h.id_or_path,
            "snippet": h.snippet.chars().take(160).collect::<String>(),
        })).collect::<Vec<_>>(),
        "notes": notes,
        "errors": errors,
    });

    DispatchResult { view: Some(view),
        stdout: serde_json::to_string_pretty(&result_payload)
            .unwrap_or_else(|_| result_payload.to_string()),
        stderr: if errors.is_empty() {
            String::new()
        } else {
            errors.join("\n")
        },
        success,
        route: "retrieval".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_count_prior_same_path_searches_normalizes_and_filters() {
        let steps = vec![
            json!({"id": 1, "action": "search", "status": "succeeded", "params": {"path": "src/context", "query": "a"}}),
            json!({"id": 2, "action": "search", "status": "succeeded", "params": {"path": "src/context/", "query": "b"}}),
            json!({"id": 3, "action": "search", "status": "failed", "params": {"path": "src/context", "query": "c"}}),
            json!({"id": 4, "action": "search", "status": "succeeded", "params": {"path": "src/other", "query": "a"}}),
            json!({"id": 5, "action": "read", "status": "succeeded", "params": {"path": "src/context"}}),
            json!({"id": 6, "action": "search", "status": "succeeded", "params": {"kinds": ["history"], "query": "x"}}),
        ];
        // 末尾斜杠归一化、failed 与非 search 步骤不计、无 path 的历史检索不计
        assert_eq!(count_prior_same_path_searches(&steps, "src/context"), 2);
        assert_eq!(count_prior_same_path_searches(&steps, "src/context/"), 2);
        assert_eq!(count_prior_same_path_searches(&steps, "src/other"), 1);
        assert_eq!(count_prior_same_path_searches(&steps, ""), 0);
    }

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(
                output_id UNINDEXED,
                session_id UNINDEXED,
                step_id UNINDEXED,
                tool,
                action,
                outcome,
                content,
                tokenize='trigram'
            );

            CREATE TABLE agent_memory_nodes (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                level INTEGER NOT NULL,
                kind TEXT NOT NULL,
                session_id TEXT NOT NULL,
                plan_id TEXT,
                step_index INTEGER,
                content TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                confidence_score INTEGER NOT NULL DEFAULT 100,
                hit_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE agent_memory_nodes_fts USING fts5(
                node_id UNINDEXED,
                session_id UNINDEXED,
                level UNINDEXED,
                kind UNINDEXED,
                plan_id UNINDEXED,
                content,
                trigger_keywords,
                tokenize='trigram'
            );
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_fts5_indexing_and_history_search() {
        let conn = setup_test_db();

        index_step_fts(
            &conn,
            "out-001",
            "session-test",
            1,
            "shell_exec",
            "cargo build",
            "failed",
            "error[E0425]: cannot find value `db_conn` in this scope in src/db.rs:42",
        )
        .unwrap();

        index_step_fts(
            &conn,
            "out-002",
            "session-test",
            2,
            "write_file",
            "fix db.rs",
            "succeeded",
            "Updated src/db.rs with correct connection pool",
        )
        .unwrap();

        // 1. 搜索报错关键词
        let hits = search_history(&conn, "session-test", "db_conn", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-001");
        assert_eq!(hits[0].outcome.as_deref(), Some("failed"));

        // 2. 按 outcome="failed" 过滤（大小写不敏感归一）
        let failed_hits =
            search_history(&conn, "session-test", "src", Some("FAILED"), None, 10).unwrap();
        assert_eq!(failed_hits.len(), 1);
        assert_eq!(failed_hits[0].id_or_path, "out-001");

        // 3. 搜索成功记录
        let succ_hits =
            search_history(&conn, "session-test", "pool", Some("succeeded"), None, 10).unwrap();
        assert_eq!(succ_hits.len(), 1);
        assert_eq!(succ_hits[0].id_or_path, "out-002");

        // 4. since_step 步骤下界过滤
        let later_hits = search_history(&conn, "session-test", "db", None, Some(2), 10).unwrap();
        assert!(later_hits.iter().all(|h| h.step_id.unwrap_or(0) >= 2));
    }

    #[test]
    fn test_chinese_substring_search_with_trigram() {
        let conn = setup_test_db();

        index_step_fts(
            &conn,
            "out-cn-001",
            "session-cn",
            1,
            "browser_click",
            "点击提交按钮",
            "failed",
            "错误: 无法连接到数据库服务器，请检查网络配置",
        )
        .unwrap();

        let hits = search_history(&conn, "session-cn", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1, "中文子串「服务器」必须可命中");
        assert_eq!(hits[0].id_or_path, "out-cn-001");
        assert!(hits[0].snippet.contains("服务器"));

        let hits2 = search_history(&conn, "session-cn", "网络配置", None, None, 10).unwrap();
        assert_eq!(hits2.len(), 1, "中文子串「网络配置」必须可命中");

        let short = search_history(&conn, "session-cn", "网络", None, None, 10).unwrap();
        assert_eq!(short.len(), 1, "2 字短中文「网络」应通过 LIKE 降级命中");
        assert_eq!(short[0].id_or_path, "out-cn-001");
    }

    #[test]
    fn test_session_isolation() {
        let conn = setup_test_db();
        for (out_id, session) in [("out-a", "session-a"), ("out-b", "session-b")] {
            index_step_fts(
                &conn,
                out_id,
                session,
                1,
                "shell_exec",
                "cargo test",
                "failed",
                "linking error openssl not found",
            )
            .unwrap();
        }

        let hits = search_history(&conn, "session-a", "openssl", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-a");

        let all = search_history(&conn, "", "openssl", None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_shallow_three_tier_memory_and_context_enrichment() {
        let conn = setup_test_db();

        let summary_node = MemoryNode {
            id: "phase-1".to_string(),
            parent_id: None,
            level: 2,
            kind: "summary".to_string(),
            session_id: "sess-tier".to_string(),
            plan_id: Some("plan-1".to_string()),
            step_index: Some(1),
            content: "阶段总结: 完成 PostgreSQL 驱动替换并修复跨域问题".to_string(),
            metadata_json: "{}".to_string(),
            confidence_score: 100,
            hit_count: 0,
            created_at: "2026-08-17".to_string(),
        };
        insert_memory_node(&conn, &summary_node, "PostgreSQL, 跨域, 阶段总结").unwrap();

        let event_node = MemoryNode {
            id: "step-10".to_string(),
            parent_id: Some("phase-1".to_string()),
            level: 0,
            kind: "event".to_string(),
            session_id: "sess-tier".to_string(),
            plan_id: Some("plan-1".to_string()),
            step_index: Some(10),
            content: "执行 cargo build 失败: error linking with cc failed: -lpq not found"
                .to_string(),
            metadata_json: "{}".to_string(),
            confidence_score: 100,
            hit_count: 0,
            created_at: "2026-08-17".to_string(),
        };
        insert_memory_node(&conn, &event_node, "linking error, lpq, PostgreSQL").unwrap();

        let hits = search_memory_nodes_enriched(&conn, "sess-tier", "lpq", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "step-10");
        assert!(
            !hits[0].evidence.is_empty(),
            "命中原始事件应自动带出父阶段总结"
        );
        assert!(hits[0].evidence[0].contains("PostgreSQL 驱动替换"));

        let summary_hits =
            search_memory_nodes_enriched(&conn, "sess-tier", "跨域问题", None, 10).unwrap();
        assert_eq!(summary_hits.len(), 1);
        assert_eq!(summary_hits[0].id_or_path, "phase-1");
        assert!(
            !summary_hits[0].evidence.is_empty(),
            "命中阶段总结应自动带出子证据"
        );
        assert!(summary_hits[0].evidence[0].contains("linking with cc failed"));
    }

    #[test]
    fn test_dispatch_search_defaults_and_error_reporting() {
        let conn = setup_test_db();
        index_step_fts(
            &conn,
            "out-d-1",
            "session-dispatch",
            1,
            "shell_exec",
            "cargo build",
            "failed",
            "错误: 无法连接到数据库服务器",
        )
        .unwrap();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));

        let result = dispatch_search(&db, "session-dispatch", &json!({"query": "数据库服务器"}), None);
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);

        let short = dispatch_search(&db, "session-dispatch", &json!({"query": "数据"}), None);
        assert!(short.success);
        let short_payload: Value = serde_json::from_str(&short.stdout).unwrap();
        assert_eq!(
            short_payload["total_hits"], 1,
            "2 字短中文「数据」应通过 LIKE 降级命中"
        );

        let code_missing = dispatch_search(
            &db,
            "session-dispatch",
            &json!({"query": "fn main", "kinds": ["code"]}),
            None,
        );
        assert!(!code_missing.success);
        assert!(code_missing.stderr.contains("path"));

        let str_query = dispatch_search(&db, "session-dispatch", &json!("数据库服务器"), None);
        assert!(str_query.success);
        let str_payload: Value = serde_json::from_str(&str_query.stdout).unwrap();
        assert_eq!(str_payload["total_hits"], 1);

        // view 结构化视图：与模型向文本同事实、两路投影
        let v = str_query.view.as_ref().expect("检索应产出结构化 view");
        assert_eq!(v["kind"], "search");
        assert_eq!(v["total_hits"], 1);
        assert!(v["hits"].as_array().unwrap()[0]["path"].is_string());
    }

    /// 行表索引路径的测试库：在 setup_test_db 基础上补 run_traces/run_steps/step_logs。
    fn setup_index_db() -> Connection {
        let conn = setup_test_db();
        conn.execute_batch(
            "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT NOT NULL,
                status TEXT NOT NULL, steps_json TEXT NOT NULL DEFAULT '[]',
                completion_json TEXT, started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL, completed_at TEXT);
             CREATE TABLE agent_run_steps (
                run_id TEXT NOT NULL, seq INTEGER NOT NULL, step_json TEXT NOT NULL,
                PRIMARY KEY (run_id, seq));
             CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL, raw_output TEXT NOT NULL, created_at TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    fn insert_run_step(conn: &Connection, run_id: &str, seq: i64, step: &Value) {
        conn.execute(
            "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1,?2,?3)",
            params![run_id, seq, serde_json::to_string(step).unwrap()],
        )
        .unwrap();
    }

    fn seed_trace(conn: &Connection, session_id: &str, run_id: &str) {
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES (?1,?2,'g','running','[]','2026-01-01','2026-01-01')",
            params![run_id, session_id],
        )
        .unwrap();
    }

    #[test]
    fn test_indexed_dedup_hit_and_incremental_sync() {
        let conn = setup_index_db();
        let session = "sess-idx-dedup";
        seed_trace(&conn, session, "run-i1");
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));

        // 第一次派发：行表无任何步 → 正常检索路径（索引同步成功但无命中）
        let params = json!({"query": "数据库连接池", "kinds": ["history"], "path": "src"});
        let first = dispatch_search(&db, session, &params, None);
        assert_eq!(first.route, "retrieval", "无历史步时应正常检索而非命中缓存");

        // 落一步"同参成功检索"行 → 第二次派发应经增量索引命中去重锚点
        {
            let conn = db.0.lock().unwrap();
            insert_run_step(
                &conn,
                "run-i1",
                0,
                &json!({
                    "id": 1, "action": "search", "status": "succeeded",
                    "params": params,
                    "output": "{\"query\":\"数据库连接池\",\"total_hits\":3,\"hits\":[]}",
                    "output_id": "out-dedup-1"
                }),
            );
        }
        let second = dispatch_search(&db, session, &params, None);
        assert_eq!(second.route, "retrieval-cache", "同参重搜应命中增量索引去重");
        assert!(second.stdout.contains("检索去重"));
        assert!(second.stdout.contains("out-dedup-1"), "锚点应携带 output_id 句柄");
    }

    #[test]
    fn test_indexed_dedup_invalidated_by_write_step() {
        let conn = setup_index_db();
        let session = "sess-idx-inval";
        seed_trace(&conn, session, "run-i2");
        let params = json!({"query": "配置项", "kinds": ["history"]});
        insert_run_step(
            &conn,
            "run-i2",
            0,
            &json!({
                "id": 1, "action": "search", "status": "succeeded",
                "params": params,
                "output": "{\"total_hits\":1}", "output_id": "o1"
            }),
        );
        // 写步使之前的缓存命中失效——与原全量扫描 INVALIDATING_ACTIONS 语义一致
        insert_run_step(
            &conn,
            "run-i2",
            1,
            &json!({"id": 2, "action": "edit", "status": "succeeded", "params": {}}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session, &params, None);
        assert_eq!(result.route, "retrieval", "写步之后的同参检索不得命中缓存");
    }

    #[test]
    fn test_indexed_same_path_signpost_counts() {
        let conn = setup_index_db();
        let session = "sess-idx-signpost";
        seed_trace(&conn, session, "run-i3");
        for (seq, id, q) in [(0, 1, "甲"), (1, 2, "乙")] {
            insert_run_step(
                &conn,
                "run-i3",
                seq,
                &json!({
                    "id": id, "action": "search", "status": "succeeded",
                    "params": {"query": q, "kinds": ["history"], "path": "src/context"},
                    "output": "{\"total_hits\":1}"
                }),
            );
        }
        // 干扰项：失败检索与异路径检索不应计数
        insert_run_step(
            &conn,
            "run-i3",
            2,
            &json!({"id": 3, "action": "search", "status": "failed",
                    "params": {"query": "丙", "path": "src/context"}}),
        );
        insert_run_step(
            &conn,
            "run-i3",
            3,
            &json!({"id": 4, "action": "search", "status": "succeeded",
                    "params": {"query": "丁", "path": "src/other"},
                    "output": "{\"total_hits\":1}"}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "戊", "kinds": ["history"], "path": "src/context"}),
            None,
        );
        assert!(
            result.stdout.contains("检索 signpost") && result.stdout.contains("2 次"),
            "索引路径的同路径计数应与整载一致，实际: {}",
            result.stdout
        );
    }

    #[test]
    fn test_indexed_empty_output_match_does_not_shadow_earlier_hit() {
        // 语义对齐 find_cached_search：空输出的同参匹配不覆盖更早的非空命中
        let conn = setup_index_db();
        let session = "sess-idx-emptyout";
        seed_trace(&conn, session, "run-i4");
        let params = json!({"query": "重试策略", "kinds": ["history"]});
        insert_run_step(
            &conn,
            "run-i4",
            0,
            &json!({"id": 7, "action": "search", "status": "succeeded",
                    "params": params, "output": "{\"total_hits\":2}", "output_id": "o7"}),
        );
        insert_run_step(
            &conn,
            "run-i4",
            1,
            &json!({"id": 8, "action": "search", "status": "succeeded",
                    "params": params, "output": ""}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session, &params, None);
        assert_eq!(result.route, "retrieval-cache");
        assert!(result.stdout.contains("第 7 步"), "应锚定更早的非空命中: {}", result.stdout);
    }

    #[test]
    fn test_trigram_migration_rebuilds_old_unicode61_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL, raw_output TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(
                output_id UNINDEXED, session_id UNINDEXED, step_id UNINDEXED,
                tool, action, outcome, content);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_step_logs VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "out-mig",
                "s-mig",
                1i64,
                "错误: 无法连接到数据库服务器",
                "2026-08-17"
            ],
        )
        .unwrap();

        assert!(search_history(&conn, "s-mig", "服务器", None, None, 10)
            .unwrap()
            .is_empty());

        let rebuilt = ensure_trigram_fts(&conn).unwrap();
        assert_eq!(rebuilt, 1, "应从主表回填 1 行");
        assert_eq!(ensure_trigram_fts(&conn).unwrap(), 0, "重复迁移应为 0 行");

        let hits = search_history(&conn, "s-mig", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-mig");
    }

    struct TempSearchDir(std::path::PathBuf);

    impl TempSearchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "free-api-search-{}-{}",
                tag,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempSearchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_code_search_accepts_file_path_and_reports_resolved_root() {
        let temp = TempSearchDir::new("file-path");
        let file = temp.0.join("sample_scope.rs");
        std::fs::write(&file, "pub fn observe_tool_result() {}\n").unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            "session-file-path",
            &json!({
                "query": "observe_tool_result",
                "kinds": ["code"],
                "path": file.to_str().unwrap(),
            }),
            None,
        );

        assert!(
            result.success,
            "文件路径应可作为 code 检索目标: {}",
            result.stderr
        );
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let hit_path = payload["hits"][0]["id_or_path"].as_str().unwrap();
        assert!(
            hit_path.starts_with("sample_scope.rs:"),
            "单文件检索的命中应使用文件名，实际: {}",
            hit_path
        );
        let resolved = payload["resolved_root"].as_str().unwrap();
        assert!(
            Path::new(resolved).is_absolute(),
            "resolved_root 必须是绝对路径，实际: {}",
            resolved
        );
    }

    #[test]
    fn test_code_search_regex_parse_error_falls_back_to_literal_match() {
        let temp = TempSearchDir::new("literal");
        std::fs::write(
            temp.0.join("parser.rs"),
            "fn parse_repetition() {\n    let q = 1;\n}\n",
        )
        .unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        // 含未转义元字符的查询在正则模式下必然 exit=2；应降级为字面量命中而不是失败。
        let result = dispatch_search(
            &db,
            "session-literal",
            &json!({
                "query": "fn parse_repetition() {",
                "kinds": ["code"],
                "path": temp.0.to_str().unwrap(),
            }),
            None,
        );

        assert!(result.success, "正则解析失败应降级重试: {}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let notes = payload["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().unwrap().contains("字面量")),
            "降级必须以 note 告知，实际: {:?}",
            notes
        );
    }

    #[test]
    fn test_code_search_reports_resolved_root_for_relative_directory() {
        let temp = TempSearchDir::new("rel-root");
        std::fs::write(temp.0.join("leaf.rs"), "pub struct Marker;\n").unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        // 相对目录：resolved_root 应解析为进程 cwd 下的绝对路径并出现在结果里，
        // 供资源作用域契约把此类访问判为 inside 而不是 unknown。
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp.0).unwrap();
        let result = dispatch_search(
            &db,
            "session-rel-root",
            &json!({
                "query": "Marker",
                "kinds": ["code"],
                "path": ".",
            }),
            None,
        );
        std::env::set_current_dir(original_cwd).unwrap();

        assert!(result.success, "相对目录检索应成功: {}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let resolved = payload["resolved_root"].as_str().unwrap();
        assert!(
            Path::new(resolved).is_absolute(),
            "相对路径必须投影出绝对 resolved_root，实际: {}",
            resolved
        );
    }
}
