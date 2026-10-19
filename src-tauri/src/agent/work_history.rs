//! 工作历史目录：任务/计划归属由宿主保存，不生成语义摘要或验收结论。
use crate::agent::context::SandwichContext;
use crate::agent::retrieval::{insert_memory_node, MemoryNode};
use crate::db::DbState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{cell::RefCell, future::Future};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkGroup {
    pub session_id: String,
    pub task_id: String,
    pub plan_id: String,
    pub title: String,
    pub step_id: usize,
}

impl WorkGroup {
    pub fn capture(session_id: &str, context: &SandwichContext, step_id: usize) -> Option<Self> {
        Some(Self {
            session_id: session_id.into(),
            task_id: context.history_task_id.clone(),
            plan_id: context.active_plan_id()?.into(),
            title: context.active_plan_title().unwrap_or("工作过程").into(),
            step_id,
        })
    }

    pub fn node_id(&self) -> String {
        let identity = json!([self.session_id, self.task_id, self.plan_id]).to_string();
        format!(
            "episode-{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, identity.as_bytes())
        )
    }
}

tokio::task_local! { static DISPATCH_GROUP: Option<WorkGroup>; }
thread_local! { static BLOCKING_GROUP: RefCell<Option<WorkGroup>> = const { RefCell::new(None) }; }

pub(crate) fn current_group() -> Option<WorkGroup> {
    match DISPATCH_GROUP.try_with(Clone::clone) {
        Ok(group) => group,
        Err(_) => BLOCKING_GROUP.with(|g| g.borrow().clone()),
    }
}

pub(crate) async fn in_group<F: Future>(group: Option<WorkGroup>, future: F) -> F::Output {
    DISPATCH_GROUP.scope(group, future).await
}

pub(crate) fn in_blocking_group<T>(group: Option<WorkGroup>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<WorkGroup>);
    impl Drop for Restore {
        fn drop(&mut self) {
            BLOCKING_GROUP.with(|g| *g.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(BLOCKING_GROUP.with(|g| g.replace(group)));
    f()
}

pub(crate) fn ensure_group(conn: &Connection, group: &WorkGroup) -> Result<String, String> {
    write_group(conn, group, false)
}

pub(crate) fn prepare_group(db: &DbState, group: Option<&WorkGroup>) {
    if let Some(group) = group {
        let result =
            db.0.lock()
                .map_err(|e| e.to_string())
                .and_then(|conn| write_group(&conn, group, true));
        if let Err(e) = result {
            eprintln!("工作目录派发标签未保存: {e}");
        }
    }
}

fn write_group(conn: &Connection, group: &WorkGroup, update_label: bool) -> Result<String, String> {
    let id = group.node_id();
    // 只更新标签；不覆盖此前冻结的异步归属，也不改变创建时间。
    let label = format!("工作目录（标签，不代表完成）: {}", group.title);
    let existing: Option<(String, String, String)> = conn.query_row(
        "SELECT metadata_json, created_at, content FROM agent_memory_nodes WHERE id=?1 AND session_id=?2",
        params![id, group.session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional().map_err(|e| e.to_string())?;
    if existing
        .as_ref()
        .is_some_and(|(_, _, content)| !update_label || *content == label)
    {
        return Ok(id);
    }
    let (mut metadata, created_at) = existing
        .map(|(m, at, _)| {
            (
                serde_json::from_str::<Value>(&m).unwrap_or_else(|_| json!({})),
                at,
            )
        })
        .unwrap_or_else(|| (json!({}), chrono::Utc::now().to_rfc3339()));
    metadata["work_group"] = json!(group);
    let node = MemoryNode {
        id: id.clone(),
        parent_id: None,
        level: 1,
        kind: "episode".into(),
        session_id: group.session_id.clone(),
        plan_id: Some(group.plan_id.clone()),
        step_index: None,
        content: label,
        metadata_json: metadata.to_string(),
        confidence_score: 100,
        hit_count: 0,
        created_at,
    };
    insert_memory_node(conn, &node, &group.title)?;
    Ok(id)
}

/// 在真实异步记录创建时绑定派发快照。未知归属不借用回收时的活动计划。
pub(crate) fn bind_async(db: &DbState, session_id: &str, async_id: &str, inherited: Option<&str>) {
    let group = match inherited {
        Some(id) => async_group(db, session_id, id),
        None => current_group().filter(|g| g.session_id == session_id),
    };
    let Some(group) = group else { return };
    let result = (|| -> Result<(), String> {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let id = ensure_group(&conn, &group)?;
        conn.execute(
            "UPDATE agent_memory_nodes SET metadata_json=json_set(metadata_json, '$.async_origins',
             json_insert(COALESCE(json_extract(metadata_json,'$.async_origins'),'[]'),'$[#]',json(?1)))
             WHERE id=?2 AND NOT EXISTS (SELECT 1 FROM json_each(metadata_json,'$.async_origins') WHERE json_extract(value,'$.id')=?3)",
            params![json!({"id":async_id,"origin":group}).to_string(), id, async_id],
        ).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("工作目录异步归属未保存: {e}");
    }
}

fn async_group(db: &DbState, session_id: &str, async_id: &str) -> Option<WorkGroup> {
    let conn = db.0.lock().ok()?;
    let mut stmt = conn.prepare(
        "SELECT json_extract(j.value,'$.origin') FROM agent_memory_nodes n, json_each(n.metadata_json,'$.async_origins') j
         WHERE n.session_id=?1 AND n.kind='episode' AND json_extract(j.value,'$.id')=?2 LIMIT 2"
    ).ok()?;
    let values = stmt
        .query_map(params![session_id, async_id], |r| r.get::<_, String>(0))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() != 1 {
        return None;
    }
    serde_json::from_str(&values[0]).ok()
}

/// 完成回执另存为观察，不把“派发成功”当成后台工作完成。
pub(crate) fn record_async_result(
    db: &DbState,
    session_id: &str,
    async_id: &str,
    status: &str,
    raw: &str,
) {
    let Some(group) = async_group(db, session_id, async_id) else {
        return;
    };
    // 已完成回执以真实异步 ID 幂等，跨消费通道/并发重放不重复造观察。
    if let Err(e) = crate::agent::run_trace::save_group_observation(
        db,
        session_id,
        group.step_id,
        "async_result",
        &format!("后台回执 {async_id}"),
        status,
        raw,
        Some(&group),
        Some(async_id),
    ) {
        eprintln!("工作目录后台回执未保存: {e}");
    }
}

/// 按需分页展开目录，只展示观察索引，不自动加载原文或判定业务成败。
pub(crate) fn get_group(
    conn: &Connection,
    session_id: &str,
    request: &Value,
) -> Result<Value, String> {
    let id = request
        .get("group_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("get_work_group 缺少 group_id（使用检索返回的 parent_id 或 episode ID）")?;
    let offset = request
        .get("offset")
        .map(|v| v.as_u64().ok_or("offset 必须是非负整数"))
        .transpose()?
        .unwrap_or(0)
        .min(i64::MAX as u64) as i64;
    let limit = request
        .get("limit")
        .map(|v| v.as_u64().filter(|n| *n > 0).ok_or("limit 必须是正整数"))
        .transpose()?
        .unwrap_or(10)
        .min(25) as i64;
    let title: String = conn.query_row(
        "SELECT content FROM agent_memory_nodes WHERE id=?1 AND session_id=?2 AND level=1 AND kind='episode'",
        params![id, session_id], |r| r.get(0)
    ).map_err(|_| "未找到本会话的工作目录".to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, content, metadata_json, created_at FROM agent_memory_nodes
         WHERE parent_id=?1 AND session_id=?2 AND level=0 ORDER BY created_at,id LIMIT ?3 OFFSET ?4"
    ).map_err(|e| e.to_string())?;
    let mut events = stmt.query_map(params![id, session_id, limit + 1, offset], |r| {
        let metadata: String = r.get(2)?;
        let metadata: Value = serde_json::from_str(&metadata).unwrap_or(Value::Null);
        Ok(json!({"event_id":r.get::<_,String>(0)?, "preview":r.get::<_,String>(1)?.chars().take(240).collect::<String>(),
            "output_id":metadata["output_id"], "outcome":metadata["outcome"], "tool":metadata["tool"],
            "action":metadata["action"].as_str().map(|s|s.chars().take(120).collect::<String>()), "created_at":r.get::<_,String>(3)?}))
    }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?;
    let has_more = events.len() > limit as usize;
    events.truncate(limit as usize);
    Ok(
        json!({"group_id":id,"label":title,"events":events,"offset":offset,"has_more":has_more,
        "next_offset":if has_more {Some(offset + events.len() as i64)} else {None},
        "note":"目录记录尝试和观察；标签不代表完成，outcome 不替代业务验证。原文按 output_id 回查。"}),
    )
}

#[cfg(test)]
mod tests;
