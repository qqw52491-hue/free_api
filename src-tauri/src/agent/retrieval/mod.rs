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
    unread_file_search_counts: std::collections::HashMap<String, usize>,
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
const SAME_FILE_SEARCH_HARD_LIMIT: usize = 3;

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
            if let Some(path) = step_normalized_path(step) {
                *idx.path_counts.entry(path).or_default() += 1;
            }
            if !is_cached_search_step(step) {
                if let Some(key) = step_gate_file_key(step) {
                    *idx.unread_file_search_counts.entry(key).or_default() += 1;
                }
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
        Some("read") => {
            if is_meaningful_file_read_step(step) {
                if let Some(key) = step_gate_file_key(step) {
                    idx.unread_file_search_counts.remove(&key);
                }
            }
        }
        // edit/write 步（不论成败）使此前的缓存命中失效——与原全量扫描的
        // INVALIDATING_ACTIONS 语义一致：cached 在写步之后清零。
        Some("edit") | Some("write") => {
            idx.last_write_seq = Some(idx.last_write_seq.map_or(seq, |w| w.max(seq)));
            idx.unread_file_search_counts.clear();
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
    // 取最新 run 必须按 started_at 排序：id 是 UUID v4，字典序与时间无关——
    // 实测曾把索引绑到"字典序最大但更早完成"的旧 run 上，当前 run 的步永不进
    // 索引，同参去重与同路径 signpost 双双静默失效。id DESC 仅作同刻决胜键。
    let run_id: String = match conn
        .query_row(
            "SELECT id FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at DESC, id DESC LIMIT 1",
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
            unread_file_search_counts: std::collections::HashMap::new(),
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
            unread_file_search_counts: std::collections::HashMap::new(),
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
        "SELECT steps_json FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at DESC, id DESC LIMIT 1",
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

fn step_normalized_path(step: &Value) -> Option<String> {
    step.get("params")
        .and_then(|p| p.get("path"))
        .and_then(Value::as_str)
        .map(normalize_search_path)
        .filter(|p| !p.is_empty())
}

/// 硬闸判定键：优先取持久化 view 里的 canonical 路径（search→resolved_root，
/// read→resolved_path），缺失时回退 params.path。raw 串 "./x" 与 "x" 同键化，
/// 杜绝改写路径拼写绕过计数或让 read 复位落空。
fn step_gate_file_key(step: &Value) -> Option<String> {
    step.pointer("/view/resolved_root")
        .or_else(|| step.pointer("/view/resolved_path"))
        .or_else(|| step.pointer("/params/path"))
        .and_then(Value::as_str)
        .map(normalize_search_path)
        .filter(|p| !p.is_empty())
}

/// 检索根解析：相对路径锚定会话工作区而非进程 CWD——与 dispatch_search
/// 主路径同一套解析，供硬闸键与 code 分支共用。
fn resolve_search_root(
    raw_path: &str,
    workspace_root: Option<&Path>,
) -> std::path::PathBuf {
    let raw = Path::new(raw_path);
    match (raw.is_absolute(), workspace_root) {
        (false, Some(ws)) => std::path::PathBuf::from(ws).join(raw),
        _ => std::path::PathBuf::from(raw),
    }
}

fn is_cached_search_step(step: &Value) -> bool {
    step.get("view")
        .and_then(|v| v.get("kind"))
        .and_then(Value::as_str)
        == Some("search-cache")
        || step
            .get("output")
            .and_then(Value::as_str)
            .map(|o| o.contains("【检索去重】"))
            .unwrap_or(false)
}

fn is_meaningful_file_read_step(step: &Value) -> bool {
    step.get("action").and_then(Value::as_str) == Some("read")
        && step.get("status").and_then(Value::as_str) == Some("succeeded")
        && step
            .get("view")
            .and_then(|v| v.get("kind"))
            .and_then(Value::as_str)
            == Some("read")
}

fn indexed_unread_file_search_count(session_id: &str, normalized_path: &str) -> Option<usize> {
    let map = search_step_index().lock().ok()?;
    map.get(session_id).map(|idx| {
        idx.unread_file_search_counts
            .get(normalized_path)
            .copied()
            .unwrap_or(0)
    })
}

fn count_unread_file_searches(steps: &[Value], gate_key: &str) -> usize {
    let gate_key = normalize_search_path(gate_key);
    if gate_key.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    for step in steps.iter().rev() {
        match step.get("action").and_then(Value::as_str) {
            Some("edit") | Some("write") => break,
            Some("read") => {
                if is_meaningful_file_read_step(step)
                    && step_gate_file_key(step).as_deref() == Some(gate_key.as_str())
                {
                    break;
                }
            }
            Some("search") => {
                if step.get("status").and_then(Value::as_str) == Some("succeeded")
                    && !is_cached_search_step(step)
                    && step_gate_file_key(step).as_deref() == Some(gate_key.as_str())
                {
                    count += 1;
                }
            }
            _ => {}
        }
    }
    count
}

fn search_read_required_result(path: &str, count: usize) -> DispatchResult {
    DispatchResult {
        view: Some(json!({
            "kind": "search-read-required",
            "path": path,
            "searches_since_read": count,
            "required_tool": "local_file",
            "required_action": "read"
        })),
        stdout: String::new(),
        stderr: format!("❌ [search→read 硬闸] 文件「{}」自上次有效读取后已执行 {} 次不同检索，检索通道已暂停。请从已有 file:line 命中选择目标区块，调用 local_file(action=\"read\", path=\"{}\", start_line=..., end_line=...) 一次读取完整窗口（建议锚点 ±15 行）；读取成功后自动解除。不要换关键词或改写同一路径继续探测。", path, count, path),
        success: false,
        route: "host_rejection:search_read_required".to_string(),
    }
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

    // 同文件硬闸的判定键：仅当 code 检索路径解析后确为单文件才建键
    // （目录检索与无效路径不进闸）；canonical 化使 ./x 与绝对路径同键。
    // 必须在去重缓存命中之前判定——已达阈值后同参复读命中缓存仍会烧轮次，
    // 且缓存行不计数，硬闸须先于一切"成功回执"拦截。
    let gate_file: Option<(String, String)> = if wants_code {
        params
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .and_then(|raw_path| {
                let root = resolve_search_root(raw_path, workspace_root);
                root.is_file().then(|| {
                    let canonical = root
                        .canonicalize()
                        .unwrap_or_else(|_| root.clone())
                        .to_string_lossy()
                        .to_string();
                    (raw_path.to_string(), normalize_search_path(&canonical))
                })
            })
    } else {
        None
    };

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
                if let Some((raw_path, key)) = gate_file.as_ref() {
                    let unread =
                        indexed_unread_file_search_count(session_id, key).unwrap_or(0);
                    if unread >= SAME_FILE_SEARCH_HARD_LIMIT {
                        return search_read_required_result(raw_path, unread);
                    }
                }
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
            if let Some((raw_path, key)) = gate_file.as_ref() {
                let unread = count_unread_file_searches(&steps, key);
                if unread >= SAME_FILE_SEARCH_HARD_LIMIT {
                    return search_read_required_result(raw_path, unread);
                }
            }
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
                let root = resolve_search_root(raw_path, workspace_root);
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

    // 命中集中度引导：≥3 条 code 命中落在同一文件时，显式点名"该文件的完整
    // 区块请用 local_file 窗口精读"——实测模型曾拿换词探测当逐行翻页器，
    // 单文件被搜 45 次只为还原一段 12 行模板。
    {
        let mut file_hit_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for h in all_hits.iter().filter(|h| h.kind == "code") {
            let file = h
                .id_or_path
                .rsplit_once(':')
                .map(|(f, _)| f)
                .unwrap_or(h.id_or_path.as_str());
            *file_hit_counts.entry(file).or_default() += 1;
        }
        if let Some((file, n)) = file_hit_counts
            .iter()
            .max_by_key(|(_, n)| *n)
            .filter(|(_, n)| **n >= 3)
        {
            notes.push(format!(
                "命中集中度提示：{} 条命中均位于「{}」。若需要该处完整结构（组件属性/函数体/模板块），请 local_file(action=read, start_line/end_line) 按行号窗口一次取回，勿换关键词对同一文件反复探测。",
                n, file
            ));
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
mod tests;