//! Agent 可选的证据链索引。宿主只持久化可回查来源与观察事实；
//! 文本相似仅用于召回候选，绝不决定测试是否通过或自动跳过验证。
use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use uuid::Uuid;

const MAX_QUERY_RESULTS: usize = 30;
const FULL_HASH_THRESHOLD: u64 = 16 * 1024 * 1024;
const SAMPLE_CHUNK: u64 = 4 * 1024 * 1024;

/// 允许按 subject（含文件外对象）、purpose、command、标签检索；输出均是候选。
pub(crate) fn dispatch(
    db: &DbState,
    session_id: &str,
    action: &str,
    input: &Value,
    workspace: Option<&Path>,
) -> Result<Value, String> {
    match action {
        "evidence_record" => record(db, session_id, input, workspace),
        "evidence_find" => find(db, session_id, input),
        "evidence_inspect" => inspect(db, session_id, input),
        "evidence_compare" => compare(db, session_id, input, workspace),
        _ => Err(format!("未知证据工具操作: {action}")),
    }
}

fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn checked_text(v: &Value, key: &str, max: usize) -> Result<String, String> {
    if v.get(key)
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(format!("{key} 必须为字符串"));
    }
    let s = field(v, key);
    if s.chars().count() > max {
        return Err(format!("{key} 超过 {max} 字符"));
    }
    Ok(s.to_string())
}

/// 这里做的是检索别名，而非验证等价性。测试/测验/检测只是可能相关的候选。
fn normalize_search(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut s = lower.replace("单测", "单元测试");
    for term in ["测验", "检验", "校验", "检测", "验证", "检查"] {
        s = s.replace(term, "测试");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn match_score(rec: &Value, input: &Value) -> (i64, Vec<String>) {
    let subject = field(input, "subject");
    let command = field(input, "command");
    let query = field(input, "query");
    let mut score = 0;
    let mut reasons = Vec::new();
    let stored_subject = field(rec, "subject");
    let stored_path = field(rec, "file_path");
    let canonical_path = field(&rec["snapshot"], "path");
    let stored_command = field(rec, "command");
    if !subject.is_empty() {
        if stored_subject.eq_ignore_ascii_case(subject)
            || stored_path.eq_ignore_ascii_case(subject)
            || canonical_path.eq_ignore_ascii_case(subject)
        {
            score += 100;
            reasons.push("subject_exact".to_string());
        } else if normalize_search(stored_subject).contains(&normalize_search(subject))
            || normalize_search(stored_path).contains(&normalize_search(subject))
            || normalize_search(canonical_path).contains(&normalize_search(subject))
        {
            score += 48;
            reasons.push("subject_related".to_string());
        }
    }
    if !command.is_empty() {
        if stored_command.eq_ignore_ascii_case(command) {
            score += 85;
            reasons.push("command_exact".to_string());
        } else if normalize_search(stored_command).contains(&normalize_search(command)) {
            score += 32;
            reasons.push("command_related".to_string());
        }
    }
    if !query.is_empty() {
        let needle = normalize_search(query);
        let searchable = format!(
            "{} {} {} {} {} {}",
            field(rec, "subject"),
            field(rec, "purpose"),
            field(rec, "command"),
            stored_path,
            canonical_path,
            rec.get("tags").map(|v| v.to_string()).unwrap_or_default()
        );
        let hay = normalize_search(&searchable);
        if hay.contains(&needle) {
            score += 50;
            reasons.push("text_or_alias_match".to_string());
        } else {
            let terms: Vec<&str> = needle
                .split_whitespace()
                .filter(|t| !t.is_empty())
                .collect();
            let count = terms.iter().filter(|t| hay.contains(**t)).count();
            if count > 0 {
                score += (count * 10) as i64;
                reasons.push("partial_terms".to_string());
            }
            // 连续中文改写通常没有空格，补充双字片段召回。
            // 只是低分候选；相似字符串绝不作为 PASS 或自动选择的依据。
            let chars: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
            if chars.len() >= 3 && chars.len() <= 160 {
                let bigrams: HashSet<String> = chars
                    .windows(2)
                    .map(|w| w.iter().collect::<String>())
                    .collect();
                let overlaps = bigrams
                    .iter()
                    .filter(|gram| hay.contains(gram.as_str()))
                    .count();
                if overlaps >= 2 {
                    score += (overlaps * 3).min(21) as i64;
                    reasons.push("lexical_overlap_candidate".to_string());
                }
            }
        }
    }
    (score, reasons)
}

fn record(
    db: &DbState,
    session_id: &str,
    input: &Value,
    workspace: Option<&Path>,
) -> Result<Value, String> {
    let purpose = checked_text(input, "purpose", 400)?;
    if purpose.is_empty() {
        return Err("evidence_record 需要 purpose（验证目的）".into());
    }
    let subject = checked_text(input, "subject", 700)?;
    let command = checked_text(input, "command", 1500)?;
    let file_path = checked_text(input, "file_path", 1500)?;
    let note = checked_text(input, "note", 2000)?;
    let reported_outcome = checked_text(input, "reported_outcome", 100)?;
    let output_id = checked_text(input, "output_id", 200)?;
    let key = checked_text(input, "idempotency_key", 200)?;
    let tags = match input.get("tags") {
        None => Vec::<String>::new(),
        Some(Value::Array(items)) if items.len() <= 16 => {
            let mut tags = Vec::new();
            for item in items {
                let tag = item.as_str().ok_or("tags 中每项必须为字符串")?.trim();
                if tag.chars().count() > 60 {
                    return Err("tags 中单项超过 60 字符".into());
                }
                if !tag.is_empty() {
                    tags.push(tag.to_string());
                }
            }
            tags
        }
        _ => return Err("tags 必须为最多 16 个字符串组成的数组".into()),
    };
    if subject.is_empty() && command.is_empty() && file_path.is_empty() {
        return Err("至少提供 subject、command 或 file_path 之一（不要求文件路径）".into());
    }
    // 必须由宿主核对 output_id 会话归属，不接受 Agent 冒认其他会话的回执。
    let source = if output_id.is_empty() {
        Value::Null
    } else {
        crate::agent::run_trace::saved_output_source(db, session_id, &output_id)?
    };

    // 默认每次调用代表一次新观察。只有显式幂等键才表示重试同一次采集；
    // 参数相同不证明文件、环境或执行时刻相同。
    let identity = json!({"session":session_id,"purpose":purpose,"subject":subject,
        "command":command,"file_path":file_path,"output_id":output_id,"key":key,
        "tags":tags,"note":note,"reported_outcome":reported_outcome});
    let request_fingerprint = format!("{:x}", Sha256::digest(identity.to_string().as_bytes()));
    let id = if key.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            json!(["verification-evidence-v2", session_id, key])
                .to_string()
                .as_bytes(),
        )
        .to_string()
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    if let Some(existing) = load_owned(&conn, session_id, &id)? {
        check_retry_payload(&existing, &request_fingerprint)?;
        return Ok(json!({"evidence":existing,"reused_record":true}));
    }
    // 不在持有数据库锁时采样；并发重试由 INSERT OR IGNORE 保留首个已保存观察。
    drop(conn);
    let snapshot = if file_path.is_empty() {
        Value::Null
    } else {
        let root = workspace.ok_or("当前没有有效工作区，不能采集文件快照")?;
        file_snapshot(&file_path, root, session_id)?
    };
    let recorded_at = Utc::now().to_rfc3339();
    let rec = json!({
        "evidence_id":id,
        "request_fingerprint":request_fingerprint,
        "subject":subject,
        "purpose":purpose,
        "command":command,
        "file_path":file_path,
        "tags":tags,
        "note":note,
        "reported_outcome":if reported_outcome.is_empty(){"unknown"}else{reported_outcome.as_str()},
        "output_id":if output_id.is_empty(){ Value::Null }else{json!(output_id)},
        "output_source":source,
        "snapshot":snapshot,
        "recorded_at":recorded_at,
        "trust_notice":"subject/purpose/command/tags/note/reported_outcome 为 Agent 声明，不能代替原始命令与退出码；文件快照是登记时的宿主观测，不证明原始验证针对的就是该版本。recorded_at 不是原始验证时间。",
    });
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO agent_verification_evidence
            (id,session_id,subject,purpose,command,search_text,data_json,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id,
                session_id,
                subject,
                purpose,
                command,
                normalize_search(&format!(
                    "{subject} {purpose} {command} {file_path} {}",
                    tags.join(" ")
                )),
                rec.to_string(),
                recorded_at
            ],
        )
        .map_err(|e| e.to_string())?;
    let persisted = load_owned(&conn, session_id, &id)?.ok_or("保存证据后未找到记录")?;
    check_retry_payload(&persisted, &request_fingerprint)?;
    Ok(json!({"evidence":persisted,"reused_record":inserted == 0}))
}

fn check_retry_payload(existing: &Value, fingerprint: &str) -> Result<(), String> {
    if existing["request_fingerprint"].as_str() != Some(fingerprint) {
        return Err(
            "idempotency_key 已用于不同参数；本次未覆盖旧记录，请为新观察使用新键或省略键".into(),
        );
    }
    Ok(())
}

fn load_owned(
    conn: &rusqlite::Connection,
    session: &str,
    id: &str,
) -> Result<Option<Value>, String> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT data_json FROM agent_verification_evidence WHERE session_id=?1 AND id=?2",
            params![session, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
        .transpose()
}

/// 终审只注入轻量定位信息；登记时间不是原始验证时间，不自动采信 Agent 声明。
pub(crate) fn review_digest(
    db: &DbState,
    session: &str,
    since: chrono::DateTime<Utc>,
) -> Result<Option<String>, String> {
    let conn = db.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT id,subject,purpose,output_id,created_at FROM (
            SELECT id,subject,purpose,json_extract(data_json,'$.output_id') AS output_id,created_at
            FROM agent_verification_evidence WHERE session_id=?1
              AND julianday(created_at)>=julianday(?2)
        ) ORDER BY julianday(created_at) DESC,id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session, since.to_rfc3339()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut candidates = Vec::new();
    let mut has_more = false;
    let mut version = Sha256::new();
    for row in rows {
        let (id, subject, purpose, output_id, at) = row.map_err(|e| e.to_string())?;
        if !chrono::DateTime::parse_from_rfc3339(&at).is_ok_and(|at| at >= since) {
            continue;
        }
        // 展示可截断，但缓存身份覆盖全部不可变记录，不能漏掉同毫秒下未展示的新证据。
        version.update((id.len() as u64).to_le_bytes());
        version.update(id.as_bytes());
        if candidates.len() < 6 {
            candidates.push(json!({"evidence_id":id,"subject":subject,"purpose":purpose,"output_id":output_id,"recorded_at":at}));
        } else {
            has_more = true;
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let version = format!("{:x}", version.finalize());
    Ok(Some(format!(
        "\n\n【可选证据登记索引（has_more={has_more}，index_fingerprint={version}，不是验证通过证明）】\nsubject/purpose 为 Agent 声明；recorded_at 为登记时间，不是验证时间。登记后取得的快照不证明原始验证针对该版本。需要时用 evidence_inspect / evidence_compare 回查，原始输出用 read_step_output；检索遗漏不构成新增验收义务。\n{}",
        serde_json::to_string(&candidates).map_err(|e| e.to_string())?
    )))
}

fn inspect(db: &DbState, session: &str, input: &Value) -> Result<Value, String> {
    let id = field(input, "evidence_id");
    if id.is_empty() {
        return Err("evidence_inspect 需要 evidence_id".into());
    }
    let conn = db.reader()?;
    let e = load_owned(&conn, session, id)?.ok_or("证据不存在或不属于当前会话")?;
    Ok(
        json!({"evidence":e,"note":"如需核实实际执行输出，请使用 task_store.get_step_output(output_id)。"}),
    )
}

fn find(db: &DbState, session: &str, input: &Value) -> Result<Value, String> {
    let subject = checked_text(input, "subject", 700)?;
    let command = checked_text(input, "command", 1500)?;
    let query = checked_text(input, "query", 400)?;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, MAX_QUERY_RESULTS as u64) as usize;
    let conn = db.reader()?;
    let mut stmt=conn.prepare(
        "SELECT data_json FROM agent_verification_evidence WHERE session_id=?1 ORDER BY created_at DESC,id DESC"
    ).map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(params![session], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let any_filter = !subject.is_empty() || !command.is_empty() || !query.is_empty();
    let mut matches: Vec<(i64, Value)> = Vec::new();
    for row in rows {
        let raw = row.map_err(|e| e.to_string())?;
        let Ok(rec) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let (score, reasons) = match_score(&rec, input);
        if any_filter && score == 0 {
            continue;
        }
        matches.push((
            score,
            json!({
                "evidence_id":rec["evidence_id"],
                "subject":rec["subject"],
                "purpose":rec["purpose"],
                "command":rec["command"],
                "file_path":rec["file_path"],
                "reported_outcome":rec["reported_outcome"],
                "output_id":rec["output_id"],
                "output_source":rec["output_source"],
                "recorded_at":rec["recorded_at"],
                "has_snapshot":!rec["snapshot"].is_null(),
                "candidate_score":score,
                "match_reasons":reasons
            }),
        ));
    }
    matches.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            b.1["recorded_at"]
                .as_str()
                .cmp(&a.1["recorded_at"].as_str())
        })
    });
    let total = matches.len();
    let candidates: Vec<Value> = matches.into_iter().take(limit).map(|(_, r)| r).collect();
    Ok(
        json!({"candidates":candidates,"total_matches":total,"has_more":total>limit,
        "matching_is_not_verification":true,
        "note":"召回匹配（包含测试/测验等同义词）只用于定位候选。reported_outcome/command 为 Agent 声明；recorded_at 不是验证时间。候选可能是失败/旧环境/旧产物，必须由 Agent 依据完整回执和当前条件判断。"}),
    )
}

fn compare(
    db: &DbState,
    session: &str,
    input: &Value,
    workspace: Option<&Path>,
) -> Result<Value, String> {
    let id = field(input, "evidence_id");
    if id.is_empty() {
        return Err("evidence_compare 需要 evidence_id；先 evidence_find".into());
    }
    let conn = db.reader()?;
    let rec = load_owned(&conn, session, id)?.ok_or("证据不存在或不属于当前会话")?;
    drop(conn);
    let old = &rec["snapshot"];
    if old.is_null() {
        return Ok(json!({"evidence_id":id,"status":"not_applicable",
            "reason":"此证据没有文件快照；请结合 Git 历史、执行回执或环境信息进行语义评估，不能据此自动 PASS。"}));
    }
    let root = workspace.ok_or("没有有效工作区，无法执行当前文件对比")?;
    if old.get("digest_format").is_some_and(|format| {
        !matches!(
            format.as_str(),
            Some("sha256_content_v1" | "sha256_ranges_le64_v1")
        )
    }) {
        return Ok(json!({"evidence_id":id, "status":"inconclusive",
            "reason":"历史快照的摘要格式不受支持，未比较当前文件，不能据此判断内容变化"}));
    }
    // 重开记录时解析到的对象，避免切换工作区后把相同相对路径当成同一产物。
    let path = old["path"]
        .as_str()
        .unwrap_or_else(|| field(&rec, "file_path"));
    let legacy_digest = old.get("digest_format").is_none();
    let current = file_snapshot_with_format(path, root, session, legacy_digest)?;
    let keys = [
        "size",
        "modified_ns",
        "changed_ns",
        "device",
        "inode",
        "sample_sha256",
        "sampling",
    ];
    let mut changed = Vec::new();
    let mut unknown = Vec::new();
    for k in keys {
        if old.get(k).is_none_or(Value::is_null) || current.get(k).is_none_or(Value::is_null) {
            unknown.push(k);
        } else if old[k] != current[k] {
            changed.push(k);
        }
    }
    let stable = old["stable_during_capture"] == true && current["stable_during_capture"] == true;
    // Windows 上 ctime / inode / device 可能缺失：如实披露，但不把平台
    // 没有的字段当成内容发生变化。使用实际获得的摘要、大小、mtime 等判断。
    let status = if !stable {
        "inconclusive"
    } else if !changed.is_empty() {
        "change_detected"
    } else {
        "no_change_detected"
    };
    Ok(
        json!({"evidence_id":id,"status":status,"changed_fields":changed,
        "unknown_fields":unknown,"before":old,"now":current,
        "limitations":"元数据和抽样 SHA-256 一致不证明整个文件逐字节一致，亦不代表之前测试仍适用；不自动放行。"}),
    )
}

fn file_snapshot(raw: &str, workspace: &Path, session: &str) -> Result<Value, String> {
    file_snapshot_with_format(raw, workspace, session, false)
}

fn file_snapshot_with_format(
    raw: &str,
    workspace: &Path,
    session: &str,
    legacy_digest: bool,
) -> Result<Value, String> {
    let allowed =
        crate::agent::path_guard::ensure_readable_path(raw, workspace, Some(session), false)?;
    let path = allowed.canonical_path;
    if !std::fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("目前只支持普通文件快照（目录/特殊设备不会被读取）".into());
    }
    let capture_started_at = Utc::now().to_rfc3339();
    let mut file = std::fs::File::open(&path).map_err(|e| format!("读取对象失败: {e}"))?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("目前只支持普通文件快照（目录/特殊设备不会被读取）".into());
    }
    let size = before.len();
    let full = size <= FULL_HASH_THRESHOLD;
    let mut sha = Sha256::new();
    let mut ranges = Vec::new();
    if full {
        ranges.push((0, size));
    } else {
        ranges.push((0, SAMPLE_CHUNK.min(size)));
        let mid = size / 2;
        ranges.push((mid.saturating_sub(SAMPLE_CHUNK / 2), SAMPLE_CHUNK));
        ranges.push((size - SAMPLE_CHUNK, SAMPLE_CHUNK));
    }
    for (offset, length) in &ranges {
        // 全文件模式输出标准文件 SHA-256，便于与 sha256sum 等外部工具对照。
        // 旧记录曾对全文件也添加范围头，仅在比较旧记录时保留其历史算法。
        if !full || legacy_digest {
            sha.update(offset.to_le_bytes());
            sha.update(length.to_le_bytes());
        }
        file.seek(SeekFrom::Start(*offset))
            .map_err(|e| e.to_string())?;
        let mut buf = [0u8; 64 * 1024];
        let mut left = *length;
        while left > 0 {
            let chunk_size = buf.len().min(left as usize);
            let n = file
                .read(&mut buf[..chunk_size])
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("读取文件快照时发生截断，无法登记有效采样".into());
            }
            sha.update(&buf[..n]);
            left -= n as u64;
        }
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    let old_meta = file_metadata(&before);
    let new_meta = file_metadata(&after);
    let path_still_matches =
        std::fs::metadata(&path).is_ok_and(|metadata| file_metadata(&metadata) == new_meta);
    let stable = old_meta == new_meta && path_still_matches;
    Ok(
        json!({"path":path.to_string_lossy(),"size":new_meta["size"],
            "modified_ns":new_meta["modified_ns"],"changed_ns":new_meta["changed_ns"],
            "inode":new_meta["inode"],"device":new_meta["device"],
            "sampling":if full {"full_sha256"}else{"head_middle_tail_sha256"},
            "digest_format":if full && !legacy_digest {"sha256_content_v1"}else{"sha256_ranges_le64_v1"},
            "sample_ranges":ranges.iter().map(|(offset,length)| json!({"offset":offset,"length":length})).collect::<Vec<_>>(),
            "sampled_bytes":ranges.iter().map(|(_,n)|n).sum::<u64>(),
            "sample_sha256":format!("{:x}",sha.finalize()),
            "stable_during_capture":stable,
            "capture_started_at":capture_started_at,
            "observed_at":Utc::now().to_rfc3339(),
            "warning":if full {"完整文件内容摘要，但仅能证明采样时所见内容"}
                else {"仅包含首中尾区域，不是全文件内容完整性证明"}
        }),
    )
}

fn ns(t: std::io::Result<std::time::SystemTime>) -> Value {
    t.ok()
        .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| json!(d.as_nanos().to_string()))
        .unwrap_or(Value::Null)
}
fn file_metadata(m: &std::fs::Metadata) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let change = (m.ctime() as i128) * 1_000_000_000 + (m.ctime_nsec() as i128);
        json!({"size":m.len(),"modified_ns":ns(m.modified()),
            "changed_ns":change.to_string(),"inode":m.ino(),"device":m.dev()})
    }
    #[cfg(not(unix))]
    {
        json!({"size":m.len(),"modified_ns":ns(m.modified()),
            "changed_ns":Value::Null,"inode":Value::Null,"device":Value::Null})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn memory_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE agent_verification_evidence(
            id TEXT PRIMARY KEY,session_id TEXT NOT NULL,subject TEXT NOT NULL,
            purpose TEXT NOT NULL,command TEXT NOT NULL,search_text TEXT NOT NULL,
            data_json TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE INDEX idx_agent_evidence_session ON agent_verification_evidence(session_id,created_at DESC);").unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }
    #[test]
    fn synonym_recall_and_non_file_records() {
        let db = memory_db();
        let p = json!({"subject":"Rust 单元测试","purpose":"软件测试通过性",
            "command":"cargo test --lib","reported_outcome":"claimed_passed"});
        let created = dispatch(&db, "a", "evidence_record", &p, None).unwrap();
        assert_eq!(created["evidence"]["reported_outcome"], "claimed_passed");
        let found = dispatch(
            &db,
            "a",
            "evidence_find",
            &json!({"query":"单元测验"}),
            None,
        )
        .unwrap();
        assert_eq!(found["total_matches"], 1);
        assert_eq!(
            found["candidates"][0]["match_reasons"][0],
            "text_or_alias_match"
        );
        assert_eq!(found["matching_is_not_verification"], true);
        assert_eq!(
            dispatch(
                &db,
                "b",
                "evidence_find",
                &json!({"query":"单元测验"}),
                None
            )
            .unwrap()["total_matches"],
            0
        );
        let id = created["evidence"]["evidence_id"].as_str().unwrap();
        let inspected = dispatch(
            &db,
            "a",
            "evidence_inspect",
            &json!({"evidence_id":id}),
            None,
        )
        .unwrap();
        assert_eq!(inspected["evidence"]["command"], "cargo test --lib");
        assert!(dispatch(
            &db,
            "b",
            "evidence_inspect",
            &json!({"evidence_id":id}),
            None
        )
        .is_err());
        let no_file = dispatch(
            &db,
            "a",
            "evidence_compare",
            &json!({"evidence_id":id}),
            None,
        )
        .unwrap();
        assert_eq!(no_file["status"], "not_applicable");
        let again = dispatch(&db, "a", "evidence_record", &p, None).unwrap();
        assert_eq!(again["reused_record"], false);
        assert_ne!(
            again["evidence"]["evidence_id"],
            created["evidence"]["evidence_id"]
        );
    }
    #[test]
    fn task_store_dispatch_can_find_evidence_without_a_file_path() {
        let db = memory_db();
        let recorded = crate::agent::task_store::dispatch(
            &db,
            "s",
            "evidence_record",
            &json!({"subject":"服务接口回归","purpose":"API 测试",
                "command":"pytest tests/api", "reported_outcome":"unknown"}),
        );
        assert!(recorded.success, "{}", recorded.stderr);
        let found = crate::agent::task_store::dispatch(
            &db,
            "s",
            "evidence_find",
            &json!({"query":"API 测验"}),
        );
        assert!(found.success, "{}", found.stderr);
        let value: Value = serde_json::from_str(&found.stdout).unwrap();
        assert_eq!(value["total_matches"], 1);
        assert_eq!(value["matching_is_not_verification"], true);
    }

    #[test]
    fn similar_words_are_candidates_not_automatic_pass() {
        let db = memory_db();
        dispatch(
            &db,
            "s",
            "evidence_record",
            &json!({"subject":"API 可靠性检验",
            "purpose":"部署环境检查","reported_outcome":"failed"}),
            None,
        )
        .unwrap();
        let found = dispatch(
            &db,
            "s",
            "evidence_find",
            &json!({"query":"API 可靠性测试"}),
            None,
        )
        .unwrap();
        assert_eq!(found["total_matches"], 1);
        assert_eq!(found["candidates"][0]["reported_outcome"], "failed");
        assert_eq!(found["matching_is_not_verification"], true);
    }
    #[test]
    fn loose_chinese_phrase_can_recall_without_forcing_exact_terms() {
        let db = memory_db();
        dispatch(
            &db,
            "s",
            "evidence_record",
            &json!({
                "subject":"Rust 单元测试",
                "purpose":"发布前检查",
                "command":"cargo test --lib"
            }),
            None,
        )
        .unwrap();
        let found = dispatch(
            &db,
            "s",
            "evidence_find",
            &json!({"query":"之前进行过单元测验吗"}),
            None,
        )
        .unwrap();
        assert_eq!(found["total_matches"], 1);
        assert!(found["candidates"][0]["match_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "lexical_overlap_candidate"));
        assert_eq!(found["matching_is_not_verification"], true);
    }

    #[test]
    fn output_ref_must_belong_to_same_session() {
        let db = memory_db();
        {
            let conn = db.0.lock().unwrap();
            conn.execute_batch("CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY,session_id TEXT,step_id INTEGER,
                raw_output TEXT,created_at TEXT
            );
            CREATE TABLE agent_step_logs_fts (
                output_id TEXT,session_id TEXT,step_id INTEGER,tool TEXT,action TEXT,outcome TEXT
            );
            INSERT INTO agent_step_logs VALUES ('real-step','owner',3,'exit 0','2026-10-15T00:00:00Z');
            INSERT INTO agent_step_logs_fts VALUES ('real-step','owner',3,'local_shell','exec','succeeded');
            ").unwrap();
        }
        let input = json!({"subject":"单元测试","purpose":"测试回执",
            "output_id":"real-step","reported_outcome":"claimed_passed"});
        assert!(dispatch(&db, "intruder", "evidence_record", &input, None).is_err());
        let saved = dispatch(&db, "owner", "evidence_record", &input, None).unwrap();
        assert_eq!(saved["evidence"]["output_source"]["outcome"], "succeeded");
        assert_eq!(saved["evidence"]["output_source"]["step_id"], 3);
    }

    #[test]
    fn large_file_sampling_reads_only_bounded_regions() {
        let dir = std::env::temp_dir().join(format!("ev-large-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(17 * 1024 * 1024).unwrap();
        let snapshot = file_snapshot("large.bin", &dir, "s").unwrap();
        assert_eq!(snapshot["sampling"], "head_middle_tail_sha256");
        assert_eq!(snapshot["sampled_bytes"], SAMPLE_CHUNK * 3);
        assert_eq!(snapshot["stable_during_capture"], true);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn file_snapshot_changed_and_equal_are_non_authoritative() {
        let db = memory_db();
        let dir = std::env::temp_dir().join(format!("evs_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"sample").unwrap();
        let rec = dispatch(
            &db,
            "s",
            "evidence_record",
            &json!({
            "subject":"a.bin","purpose":"文件内容采样","file_path":"a.bin"}),
            Some(&dir),
        )
        .unwrap();
        let id = rec["evidence"]["evidence_id"].as_str().unwrap();
        let same = dispatch(
            &db,
            "s",
            "evidence_compare",
            &json!({"evidence_id":id}),
            Some(&dir),
        )
        .unwrap();
        assert_eq!(same["status"], "no_change_detected");
        assert!(same["limitations"].as_str().unwrap().contains("不自动放行"));
        std::fs::write(&file, b"different bytes").unwrap();
        let diff = dispatch(
            &db,
            "s",
            "evidence_compare",
            &json!({"evidence_id":id}),
            Some(&dir),
        )
        .unwrap();
        assert_eq!(diff["status"], "change_detected");
        assert!(diff["changed_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "size"));
        let by_path = dispatch(
            &db,
            "s",
            "evidence_find",
            &json!({"subject":file.to_string_lossy()}),
            None,
        )
        .unwrap();
        assert_eq!(by_path["total_matches"], 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fresh_capture_and_explicit_retry_have_different_identities() {
        let db = memory_db();
        let dir = std::env::temp_dir().join(format!("ev-refresh-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut input = json!({"purpose":"检查当前产物", "file_path":"a.bin"});
        std::fs::write(dir.join("a.bin"), b"first").unwrap();
        let first = dispatch(&db, "s", "evidence_record", &input, Some(&dir)).unwrap();
        std::fs::write(dir.join("a.bin"), b"second version").unwrap();
        let second = dispatch(&db, "s", "evidence_record", &input, Some(&dir)).unwrap();
        assert_eq!(first["evidence"]["snapshot"]["size"], 5);
        assert_eq!(second["evidence"]["snapshot"]["size"], 14);
        assert_ne!(
            first["evidence"]["evidence_id"],
            second["evidence"]["evidence_id"]
        );

        input["idempotency_key"] = json!("capture-2");
        let keyed = dispatch(&db, "s", "evidence_record", &input, Some(&dir)).unwrap();
        std::fs::write(dir.join("a.bin"), b"third").unwrap();
        let retried = dispatch(&db, "s", "evidence_record", &input, Some(&dir)).unwrap();
        assert_eq!(keyed["evidence"], retried["evidence"]);
        assert_eq!(retried["reused_record"], true);
        input["purpose"] = json!("不同请求不可沿用同一键");
        assert!(dispatch(&db, "s", "evidence_record", &input, Some(&dir)).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_digest_matches_standard_sha256_and_legacy_records_remain_comparable() {
        let db = memory_db();
        let dir = std::env::temp_dir().join(format!("ev-digest-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), b"abc").unwrap();
        let saved = dispatch(
            &db,
            "s",
            "evidence_record",
            &json!({
                "purpose":"完整摘要", "file_path":"a.bin"
            }),
            Some(&dir),
        )
        .unwrap();
        assert_eq!(
            saved["evidence"]["snapshot"]["sample_sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut legacy = saved["evidence"].clone();
        let mut hash = Sha256::new();
        hash.update(0u64.to_le_bytes());
        hash.update(3u64.to_le_bytes());
        hash.update(b"abc");
        legacy["snapshot"]["sample_sha256"] = json!(format!("{:x}", hash.finalize()));
        legacy["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("digest_format");
        db.0.lock()
            .unwrap()
            .execute(
                "UPDATE agent_verification_evidence SET data_json=?1 WHERE id=?2",
                params![legacy.to_string(), legacy["evidence_id"].as_str().unwrap()],
            )
            .unwrap();
        let compared = dispatch(
            &db,
            "s",
            "evidence_compare",
            &json!({
                "evidence_id": legacy["evidence_id"]
            }),
            Some(&dir),
        )
        .unwrap();
        assert_eq!(compared["status"], "no_change_detected");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_optional_fields_are_rejected_instead_of_silently_dropped() {
        let db = memory_db();
        for patch in [
            json!({"output_id":42}),
            json!({"file_path":false}),
            json!({"tags":["valid", 123]}),
        ] {
            let mut input = json!({"subject":"API", "purpose":"检查"});
            input
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(
                dispatch(&db, "s", "evidence_record", &input, None).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn truncated_review_index_still_changes_for_unshown_same_time_records() {
        let db = memory_db();
        let at = "2026-10-17T00:00:00Z";
        let add = |id: &str| {
            db.0.lock()
                .unwrap()
                .execute(
                    "INSERT INTO agent_verification_evidence
                (id,session_id,subject,purpose,command,search_text,data_json,created_at)
                VALUES (?1,'s','artifact','check','','','{}',?2)",
                    params![id, at],
                )
                .unwrap();
        };
        for id in ["a1", "a2", "a3", "a4", "a5", "a6", "a7"] {
            add(id);
        }
        let first = review_digest(&db, "s", chrono::DateTime::UNIX_EPOCH)
            .unwrap()
            .unwrap();
        add("a0"); // 同时间但排序靠后，不会进入展示的六项。
        let second = review_digest(&db, "s", chrono::DateTime::UNIX_EPOCH)
            .unwrap()
            .unwrap();
        assert!(first.contains("has_more=true") && second.contains("has_more=true"));
        assert_eq!(first.lines().last(), second.lines().last());
        assert_ne!(first, second, "未展示的新增证据也须使旧审查缓存失效");
    }

    #[cfg(unix)]
    #[test]
    fn named_pipe_is_rejected_before_opening_it() {
        let dir = std::env::temp_dir().join(format!("ev-pipe-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(dir.join("pipe"))
            .status()
            .unwrap()
            .success());
        let (tx, rx) = std::sync::mpsc::channel();
        let root = dir.clone();
        std::thread::spawn(move || {
            let _ = tx.send(file_snapshot("pipe", &root, "s"));
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result
            .expect("文件快照不能阻塞在 FIFO open 上")
            .unwrap_err()
            .contains("普通文件"));
    }
}
