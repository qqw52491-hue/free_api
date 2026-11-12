//! 带宿主来源的定向消息。消息是协作材料，不是人类批准，也不赋予目录/角色权限。
use crate::db::DbState;
pub(crate) const WAIT_INTERRUPTED: &str = "agent_message:interrupted";
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
pub(crate) mod replies;
pub(crate) mod coordination;
pub(crate) mod observation;

#[derive(Clone, Debug)]
pub(crate) struct Actor {
    pub run: String,
    pub session: String,
}
impl Actor {
    pub fn new(run: &str, session: &str) -> Self {
        Self {
            run: run.into(),
            session: session.into(),
        }
    }
}

fn parent(conn: &Connection, run: &str) -> Result<Option<Actor>, String> {
    conn.query_row(
        "SELECT parent_run_id,parent_session_id FROM agent_child_runs WHERE id=?1",
        [run],
        |r| {
            Ok(Actor {
                run: r.get(0)?,
                session: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}
fn root(conn: &Connection, actor: &Actor) -> Result<Actor, String> {
    let mut node = actor.clone();
    let mut seen = std::collections::HashSet::new();
    while let Some(up) = parent(conn, &node.run)? {
        if !seen.insert(node.run.clone()) {
            return Err("任务归属链存在循环，不能确认消息接收范围。".into());
        }
        node = up;
    }
    Ok(node)
}
fn resolve(conn: &Connection, sender: &Actor, target: &str) -> Result<(Actor, String), String> {
    let own_root = root(conn, sender)?;
    let id = match target {
        "parent" => {
            parent(conn, &sender.run)?
                .ok_or("当前运行没有父任务。")?
                .run
        }
        "self" => sender.run.clone(),
        _ => target.into(),
    };
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT kind,parent_session_id FROM agent_child_runs WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (actor, kind) = if let Some((kind, _)) = found {
        let session = if kind == "task" {
            format!("task-{id}")
        } else {
            format!("child:{id}")
        };
        (Actor { run: id, session }, kind)
    } else if id == own_root.run {
        (own_root.clone(), "main".into())
    } else {
        return Err("接收方不在当前任务的协作范围内。".into());
    };
    let target_root = root(conn, &actor)?;
    if target_root.run != own_root.run || target_root.session != own_root.session {
        return Err("接收方属于其他任务，消息未发送。".into());
    }
    Ok((actor, kind))
}

pub(crate) fn validate_payload(params: &Value) -> Result<(), String> {
    for key in ["target", "message", "idempotency_key"] {
        if let Some(v) = params.get(key) {
            if !v.is_string() {
                return Err(format!("{key} 类型错误：期望 string。"));
            }
        }
    }
    if let Some(v) = params.get("refs") {
        if !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
            return Err("refs 类型错误：期望 string[]。".into());
        }
    }
    if params
        .get("target")
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty())
    {
        return Err("send 缺少非空 target（任务 ID 或 parent）。".into());
    }
    if params
        .get("message")
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty())
        && params.get("data").is_none()
        && params
            .get("refs")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return Err("send 至少提供 message、data 或 refs。".into());
    }
    let payload = json!({"message":params.get("message"),"data":params.get("data"),"refs":params.get("refs")});
    if payload.to_string().chars().count() > 65_536 {
        return Err("协作消息超过 65536 字符；本次未发送，请用 refs 传递长证据引用。".into());
    }
    Ok(())
}

pub(crate) fn send(
    db: &DbState,
    sender: &Actor,
    params: &Value,
    retry_key: Option<&str>,
    human: bool,
) -> Result<Value, String> {
    validate_payload(params)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let target_name = params["target"].as_str().unwrap();
    let (target, kind) = resolve(&tx, sender, target_name)?;
    if !matches!(kind.as_str(), "main" | "task" | "worker" | "writer") {
        return Err(
            "该专用角色没有运行中消息入口；审查员沿用原有 resume_child 对质/追问。消息未发送。"
                .into(),
        );
    }
    let owner = root(&tx, sender)?;
    let payload = json!({"message":params.get("message"),"data":params.get("data"),"refs":params.get("refs")});
    let key = params
        .get("idempotency_key")
        .and_then(Value::as_str)
        .or(retry_key);
    let id = key
        .map(|key| {
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                json!([sender.run, key]).to_string().as_bytes(),
            )
            .to_string()
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let existing:Option<(String,String,String,Option<String>)>=tx.query_row("SELECT target_run_id,payload_json,created_at,delivered_at FROM agent_messages WHERE id=?1 AND sender_run_id=?2",params![id,sender.run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
    if let Some((to, body, created, delivered)) = existing {
        if to != target.run || body != payload.to_string() {
            return Err("同一消息重试键已绑定其他内容或接收方；本次未发送。".into());
        }
        return Ok(
            json!({"message_id":id,"target":to,"created_at":created,"delivery":if delivered.is_some(){"in_context"}else{"queued"},"duplicate":true,"notice":"in_context 只证明已保存到对方上下文，不证明模型已理解或完成要求。"}),
        );
    }
    if !replies::root_active_on(&tx, &owner.run)? {
        return Err("所属主任务已结束；消息未登记。".into());
    }
    if !human && sender.run != owner.run {
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_child_runs WHERE id=?1 AND status IN ('queued','running'))",[&sender.run],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !active { return Err("发送方执行已结束；新消息未登记。".into()); }
    }
    let status: Option<String> = if kind == "main" {
        tx.query_row(
            "SELECT status FROM agent_run_traces WHERE id=?1 AND session_id=?2",
            params![target.run, target.session],
            |r| r.get(0),
        )
        .optional()
    } else {
        tx.query_row(
            "SELECT status FROM agent_child_runs WHERE id=?1",
            [&target.run],
            |r| r.get(0),
        )
        .optional()
    }
    .map_err(|e| e.to_string())?;
    if !matches!(status.as_deref(), Some("queued" | "running" | "waiting_human")) && (human || status.as_deref() != Some("succeeded")) {
        return Err(format!("接收方状态为 {}；本入口仅接收运行中消息及 Agent 发给成功成员的只读材料，消息未入队。失败或取消成员请由负责人显式续派。",status.as_deref().unwrap_or("unknown")));
    }
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute("INSERT INTO agent_messages (id,session_id,root_run_id,sender_run_id,sender_session_id,sender_kind,target_run_id,target_session_id,payload_json,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![id,owner.session,owner.run,sender.run,sender.session,if human{"user_material"}else{"agent_material"},target.run,target.session,payload.to_string(),now]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    drop(conn);
    coordination::kick(db);
    crate::agent::job_engine::notify_job_state_changed();
    Ok(
        json!({"message_id":id,"target":target.run,"created_at":now,"delivery":"queued","notice":"已登记；运行中在安全边界接收，成功结束后由关联只读执行接收。不会中断工具，不作为人工批准。"}),
    )
}

pub(crate) fn pending(db: &DbState, actor: &Actor) -> Result<Vec<Value>, String> {
    pending_page(db, actor, 0, 16)
}
pub(crate) fn pending_page(
    db: &DbState,
    actor: &Actor,
    offset: u64,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_messages')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !exists {
        return Ok(vec![]);
    } // 旧数据库/最小夹具没有消息表，也就不存在可投递消息。
    let mut stmt=conn.prepare("SELECT id,sender_run_id,sender_kind,payload_json,created_at FROM agent_messages WHERE ((target_run_id=?1 AND target_session_id=?2 AND claimed_run_id IS NULL) OR claimed_run_id=?1) AND delivered_at IS NULL ORDER BY rowid LIMIT ?3 OFFSET ?4").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(
            params![
                actor.run,
                actor.session,
                limit.clamp(1, 16),
                offset.min(i64::MAX as u64)
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;
    rows.map(|r|r.map(|(id,from,kind,body,at)|json!({"message_id":id,"sender_run_id":from,"sender_kind":kind,"created_at":at,"payload":serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)})).map_err(|e|e.to_string())).collect()
}
pub(crate) fn has_pending(db: &DbState, run: &str) -> bool {
    db.0.lock().ok().and_then(|conn|conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_messages WHERE target_run_id=?1 AND delivered_at IS NULL)",[run],|r|r.get::<_,bool>(0)).ok()).unwrap_or(false)
}
/// 每封信独立保留来源与原文入口，避免整批宿主反馈截断后把中间信件误记为已进入上下文。
pub(crate) fn material_view(message: &Value) -> Value {
    let mut view = message.clone();
    let raw = view["payload"].to_string();
    if raw.chars().count() > 2048 {
        view.as_object_mut().unwrap().remove("payload");
        view["payload_excerpt"] = json!(raw.chars().take(1000).collect::<String>());
        view["payload_truncated"] = json!(true);
        view["read_full"] = json!({"message_id":message["message_id"],"main":"child_task.receipt(message_id)","specialist":"read_message(params.message_id)"});
    }
    view
}
fn render(message: &Value) -> String {
    format!("【协作消息：以下为宿主标注的来源与材料，不代表人类批准或已验证事实；节选时可按 message_id 回查原文】\n{}",material_view(message))
}

pub(crate) fn deliver_main(
    db: &DbState,
    actor: &Actor,
    context: &mut crate::agent::context::SandwichContext,
    checkpoint: &mut crate::agent::checkpoint::AgentCheckpoint,
) -> Result<usize, String> {
    if checkpoint.run_id != actor.session {
        return Err("消息接收会话与检查点归属不一致，未注入或核销。".into());
    }
    let messages = pending(db, actor)?;
    if messages.is_empty() {
        return Ok(0);
    }
    let mut candidate = context.clone();
    for message in &messages {
        candidate.add_host_feedback(&render(message));
    }
    let ids: Vec<String> = messages
        .iter()
        .filter_map(|m| m["message_id"].as_str().map(str::to_owned))
        .collect();
    crate::agent::checkpoint::save_checkpoint_and_messages(
        db, checkpoint, &candidate, &actor.run, &ids,
    )?;
    *context = candidate;
    Ok(messages.len())
}

pub(crate) fn deliver_specialist(
    db: &DbState,
    run: &str,
    messages: &mut Vec<Value>,
) -> Result<usize, String> {
    deliver_specialist_with_snapshot(db,run,messages,None)
}

pub(crate) fn deliver_specialist_with_snapshot(
    db:&DbState,run:&str,messages:&mut Vec<Value>,
    mut snapshot:Option<&mut crate::agent::execution::runtime::Snapshot>,
)->Result<usize,String>{
    if snapshot.as_ref().is_some_and(|s|s.execution_id!=run) {return Err("收信执行与 checkpoint 归属不一致；未注入或核销。".into());}
    let actor = Actor::new(run, &format!("child:{run}"));
    let inbox = pending(db, &actor)?;
    if inbox.is_empty() {
        return Ok(0);
    }
    let mut candidate = messages.clone();
    for message in &inbox {
        candidate.push(json!({"role":"user","content":render(message)}));
    }
    let persisted = crate::agent::child_writer::dehydrate_child_messages(&candidate);
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE agent_child_runs SET message_history_json=?1 WHERE id=?2",
        params![
            serde_json::to_string(&persisted).map_err(|e| e.to_string())?,
            run
        ],
    )
    .map_err(|e| e.to_string())?;
    for item in &inbox {
        tx.execute(
            "UPDATE agent_messages SET delivered_at=?1 WHERE id=?2 AND ((target_run_id=?3 AND claimed_run_id IS NULL) OR claimed_run_id=?3) AND delivered_at IS NULL",
            params![
                chrono::Utc::now().to_rfc3339(),
                item["message_id"].as_str(),
                run
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    let mut saved=snapshot.as_deref().cloned();
    if let Some(candidate_snapshot)=saved.as_mut() {
        candidate_snapshot.state["messages"]=json!(candidate);
        candidate_snapshot.save_on(&tx)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    if let (Some(target),Some(saved))=(snapshot.as_deref_mut(),saved) {*target=saved;}
    *messages = candidate;
    Ok(inbox.len())
}

pub(crate) fn receipt(db: &DbState, actor: &Actor, id: &str) -> Result<Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let (from,to,raw,at,delivered,claimed):(String,String,String,String,Option<String>,Option<String>)=conn.query_row(
        "SELECT sender_run_id,target_run_id,payload_json,created_at,delivered_at,claimed_run_id FROM agent_messages WHERE id=?1 AND (sender_run_id=?2 OR target_run_id=?2 OR claimed_run_id=?2 OR target_run_id=(SELECT resumed_from FROM agent_child_runs WHERE id=?2 AND (kind='task' OR purpose IN ('readonly_reply','readonly_inbox'))))",
        params![id,actor.run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(|_|"消息不存在或不属于当前收发方/宿主绑定的前次任务。".to_string())?;
    let status:Option<String>=conn.query_row("SELECT status FROM agent_child_runs WHERE id=?1 UNION ALL SELECT status FROM agent_run_traces WHERE id=?1 LIMIT 1",[&to],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
    let execution_status:Option<String>=conn.query_row("SELECT status FROM agent_child_runs WHERE id=?1",[claimed.as_deref().unwrap_or(&to)],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
    let delivery = if delivered.is_some() {
        "in_context"
    } else if matches!(execution_status.as_deref().or(status.as_deref()), Some("queued" | "running" | "waiting_human")) {
        "queued"
    } else {
        "not_delivered_target_ended"
    };
    Ok(
        json!({"message_id":id,"sender_run_id":from,"target_run_id":to,"payload":serde_json::from_str::<Value>(&raw).ok(),"created_at":at,"delivered_at":delivered,"delivery":delivery,"target_status":status,"execution_run_id":claimed,"execution_status":execution_status,"notice":"in_context 只证明带来源材料或节选及原文入口已保存到上下文，不证明模型理解或完成；结束时未投递的消息会作为后续追问的历史材料保留。"}),
    )
}

pub(crate) async fn wait(
    db: &DbState,
    actor: &Actor,
    target: Option<&str>,
    timeout: u64,
    interrupt: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<(), String> {
    let mut changes = crate::agent::job_engine::job_watch_receiver();
    let _ = changes.borrow_and_update();
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout.clamp(1, 600));
    loop {
        if !pending(db, actor)?.is_empty() {
            return Ok(());
        }
        if target.is_some_and(|id| crate::agent::child_task::runtime::paused(id).is_some()) {
            // 已有真实人工请求时保持挂起，不用模型反复轮询同一张表单；收到协作消息或停止仍可唤醒。
            tokio::select! {changed=changes.changed()=>{changed.map_err(|e|e.to_string())?;},_=interrupt.changed()=>return Err(WAIT_INTERRUPTED.into())}
            continue;
        }
        if let Some(id) = target {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let (target, kind) = resolve(&conn, actor, id)?;
            let status: Option<String> = if kind == "main" {
                conn.query_row(
                    "SELECT status FROM agent_run_traces WHERE id=?1",
                    [&target.run],
                    |r| r.get(0),
                )
                .optional()
            } else {
                conn.query_row(
                    "SELECT status FROM agent_child_runs WHERE id=?1",
                    [&target.run],
                    |r| r.get(0),
                )
                .optional()
            }
            .map_err(|e| e.to_string())?;
            if !matches!(status.as_deref(), Some("queued" | "running" | "waiting_human")) {
                return Ok(());
            }
        }
        tokio::select! { _=tokio::time::sleep_until(deadline)=>return Ok(()),changed=changes.changed()=>{changed.map_err(|e|e.to_string())?;},_=interrupt.changed()=>return Err(WAIT_INTERRUPTED.into()) }
    }
}

pub(crate) fn family_overview(db: &DbState, actor: &Actor, offset: u64, limit: u64) -> Result<Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let owner = root(&conn, actor)?;
    let limit = limit.clamp(1, 100);
    let offset = offset.min(i64::MAX as u64);
    // Stable ordering permits paging while members change status. Each row contains
    // only bounded projections of persisted facts, never checkpoint contents.
    let mut stmt = conn.prepare("WITH RECURSIVE family(id,depth,path) AS (SELECT ?1,0,json_array(?1) UNION ALL SELECT c.id,f.depth+1,json_insert(f.path,'$[#]',c.id) FROM agent_child_runs c JOIN family f ON c.parent_run_id=f.id WHERE NOT EXISTS (SELECT 1 FROM json_each(f.path) WHERE value=c.id)), members AS (SELECT f.id,f.depth,c.parent_run_id,c.kind,c.status,c.model_id,c.started_at,c.finished_at,c.purpose,c.resumed_from,c.applied_at,c.result_json,c.execution_checkpoint_json,c.execution_config_json,t.goal,t.updated_at,t.completion_json FROM family f LEFT JOIN agent_child_runs c ON c.id=f.id LEFT JOIN agent_run_traces t ON t.id=f.id) SELECT id,depth,parent_run_id,COALESCE(kind,'main'),status,model_id,started_at,finished_at,purpose,resumed_from,applied_at,result_json IS NOT NULL,goal,updated_at,completion_json IS NOT NULL,CASE WHEN json_valid(execution_checkpoint_json) THEN COALESCE(json_extract(execution_checkpoint_json,'$.state.main_review.role_label'),json_extract(execution_checkpoint_json,'$.state.dispatch_input.params.role_label')) END,(SELECT step_json FROM agent_run_steps s WHERE s.run_id=members.id ORDER BY seq DESC LIMIT 1), (SELECT COUNT(*) FROM agent_run_steps s WHERE s.run_id=members.id), (SELECT status FROM agent_run_traces t WHERE t.id=members.id), (SELECT started_at FROM agent_run_traces t WHERE t.id=members.id), (SELECT completed_at FROM agent_run_traces t WHERE t.id=members.id),execution_config_json,CASE WHEN kind='task' THEN COALESCE((SELECT json_object('profile','task','phase',p.phase,'version',p.version) FROM agent_checkpoints p WHERE p.run_id='task-'||members.id),execution_checkpoint_json) ELSE execution_checkpoint_json END FROM members ORDER BY depth,id LIMIT ?2 OFFSET ?3").map_err(|e|e.to_string())?;
    let rows = stmt.query_map(params![owner.run, limit + 1, offset], |r| {
        let id: String = r.get(0)?;
        let raw: Option<String> = r.get(16)?;
        let activity = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()).map(|s| json!({
            "description":s["description"].as_str().map(|v|v.chars().take(200).collect::<String>()),
            "tool":s["action"].as_str().or_else(||s["tool"].as_str()),"status":s["status"],
            "recorded_at":s["timestamp"].as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|time| time.to_rfc3339())
                .or_else(|| s["timestamp"].as_str().or_else(||s["created_at"].as_str()).map(str::to_owned))
        }));
        let kind: String = r.get(3)?;
        let is_main = kind == "main";
        Ok(json!({"child_run_id":id,"run_id":id,"depth":r.get::<_,u64>(1)?,"parent_run_id":r.get::<_,Option<String>>(2)?,
            "kind":kind,"execution_status":r.get::<_,Option<String>>(4)?.or(r.get::<_,Option<String>>(18)?),
            "model":r.get::<_,Option<String>>(5)?,"started_at":r.get::<_,Option<String>>(6)?.or(r.get::<_,Option<String>>(19)?),
            "finished_at":r.get::<_,Option<String>>(7)?.or(r.get::<_,Option<String>>(20)?),"purpose":r.get::<_,Option<String>>(8)?,
            "resumed_from":r.get::<_,Option<String>>(9)?,"collected_at":r.get::<_,Option<String>>(10)?,
            "result_available":if is_main {r.get::<_,bool>(14)?} else {r.get::<_,bool>(11)?},
            "task":r.get::<_,Option<String>>(12)?.map(|s|s.lines().next().unwrap_or("").chars().take(200).collect::<String>()),
            "updated_at":r.get::<_,Option<String>>(13)?,"role_label":r.get::<_,Option<String>>(15)?.map(|s|s.chars().take(120).collect::<String>()),
            "latest_activity":activity,"total_steps":r.get::<_,u64>(17)?,
            "execution_config":r.get::<_,Option<String>>(21)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()),
            "checkpoint":r.get::<_,Option<String>>(22)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()).map(|s|json!({"profile":s["profile"],"phase":s["phase"],"version":s["version"]}))}))
    }).map_err(|e|e.to_string())?;
    let mut tasks = rows.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    let has_more = tasks.len() > limit as usize;
    tasks.truncate(limit as usize);
    let next_offset = offset.saturating_add(tasks.len() as u64);
    Ok(json!({"tasks":tasks,"has_more":has_more,"next_offset":next_offset,"root_run_id":owner.run,"self_run_id":actor.run,
        "scope_notice":"同一任务树的只读快照；最近活动取自已保存步骤，更新时间不等于步骤发生时间。分页期间新增成员可能影响偏移。"}))
}

#[cfg(test)]
pub(crate) fn family_list(db: &DbState, actor: &Actor) -> Result<Vec<Value>, String> {
    Ok(family_overview(db, actor, 0, 100)?["tasks"].as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests;

pub(crate) fn authorize_target(db: &DbState, actor: &Actor, id: &str) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    resolve(&conn, actor, id).map(|_| ())
}

pub(crate) async fn specialist_action(
    db: &DbState,
    run: &str,
    action: &str,
    params: &Value,
    step: usize,
    interrupt: &mut tokio::sync::watch::Receiver<u64>,
    remaining_secs: u64,
    app: Option<tauri::AppHandle>,
) -> Option<Result<String, String>> {
    let actor = Actor::new(run, &format!("child:{run}"));
    let mut wait_interrupt = interrupt.clone();
    match action {
        "ask_agent" => Some(replies::ask(db, &actor, params, &format!("{step}:{params}"), app).map(|v|v.to_string())),
        "read_agent_result" => Some(replies::read_result(db, &actor, params).map(|v|v.to_string())),
        "send_message" => Some(
            send(db, &actor, params, Some(&format!("{step}:{params}")), false)
                .map(|v| v.to_string()),
        ),
        "wait_message" => Some(
            wait(
                db,
                &actor,
                None,
                params["timeout_secs"]
                    .as_u64()
                    .unwrap_or(60)
                    .min(remaining_secs.max(1)),
                &mut wait_interrupt,
            )
            .await
            .map(|_| "消息等待已返回；下一个安全边界读取信箱。等待本身不证明收到了消息。".into()),
        ),
        "read_message" => Some(
            receipt(
                db,
                &actor,
                params["message_id"].as_str().unwrap_or_default(),
            )
            .map(|v| v.to_string()),
        ),
        "team_status" if params.get("offset").is_some_and(|v| v.as_u64().is_none()) => Some(Err("team_status.offset 类型错误：期望非负整数；未查询。".into())),
        "team_status" => Some(family_overview(db, &actor, params["offset"].as_u64().unwrap_or(0), 100).and_then(|mut v| coordination::list(db,&actor,params["offset"].as_u64().unwrap_or(0)).map(|c|{v["collaboration"]=c;v.to_string()}))),
        _ => None,
    }
}
