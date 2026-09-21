//! 会话分叉：把一条会话的可见历史 + Agent 状态 + 产物账本复制为一个全新独立会话。
//!
//! 语义：
//! - `before_message_id = None`：整会话克隆——复制全部消息与最新 checkpoint。
//! - `before_message_id = Some(m)`：回合分叉——复制 m 之前的消息，并取
//!   `agent_checkpoint_history` 中锚点落在复制区间内的最近一份快照
//!   （即"m 那一轮开始之前"的世界状态）。m 的正文经 `prompt_seed` 返回，
//!   前端可回填输入框由用户改写后再开跑。
//!
//! 一致性规则：
//! - 所有复制行生成全新主键；跨表引用（run_id/output_id/resource_id/
//!   anchor_message_id 等）经同一张旧→新映射表翻译，绝不指向源会话行。
//! - 文本字段统一走 `rewrite`：源 session_id → 新 id、源会话 tmp 绝对路径 →
//!   新会话 tmp 路径、以及全部 id 映射的字面替换（id 均为 uuid，安全）。
//! - tmp 目录物理复制而非软链，两会话互不影响；源会话删除级联不会带走分叉产物。
//! - background_jobs 只复制终态行：running/pending 行是活进程的影子，
//!   进程无法随数据克隆，原样搬过去就是幽灵任务。

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct ForkReport {
    pub new_session_id: String,
    /// 新会话标题（"⑂ " 前缀 + 源标题），供前端直接开 tab。
    pub title: String,
    pub copied_messages: usize,
    pub copied_traces: usize,
    pub copied_steps: usize,
    pub copied_artifacts: usize,
    pub checkpoint_copied: bool,
    /// 分叉点消息正文（供前端回填输入框）；整会话克隆时为 None。
    pub prompt_seed: Option<String>,
    pub tmp_bytes_copied: u64,
    pub skipped_live_jobs: usize,
}

struct Rewrite {
    pairs: Vec<(String, String)>,
}

impl Rewrite {
    fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (old, new) in &self.pairs {
            if !old.is_empty() && out.contains(old.as_str()) {
                out = out.replace(old.as_str(), new);
            }
        }
        out
    }
}

/// 递归复制目录；返回复制字节数。目标目录已存在时先清空残留（仅当它是本次 fork 新建的）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    if !src.exists() {
        return Ok(0);
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("创建分叉 tmp 目录失败: {e}"))?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        let rd = std::fs::read_dir(&from).map_err(|e| format!("读取目录失败 {}: {e}", from.display()))?;
        for entry in rd.flatten() {
            let name = entry.file_name();
            let src_p = entry.path();
            let dst_p = to.join(&name);
            let ft = entry
                .file_type()
                .map_err(|e| format!("读取文件类型失败 {}: {e}", src_p.display()))?;
            if ft.is_symlink() {
                // 软链不解引用复制：目标可能指向会话外路径，跳过以守"会话内自闭合"语义
                continue;
            }
            if ft.is_dir() {
                std::fs::create_dir_all(&dst_p)
                    .map_err(|e| format!("创建目录失败 {}: {e}", dst_p.display()))?;
                stack.push((src_p, dst_p));
            } else {
                std::fs::copy(&src_p, &dst_p)
                    .map_err(|e| format!("复制文件失败 {}: {e}", src_p.display()))?;
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(total)
}

/// 把时间戳边界解析为消息 id：取 created_at <= ts 的最后一条消息。
/// 供前端"在时间线气泡上分叉"使用——时间线条目只有 run 时间戳，没有消息 id。
pub fn resolve_boundary_message(
    conn: &Connection,
    session_id: &str,
    timestamp_ms: i64,
) -> Result<Option<String>, String> {
    let target = chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .ok_or_else(|| "分叉时间戳非法".to_string())?;
    let mut stmt = conn
        .prepare("SELECT id, created_at FROM chat_messages WHERE session_id=?1 ORDER BY created_at ASC, id ASC")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut found = None;
    for (id, created_at) in rows {
        let parsed = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|d| d.with_timezone(&Utc));
        match parsed {
            Ok(dt) if dt <= target => found = Some(id),
            _ => break,
        }
    }
    Ok(found)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkPoint {
    /// 全量整会话克隆（全部消息与最新 checkpoint）
    Full,
    /// 截断到某条消息之前（不含该消息，该消息文本作为 prompt_seed 回填输入框，供用户修改后重问）
    BeforeMessage(String),
    /// 截断到某条消息为止（包含该消息，用于总结/AI回答处分叉，prompt_seed 为 None，以此回答为基准开新分支继续）
    UpToMessage(String),
    /// 截断到某个时间戳之前（按时间戳截断所有小于该时间戳的消息和回合，prompt_seed 直接由前端透传回填）
    BeforeTimestamp {
        timestamp_ms: i64,
        prompt_seed: Option<String>,
    },
}

/// 生产入口：按真实数据目录推导两侧 tmp 路径。
pub fn fork_session(
    conn: &mut Connection,
    src_session_id: &str,
    fork_point: ForkPoint,
) -> Result<ForkReport, String> {
    let new_session_id = Uuid::new_v4().to_string();
    let src_tmp = crate::agent::path_guard::get_session_tmp_dir(src_session_id);
    let dst_tmp = crate::agent::path_guard::get_session_tmp_dir(&new_session_id);
    fork_session_at(conn, src_session_id, fork_point, &new_session_id, &src_tmp, &dst_tmp)
}

/// 可注入 tmp 根的实现——测试必须注入临时目录，不得触真实数据目录。
fn fork_session_at(
    conn: &mut Connection,
    src_session_id: &str,
    fork_point: ForkPoint,
    new_session_id: &str,
    src_tmp: &Path,
    dst_tmp: &Path,
) -> Result<ForkReport, String> {
    let new_session_id = new_session_id.to_string();
    let now = Utc::now().to_rfc3339();

    // ---------- 只读阶段：源会话与消息边界 ----------
    let (title, model_id): (String, String) = conn
        .query_row(
            "SELECT title, model_id FROM chat_sessions WHERE id=?1",
            params![src_session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "源会话不存在".to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, role, content, created_at, model_id, attachments FROM chat_messages
             WHERE session_id=?1 ORDER BY created_at ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let messages: Vec<(String, String, String, String, Option<String>, Option<String>)> = stmt
        .query_map(params![src_session_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    let (boundary_idx, prompt_seed, boundary_ts) = match &fork_point {
        ForkPoint::Full => (messages.len(), None, None),
        ForkPoint::BeforeMessage(mid) => {
            let idx = messages
                .iter()
                .position(|m| &m.0 == mid)
                .ok_or_else(|| "分叉点消息不存在于该会话".to_string())?;
            let seed = Some(messages[idx].2.clone());
            let ts = Some(messages[idx].3.clone());
            (idx, seed, ts)
        }
        ForkPoint::UpToMessage(mid) => {
            let idx = messages
                .iter()
                .position(|m| &m.0 == mid)
                .ok_or_else(|| "分叉点消息不存在于该会话".to_string())?;
            let next_idx = idx + 1;
            let ts = if next_idx < messages.len() {
                Some(messages[next_idx].3.clone())
            } else {
                None
            };
            (next_idx, None, ts)
        }
        // 时间线条目时间戳≈该轮 run.started_at，而当轮用户消息的 created_at
        // 晚落库约 2s——按 target 直接切，当轮消息与 run 自然落在边界外；
        // prompt_seed 由前端透传用户原话，不再回查 chat_messages 猜锚点。
        ForkPoint::BeforeTimestamp {
            timestamp_ms,
            prompt_seed,
        } => {
            let target = chrono::DateTime::from_timestamp_millis(*timestamp_ms)
                .ok_or_else(|| "分叉时间戳非法".to_string())?;
            let idx = messages
                .iter()
                .position(|m| {
                    chrono::DateTime::parse_from_rfc3339(&m.3)
                        .map(|d| d.with_timezone(&Utc) >= target)
                        .unwrap_or(false)
                })
                .unwrap_or(messages.len());
            (idx, prompt_seed.clone(), Some(target.to_rfc3339()))
        }
    };
    let copied_msgs = &messages[..boundary_idx];

    // ---------- Pass A：收集需要重映射的 id ----------
    // (旧 id → 新 id)。先收齐再复制，保证所有文本字段引用翻译一致。
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut msg_map: HashMap<String, String> = HashMap::new();
    for m in copied_msgs {
        msg_map.insert(m.0.clone(), Uuid::new_v4().to_string());
    }
    id_map.extend(msg_map.iter().map(|(k, v)| (k.clone(), v.clone())));

    let in_boundary = |col: &str| -> String {
        match &boundary_ts {
            Some(ts) => format!(" AND {col} < '{ts}'"),
            None => String::new(),
        }
    };
    let collect_ids = |sql: String, conn: &Connection| -> Vec<String> {
        let mut s = conn.prepare(&sql).unwrap();
        s.query_map(params![src_session_id], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect()
    };
    // run_traces 用 started_at 作边界（run 启动晚于其用户消息）
    let trace_ids: Vec<String> = collect_ids(
        format!(
            "SELECT id FROM agent_run_traces WHERE session_id=?1{}",
            in_boundary("started_at")
        ),
        conn,
    );
    for id in &trace_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let artifact_ids: Vec<String> = collect_ids(
        format!(
            "SELECT id FROM agent_artifacts WHERE session_id=?1{}",
            in_boundary("created_at")
        ),
        conn,
    );
    for id in &artifact_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let log_ids: Vec<String> = collect_ids(
        format!(
            "SELECT output_id FROM agent_step_logs WHERE session_id=?1{}",
            in_boundary("created_at")
        ),
        conn,
    );
    for id in &log_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let resource_ids: Vec<String> = collect_ids(
        "SELECT resource_id FROM agent_task_resources WHERE session_id=?1".to_string(),
        conn,
    );
    for id in &resource_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let node_ids: Vec<String> = collect_ids(
        "SELECT id FROM agent_memory_nodes WHERE session_id=?1".to_string(),
        conn,
    );
    for id in &node_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let child_ids: Vec<String> = collect_ids(
        format!(
            "SELECT id FROM agent_child_runs WHERE parent_session_id=?1{}",
            in_boundary("started_at")
        ),
        conn,
    );
    for id in &child_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }
    let lesson_ids: Vec<String> = collect_ids(
        format!(
            "SELECT id FROM agent_lesson_recalls WHERE session_id=?1{}",
            in_boundary("created_at")
        ),
        conn,
    );
    for id in &lesson_ids {
        id_map.insert(id.clone(), Uuid::new_v4().to_string());
    }

    // ---------- 文本重写器 ----------
    let mut pairs: Vec<(String, String)> = vec![
        (src_session_id.to_string(), new_session_id.clone()),
        (
            src_tmp.to_string_lossy().to_string(),
            dst_tmp.to_string_lossy().to_string(),
        ),
    ];
    pairs.extend(id_map.iter().map(|(k, v)| (k.clone(), v.clone())));
    let rw = Rewrite { pairs };
    let map_or = |map: &HashMap<String, String>, v: &str, fallback: &str| -> String {
        map.get(v).cloned().unwrap_or_else(|| fallback.to_string())
    };

    // ---------- Pass B：事务内复制 ----------
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    // 会话本体 + 项目归属
    let fork_title = format!("⑂ {}", title);
    tx.execute(
        "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![new_session_id, fork_title, model_id, now, now, copied_msgs.len() as i64],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO agent_project_sessions (session_id, project_id, created_at)
         SELECT ?1, project_id, ?2 FROM agent_project_sessions WHERE session_id=?3",
        params![new_session_id, now, src_session_id],
    )
    .map_err(|e| e.to_string())?;

    // 消息
    for (old_id, role, content, created_at, msg_model, attachments) in copied_msgs {
        tx.execute(
            "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                msg_map[old_id],
                new_session_id,
                role,
                rw.apply(content),
                attachments.as_deref().map(|a| rw.apply(a)),
                created_at,
                msg_model
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    // checkpoint：分叉点→最近的历史锚点快照；整会话克隆→当前最新 checkpoint
    let is_point_fork = matches!(
        fork_point,
        ForkPoint::BeforeMessage(_) | ForkPoint::UpToMessage(_) | ForkPoint::BeforeTimestamp { .. }
    );
    let checkpoint_copied = if is_point_fork {
        let mut hs = tx
            .prepare(
                "SELECT anchor_message_id, phase, snapshot_json FROM agent_checkpoint_history
                 WHERE session_id=?1 ORDER BY id ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String)> = hs
            .query_map(params![src_session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(hs);
        let snap = rows
            .iter()
            .rev()
            .find(|(anchor, _, _)| msg_map.contains_key(anchor));
        if let Some((_, phase, sj)) = snap {
            tx.execute(
                "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![new_session_id, phase, rw.apply(sj), now, now],
            )
            .map_err(|e| e.to_string())?;
            true
        } else if boundary_idx == messages.len() {
            let row = tx
                .query_row(
                    "SELECT phase, snapshot_json FROM agent_checkpoints WHERE run_id=?1",
                    params![src_session_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .ok();
            if let Some((phase, sj)) = row {
                tx.execute(
                    "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                    params![new_session_id, phase, rw.apply(&sj), now, now],
                )
                .map_err(|e| e.to_string())?;
                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        let row = tx
            .query_row(
                "SELECT phase, snapshot_json FROM agent_checkpoints WHERE run_id=?1",
                params![src_session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        if let Some((phase, sj)) = row {
            tx.execute(
                "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![new_session_id, phase, rw.apply(&sj), now, now],
            )
            .map_err(|e| e.to_string())?;
            true
        } else {
            false
        }
    };

    // 历史快照：锚点在复制区间内的整批复制（锚点 id 也要翻译）
    {
        let mut hs = tx
            .prepare(
                "SELECT anchor_message_id, phase, snapshot_json, created_at FROM agent_checkpoint_history
                 WHERE session_id=?1 ORDER BY id ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, String)> = hs
            .query_map(params![src_session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(hs);
        for (anchor, phase, sj, cat) in rows {
            if let Some(new_anchor) = msg_map.get(&anchor) {
                tx.execute(
                    "INSERT INTO agent_checkpoint_history (session_id, anchor_message_id, phase, snapshot_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![new_session_id, new_anchor, phase, rw.apply(&sj), cat],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    // 运行过程树
    let mut copied_traces = 0usize;
    {
        let mut ts = tx
            .prepare(
                "SELECT id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at
                 FROM agent_run_traces WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, String, Option<String>, String, String, Option<String>)> = ts
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(ts);
        for (tid, goal, status, steps_json, completion, started, updated, completed) in rows {
            if let Some(bts) = &boundary_ts {
                if started >= *bts {
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id_map[&tid],
                    new_session_id,
                    goal,
                    status,
                    rw.apply(&steps_json),
                    completion.as_deref().map(|c| rw.apply(c)),
                    started,
                    updated,
                    completed
                ],
            )
            .map_err(|e| e.to_string())?;
            copied_traces += 1;
        }
    }

    // 步骤行（按 trace id 归属复制）
    let mut copied_steps = 0usize;
    for old_trace_id in &trace_ids {
        let new_trace_id = id_map[old_trace_id].clone();
        let mut ss = tx
            .prepare("SELECT seq, step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String)> = ss
            .query_map(params![old_trace_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(ss);
        for (seq, sj) in rows {
            tx.execute(
                "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1, ?2, ?3)",
                params![new_trace_id, seq, rw.apply(&sj)],
            )
            .map_err(|e| e.to_string())?;
            copied_steps += 1;
        }
    }

    // step_logs + FTS
    {
        let mut ls = tx
            .prepare(
                "SELECT output_id, step_id, raw_output, created_at FROM agent_step_logs WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, i64, String, String)> = ls
            .query_map(params![src_session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(ls);
        for (oid, step_id, raw, cat) in rows {
            if let Some(bts) = &boundary_ts {
                if cat >= *bts {
                    continue;
                }
            }
            let new_oid = &id_map[&oid];
            let raw_new = rw.apply(&raw);
            tx.execute(
                "INSERT INTO agent_step_logs (output_id, session_id, step_id, raw_output, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![new_oid, new_session_id, step_id, raw_new, cat],
            )
            .map_err(|e| e.to_string())?;
            // FTS 列与 ensure_trigram_fts 建表保持一致（content 存重写后的输出）
            tx.execute(
                "INSERT INTO agent_step_logs_fts (output_id, session_id, step_id, tool, action, outcome, content)
                 SELECT ?1, ?2, ?3, tool, action, outcome, ?4 FROM agent_step_logs_fts WHERE output_id=?5",
                params![new_oid, new_session_id, step_id, raw_new, oid],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // 产物账本
    let mut copied_artifacts = 0usize;
    {
        let mut as_ = tx
            .prepare(
                "SELECT id, run_id, display_name, format, mime_type, kind, canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
                 FROM agent_artifacts WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, Option<String>, String, String, Option<String>, String, String, i64, Option<String>, String, String, String, String)> = as_
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                    r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?,
                    r.get(10)?, r.get(11)?, r.get(12)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(as_);
        for (aid, run_id, name, fmt, mime, kind, cpath, size, sha, origin, status, cat, uat) in rows {
            if let Some(bts) = &boundary_ts {
                if cat >= *bts {
                    continue;
                }
            }
            // run_id 指向未被复制的运行 → 置 NULL（诚实，不留指向源会话的悬挂引用）
            let new_run_id = run_id.as_deref().map(|r| map_or(&id_map, r, ""));
            tx.execute(
                "INSERT INTO agent_artifacts (id, session_id, run_id, display_name, format, mime_type, kind, canonical_path, size_bytes, sha256, origin, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    id_map[&aid],
                    new_session_id,
                    new_run_id.filter(|s| !s.is_empty()),
                    name,
                    fmt,
                    mime,
                    kind,
                    rw.apply(&cpath),
                    size,
                    sha,
                    origin,
                    status,
                    cat,
                    uat
                ],
            )
            .map_err(|e| e.to_string())?;
            copied_artifacts += 1;
        }
    }

    // 任务资源 + 记录
    {
        let mut rs = tx
            .prepare(
                "SELECT resource_id, kind, label, schema_json, summary, status, row_count, cursor_json, created_at, updated_at
                 FROM agent_task_resources WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, String, String, String, i64, Option<String>, String, String)> = rs
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                    r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(rs);
        for (rid, kind, label, schema, summary, status, rc, cursor, cat, uat) in rows {
            tx.execute(
                "INSERT INTO agent_task_resources (resource_id, session_id, kind, label, schema_json, summary, status, row_count, cursor_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id_map[&rid],
                    new_session_id,
                    kind,
                    label,
                    rw.apply(&schema),
                    summary,
                    status,
                    rc,
                    cursor.as_deref().map(|c| rw.apply(c)),
                    cat,
                    uat
                ],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO agent_task_records (resource_id, record_key, data_json, created_at, updated_at)
                 SELECT ?1, record_key, data_json, created_at, updated_at FROM agent_task_records WHERE resource_id=?2",
                params![id_map[&rid], rid],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // 子运行 / 后台任务（仅终态） / LLM 遥测 / 门禁遥测
    {
        let mut cs = tx
            .prepare(
                "SELECT id, parent_run_id, workspace_root, model_id, status, started_at, finished_at, duration_ms,
                        completion_tokens, evidence_ref_count, error_kind, result_json, kind, workspace_copy, resumed_from, message_history_json, consumed_at, applied_at
                 FROM agent_child_runs WHERE parent_session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, String, String, String, Option<String>, Option<i64>, i64, i64, Option<String>, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = cs
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                    r.get(12)?, r.get(13)?, r.get(14)?, r.get(15)?, r.get(16)?, r.get(17)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(cs);
        for (cid, prun, ws, mid, status, started, finished, dur, ctoks, erc, ekind, res, kind, wcopy, resumed, mhist, consumed, applied) in rows {
            if let Some(bts) = &boundary_ts {
                if started >= *bts {
                    continue;
                }
            }
            // applied_at 随行复制：已落账回执在分叉副本里不得重投
            // （重映射的 run_id 会使 reducer 的按 id 幂等失效，必须靠标记拦截）
            tx.execute(
                "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, finished_at, duration_ms, completion_tokens, evidence_ref_count, error_kind, result_json, kind, workspace_copy, resumed_from, message_history_json, consumed_at, applied_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    id_map[&cid],
                    map_or(&id_map, &prun, &new_session_id),
                    new_session_id,
                    ws,
                    mid,
                    status,
                    started,
                    finished,
                    dur,
                    ctoks,
                    erc,
                    ekind,
                    res.as_deref().map(|r| rw.apply(r)),
                    kind,
                    wcopy,
                    resumed,
                    mhist.as_deref().map(|m| rw.apply(m)),
                    consumed,
                    applied
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    let mut skipped_live_jobs = 0usize;
    {
        let mut js = tx
            .prepare(
                "SELECT id, parent_run_id, job_type, title, params_json, status, started_at, finished_at, duration_ms, result_json, error_kind, consumed
                 FROM agent_background_jobs WHERE parent_session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, String, String, String, String, Option<String>, Option<i64>, Option<String>, Option<String>, i64)> = js
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(js);
        for (_jid, prun, jtype, title_, params_json, status, started, finished, dur, res, ekind, consumed) in rows {
            if let Some(bts) = &boundary_ts {
                if started >= *bts {
                    continue;
                }
            }
            // 活进程克隆不过去——running/pending 行搬过去就是幽灵任务，跳过并计数
            if matches!(status.as_str(), "running" | "pending" | "queued") {
                skipped_live_jobs += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO agent_background_jobs (id, parent_session_id, parent_run_id, job_type, title, params_json, status, started_at, finished_at, duration_ms, result_json, error_kind, consumed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    Uuid::new_v4().to_string(),
                    new_session_id,
                    map_or(&id_map, &prun, &new_session_id),
                    jtype,
                    title_,
                    rw.apply(&params_json),
                    status,
                    started,
                    finished,
                    dur,
                    res.as_deref().map(|r| rw.apply(r)),
                    ekind,
                    consumed
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    tx.execute(
        "INSERT INTO agent_llm_calls (session_id, step_id, created_at, model_id, model_tier, reasoning_effort, route, retry_index, prompt_tokens, completion_tokens, total_tokens, cached_tokens, cache_hit_rate, context_window, usage_percent, history_message_count, fetched_tools_count, prefix_fingerprint, ttft_ms, total_duration_ms, session_call_index, request_kind, usage_source, outcome, request_try, transport_attempt_count)
         SELECT ?1, step_id, created_at, model_id, model_tier, reasoning_effort, route, retry_index, prompt_tokens, completion_tokens, total_tokens, cached_tokens, cache_hit_rate, context_window, usage_percent, history_message_count, fetched_tools_count, prefix_fingerprint, ttft_ms, total_duration_ms, session_call_index, request_kind, usage_source, outcome, request_try, transport_attempt_count
         FROM agent_llm_calls WHERE session_id=?2",
        params![new_session_id, src_session_id],
    )
    .map_err(|e| e.to_string())?;
    // 边界裁剪：整会话克隆时上面已全量复制；分叉点克隆删掉越界行
    if let Some(bts) = &boundary_ts {
        tx.execute(
            "DELETE FROM agent_llm_calls WHERE session_id=?1 AND created_at>=?2",
            params![new_session_id, bts],
        )
        .map_err(|e| e.to_string())?;
    }

    {
        let mut gs = tx
            .prepare("SELECT run_id, step_id, gate, route, created_at FROM agent_gate_fires WHERE session_id=?1")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, i64, String, String, String)> = gs
            .query_map(params![src_session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(gs);
        for (run_id, step_id, gate, route, cat) in rows {
            if let Some(bts) = &boundary_ts {
                if cat >= *bts {
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO agent_gate_fires (run_id, session_id, step_id, gate, route, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![map_or(&id_map, &run_id, &run_id), new_session_id, step_id, gate, route, cat],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // 记忆节点 + FTS（parent_id 自引用经映射表翻译）
    {
        let mut ns = tx
            .prepare(
                "SELECT id, parent_id, level, kind, plan_id, step_index, content, metadata_json, confidence_score, hit_count, created_at
                 FROM agent_memory_nodes WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, Option<String>, i64, String, Option<String>, Option<i64>, String, String, i64, i64, String)> = ns
            .query_map(params![src_session_id], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                    r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(ns);
        for (nid, parent, level, kind, plan, step, content, meta, conf, hits, cat) in rows {
            if let Some(bts) = &boundary_ts {
                if cat >= *bts {
                    continue;
                }
            }
            let new_nid = &id_map[&nid];
            let new_parent = parent.as_deref().and_then(|p| id_map.get(p).cloned());
            tx.execute(
                "INSERT INTO agent_memory_nodes (id, parent_id, level, kind, session_id, plan_id, step_index, content, metadata_json, confidence_score, hit_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    new_nid,
                    new_parent,
                    level,
                    kind,
                    new_session_id,
                    plan,
                    step,
                    rw.apply(&content),
                    rw.apply(&meta),
                    conf,
                    hits,
                    cat
                ],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO agent_memory_nodes_fts (node_id, session_id, level, kind, plan_id, content, trigger_keywords)
                 SELECT ?1, ?2, ?3, ?4, ?5, content, trigger_keywords FROM agent_memory_nodes_fts WHERE node_id=?6",
                params![new_nid, new_session_id, level, kind, plan, nid],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    {
        let mut ls2 = tx
            .prepare(
                "SELECT id, step_id, recalled_lesson_ids, applied_lesson_ids, outcome, created_at
                 FROM agent_lesson_recalls WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, i64, String, String, String, String)> = ls2
            .query_map(params![src_session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(ls2);
        for (lid, step_id, recalled, applied, outcome, cat) in rows {
            if let Some(bts) = &boundary_ts {
                if cat >= *bts {
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO agent_lesson_recalls (id, session_id, step_id, recalled_lesson_ids, applied_lesson_ids, outcome, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id_map[&lid], new_session_id, step_id, recalled, applied, outcome, cat],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    tx.commit().map_err(|e| format!("分叉事务提交失败: {e}"))?;

    // ---------- 文件系统：物理复制会话 tmp 目录 ----------
    let tmp_bytes = copy_dir_recursive(&src_tmp, &dst_tmp)?;

    Ok(ForkReport {
        new_session_id,
        title: fork_title,
        copied_messages: copied_msgs.len(),
        copied_traces,
        copied_steps,
        copied_artifacts,
        checkpoint_copied,
        prompt_seed,
        tmp_bytes_copied: tmp_bytes,
        skipped_live_jobs,
    })
}

#[cfg(test)]
mod tests;
