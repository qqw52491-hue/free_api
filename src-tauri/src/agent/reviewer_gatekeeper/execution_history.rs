//! Bounded, paginated navigation of host records. Membership comes from the
//! persisted parent-run chain, never from a session supplied by the model.
use crate::db::DbState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn number(req: &Value, key: &str, default: u64) -> Result<u64, String> {
    match req.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .ok_or_else(|| format!("{key} 必须是非负整数；未执行查询")),
    }
}

pub(crate) fn review_run_session(db: &DbState, parent: &str, run: &str) -> Result<String, String> {
    use rusqlite::OptionalExtension;
    let conn = db.reader()?;
    let session: Option<String> = conn.query_row(
        "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT c.id FROM agent_child_runs c JOIN tree t ON c.parent_run_id=t.id)
         SELECT r.session_id FROM agent_run_traces r JOIN tree t ON t.id=r.id WHERE r.id=?2",
        rusqlite::params![parent, run], |r| r.get(0),
    ).optional().map_err(|e| e.to_string())?;
    session.ok_or_else(|| "未找到本次父任务或其后代的执行轨迹；不能读取其他任务的回执".into())
}

pub(crate) fn review_history_tool(
    db: &DbState,
    parent: &str,
    name: &str,
    req: &Value,
) -> Result<String, String> {
    let result_page = name == "read_child_result";
    let max = if result_page { 16000 } else { 100 };
    let limit = number(req, "limit", if result_page { 4000 } else { 30 })?;
    if !(1..=max).contains(&limit) {
        return Err(format!("limit 必须在 1..{max} 之间"));
    }
    for field in ["run_id", "after_id", "child_run_id"] {
        if req
            .get(field)
            .is_some_and(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
        {
            return Err(format!("{field} 必须是非空字符串"));
        }
    }
    let conn = db.reader()?;
    if name == "list_execution_steps" {
        let run = req["run_id"].as_str().unwrap_or(parent);
        drop(conn);
        review_run_session(db, parent, run)?;
        let after = match req.get("after_seq") {
            None => -1,
            Some(v) => v
                .as_i64()
                .filter(|v| *v >= -1)
                .ok_or("after_seq 必须是大于等于 -1 的整数")?,
        };
        let conn = db.reader()?;
        let mut stmt = conn.prepare("SELECT seq,step_json FROM agent_run_steps WHERE run_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![run, after, limit + 1], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut steps = Vec::new();
        for row in rows {
            let (seq, raw) = row.map_err(|e| e.to_string())?;
            let s: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            steps.push(json!({"seq":seq,"step_id":s["id"],"tool":s["action"],"status":s["status"],
                "description":s["description"].as_str().unwrap_or("").chars().take(400).collect::<String>(),"output_id":s["output_id"]}));
        }
        let more = steps.len() > limit as usize;
        steps.truncate(limit as usize);
        return Ok(json!({"run_id":run,"steps":steps,"has_more":more,"next_after_seq":steps.last().map(|s|s["seq"].clone()),
            "scope_notice":"仅宿主持久化步骤目录；描述是执行者声明，结果须按 output_id 核实。读取后代输出请携带本目录 run_id。"}).to_string());
    }
    if name == "read_child_result" {
        let id = req["child_run_id"]
            .as_str()
            .ok_or("child_run_id 缺失或类型错误")?;
        let offset = number(req, "offset", 0)? as usize;
        // Full result text is paginated separately from the directory.
        let mut stmt = conn.prepare("WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT c.id FROM agent_child_runs c JOIN tree t ON c.parent_run_id=t.id)
            SELECT c.result_json,c.status,c.consumed_at FROM agent_child_runs c JOIN tree t ON t.id=c.id WHERE c.id=?2 AND c.id!=?1").map_err(|e|e.to_string())?;
        use rusqlite::OptionalExtension;
        let row = stmt
            .query_row(rusqlite::params![parent, id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or("未找到本任务树的子执行")?;
        let raw = row.0.unwrap_or_default();
        let total = raw.chars().count();
        let content: String = raw.chars().skip(offset).take(limit as usize).collect();
        let next = offset.saturating_add(content.chars().count());
        return Ok(json!({"child_run_id":id,"status":row.1,"consumed_at":row.2,"offset":offset,"next_offset":next,"has_more":next<total,"total_chars":total,"content":content,
            "scope_notice":"子执行结果与父任务回收是不同事实；结果包含模型声明，不证明整体任务验收。"}).to_string());
    }
    if name != "list_child_runs" {
        return Err("未知执行目录查询".into());
    }
    let after = req["after_id"].as_str().unwrap_or("");
    let mut stmt = conn.prepare("WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT c.id FROM agent_child_runs c JOIN tree t ON c.parent_run_id=t.id)
        SELECT c.id,c.parent_run_id,c.kind,c.purpose,c.status,c.started_at,c.finished_at,c.consumed_at,c.error_kind,c.result_json IS NOT NULL
        FROM agent_child_runs c JOIN tree t ON t.id=c.id WHERE c.id!=?1 AND c.id>?2 AND COALESCE(c.purpose,'')!='final_review' ORDER BY c.id LIMIT ?3").map_err(|e|e.to_string())?;
    let rows = stmt.query_map(rusqlite::params![parent,after,limit+1], |r| Ok(json!({"child_run_id":r.get::<_,String>(0)?,"parent_run_id":r.get::<_,String>(1)?,
        "kind":r.get::<_,Option<String>>(2)?,"purpose":r.get::<_,Option<String>>(3)?,"status":r.get::<_,String>(4)?,"started_at":r.get::<_,String>(5)?,
        "finished_at":r.get::<_,Option<String>>(6)?,"consumed_at":r.get::<_,Option<String>>(7)?,"error_kind":r.get::<_,Option<String>>(8)?,"result_available":r.get::<_,bool>(9)?}))).map_err(|e|e.to_string())?;
    let mut children = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let more = children.len() > limit as usize;
    children.truncate(limit as usize);
    // Fingerprint the complete tree, including results outside the first page.
    // The final reviewer itself is excluded so its registration does not churn caches.
    let mut state = Sha256::new();
    let mut stmt = conn.prepare("WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT c.id FROM agent_child_runs c JOIN tree t ON c.parent_run_id=t.id)
        SELECT c.id,c.status,c.consumed_at,c.result_json,c.error_kind FROM agent_child_runs c JOIN tree t ON t.id=c.id
        WHERE c.id!=?1 AND COALESCE(c.purpose,'')!='final_review' ORDER BY c.id").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map([parent], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        state.update(
            serde_json::to_vec(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
        );
    }
    let fingerprint = format!("{:x}", state.finalize());
    Ok(json!({"scope_notice":"本任务树登记状态；省略终审自身的 final_review 执行。完整结果按 child_run_id 查询。","state_fingerprint":fingerprint,"children":children,"has_more":more,"next_after_id":children.last().map(|c|c["child_run_id"].clone())}).to_string())
}
