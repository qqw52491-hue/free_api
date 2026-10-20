use super::*;
use crate::agent::context::SandwichContext;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

fn db() -> DbState {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE chat_sessions (id TEXT PRIMARY KEY, title TEXT, model_id TEXT, created_at TEXT, updated_at TEXT, message_count INTEGER);
         CREATE TABLE chat_messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, attachments TEXT, created_at TEXT, model_id TEXT);",
    ).unwrap();
    DbState(Arc::new(Mutex::new(conn)))
}

fn enter(db: &DbState, cp: Option<&AgentCheckpoint>, goal: &str) -> SandwichContext {
    let incoming = history_for_entry(db, "s", cp).unwrap();
    let mut ctx = SandwichContext::new("SYS".into(), goal.into());
    ctx.turns_history = incoming.messages;
    ctx.conversation_cursor = incoming.cursor;
    if let Some(cp) = cp {
        cp.data.inherit_filling_into(&mut ctx);
    }
    if let Some(reference) = incoming.legacy_reference {
        ctx.turns_history.insert(0, reference);
    }
    ctx
}

fn checkpoint(ctx: &SandwichContext) -> AgentCheckpoint {
    // 必须经过真实 serde 落盘/恢复形状，游标不能只存在于内存。
    serde_json::from_value(serde_json::to_value(AgentCheckpoint::new("s".into(), ctx)).unwrap())
        .unwrap()
}

fn exact_count(ctx: &SandwichContext, text: &str) -> usize {
    ctx.assemble_messages()
        .iter()
        .filter(|m| m.content.as_str() == Some(text))
        .count()
}

#[test]
fn compacted_nogpu_request_never_reappears_after_gpu_goal_on_new_turn() {
    let db = db();
    begin_turn(&db, "s", "model", "先没有gpu的").unwrap();
    let mut ctx = enter(&db, None, "先没有gpu的");
    ctx.context_window_tokens = 8_000;
    // 使用真实压缩入口，无模型配置，确定性规则摘要；无需付费调用。
    for i in 0..60 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".into(),
            content: json!(format!("步骤 {i} {}", "X".repeat(400))),
        });
    }
    let gpu = "同样替换到那个40g的项目，停nogpu，启动GPU版";
    let cursor = append_human_intervention(&db, "s", gpu).unwrap();
    ctx.turns_history.push(ChatMessage {
        role: "user".into(),
        content: json!(gpu),
    });
    ctx.acknowledge_chat(Some(cursor));
    ctx.current_goal = gpu.into();
    let consumed = ctx.conversation_cursor.clone();
    ctx.maybe_compact_history();
    assert!(ctx.turns_history.iter().any(|m| m
        .content
        .as_str()
        .is_some_and(|s| s.contains("【历史早期执行归档"))));
    assert!(!ctx
        .turns_history
        .iter()
        .any(|m| m.content.as_str() == Some("先没有gpu的")));
    assert_eq!(ctx.conversation_cursor, consumed);

    let cp = checkpoint(&ctx);
    let goal = "删除镜像，然后重新install测试";
    begin_turn(&db, "s", "model", goal).unwrap();
    let next = enter(&db, Some(&cp), goal);
    assert_eq!(exact_count(&next, "先没有gpu的"), 0);
    assert_eq!(exact_count(&next, gpu), 1);
    assert_eq!(exact_count(&next, goal), 1);
    assert_eq!(next.effective_goal(), goal);
    assert!(history_for_entry(&db, "s", Some(&checkpoint(&next)))
        .unwrap()
        .messages
        .is_empty());

    // 用户真的再次发送同样一句话，按新消息身份保留；不能再按正文去重。
    begin_turn(&db, "s", "model", goal).unwrap();
    let again = enter(&db, Some(&checkpoint(&next)), goal);
    assert_eq!(exact_count(&again, goal), 2);
    assert_eq!(exact_count(&again, "先没有gpu的"), 0);
}

#[test]
fn consumed_answer_is_not_replayed_but_undelivered_answer_survives_resume() {
    let db = db();
    begin_turn(&db, "s", "model", "测试GPU").unwrap();
    let mut ctx = enter(&db, None, "测试GPU");
    let cursor = append_human_intervention(&db, "s", "好的，继续GPU").unwrap();
    // 回答在实际运行中以工具回执/人类回复包装呈现，不等于聊天原文。
    ctx.add_error_feedback("【人类回复】好的，继续GPU");
    ctx.acknowledge_chat(Some(cursor));
    let mut cp = checkpoint(&ctx);
    cp.mark_failed("用户停止".into(), &ctx);
    append_human_intervention(&db, "s", "续跑时不要重装Docker").unwrap();
    let entry = history_for_entry(&db, "s", Some(&cp)).unwrap();
    assert_eq!(entry.messages.len(), 1);
    let mut resumed = SandwichContext::new("SYS".into(), "继续".into());
    resumed.turns_history = entry.messages;
    resumed.conversation_cursor = entry.cursor;
    cp.restore_task_for_entry(&mut resumed, "继续");
    assert_eq!(exact_count(&resumed, "好的，继续GPU"), 0);
    assert_eq!(exact_count(&resumed, "续跑时不要重装Docker"), 1);
    assert_eq!(resumed.effective_goal(), "测试GPU");
    assert!(history_for_entry(&db, "s", Some(&checkpoint(&resumed)))
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn later_acknowledged_answer_does_not_hide_an_earlier_unconsumed_message() {
    let db = db();
    begin_turn(&db, "s", "m", "测试GPU").unwrap();
    let mut ctx = enter(&db, None, "测试GPU");
    append_human_intervention(&db, "s", "尚未消费的补充要求").unwrap();
    let consumed = append_human_intervention(&db, "s", "已消费的回答").unwrap();
    ctx.add_error_feedback("【人类回复】已消费的回答");
    ctx.acknowledge_chat(Some(consumed));
    // 模拟 clear_history；消费身份不依赖正文是否仍在上下文中。
    ctx.turns_history.clear();
    let cp = checkpoint(&ctx);
    let incoming = history_for_entry(&db, "s", Some(&cp)).unwrap();
    assert_eq!(incoming.messages.len(), 1);
    assert_eq!(
        incoming.messages[0].content.as_str(),
        Some("尚未消费的补充要求")
    );
    let next = enter(&db, Some(&cp), "继续");
    assert!(
        next.acknowledged_chat_ids.is_empty(),
        "入口推进游标后不永久累积单条确认集合"
    );
    assert!(history_for_entry(&db, "s", Some(&checkpoint(&next)))
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn cursor_uses_insert_order_despite_timestamp_ties_clock_rollback_and_vacuum() {
    let db = db();
    // 其他会话占据 rowid，删除后 VACUUM 可改变旧 rowid；必须按稳定 id 重定位。
    begin_turn(&db, "other", "m", "other").unwrap();
    begin_turn(&db, "s", "m", "GPU").unwrap();
    let ctx = enter(&db, None, "GPU");
    let cp = checkpoint(&ctx);
    let original_id = cp.data.conversation_cursor.as_ref().unwrap().id.clone();
    {
        let conn = db.0.lock().unwrap();
        conn.execute("DELETE FROM chat_messages WHERE session_id='other'", [])
            .unwrap();
        conn.execute_batch("VACUUM").unwrap();
        // 与游标相同时间，但 ID 的字典序更早，仍然是新增发言。
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) SELECT '000','s','user','同时间新消息',created_at FROM chat_messages WHERE id=?1", [&original_id]).unwrap();
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) VALUES ('rollback','s','user','时钟回拨后的新消息','2000-01-01T00:00:00Z')", []).unwrap();
    }
    let entry = history_for_entry(&db, "s", Some(&cp)).unwrap();
    assert_eq!(
        entry
            .messages
            .iter()
            .map(|m| m.content.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["同时间新消息", "时钟回拨后的新消息"]
    );
    assert_eq!(entry.cursor.unwrap().id, "rollback");
    assert!(entry.legacy_reference.is_none());
}

#[test]
fn legacy_checkpoint_marks_uncertain_answers_as_history_and_bootstraps_once() {
    let db = db();
    begin_turn(&db, "s", "m", "先没有gpu的").unwrap();
    begin_turn(&db, "s", "m", "现在测试40G GPU").unwrap();
    let mut ctx = SandwichContext::new("SYS".into(), "现在测试40G GPU".into());
    ctx.turns_history = vec![ChatMessage {
        role: "user".into(),
        content: json!("历史摘要：已从nogpu切到GPU"),
    }];
    let mut cp = checkpoint(&ctx);
    cp.created_at = "2026-10-16T06:16:00Z".into();
    cp.updated_at = "2026-10-16T06:39:00Z".into();
    {
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE chat_messages SET created_at=CASE content WHEN '先没有gpu的' THEN '2026-10-16T05:08:00Z' ELSE '2026-10-16T06:16:08Z' END", []).unwrap();
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) VALUES ('answer','s','user','只替换lib，不碰license','2026-10-16T06:25:00Z')", []).unwrap();
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) VALUES ('new','s','user','重新install','2026-10-16T06:40:00Z')", []).unwrap();
    }
    // 真正模拟缺字段的旧 JSON，而非仅内存 None。
    let mut legacy = serde_json::to_value(&cp).unwrap();
    legacy["data"]
        .as_object_mut()
        .unwrap()
        .remove("conversation_cursor");
    let cp: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
    let incoming = history_for_entry(&db, "s", Some(&cp)).unwrap();
    assert_eq!(incoming.messages.len(), 1);
    assert_eq!(incoming.messages[0].content.as_str(), Some("重新install"));
    let reference = incoming
        .legacy_reference
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .to_string();
    assert!(reference.contains("只替换lib，不碰license"));
    assert!(reference.contains("2026-10-16T06:25:00Z"));
    assert!(!reference.contains("先没有gpu的"));
    let next = enter(&db, Some(&cp), "重新install");
    assert_eq!(exact_count(&next, "先没有gpu的"), 0);
    let entry = history_for_entry(&db, "s", Some(&checkpoint(&next))).unwrap();
    assert!(entry.messages.is_empty());
    assert!(entry.legacy_reference.is_none());
}

#[test]
fn deleted_cursor_retains_uncertain_history_without_replaying_it_as_new_commands() {
    let db = db();
    begin_turn(&db, "s", "m", "先测试nogpu").unwrap();
    begin_turn(&db, "s", "m", "切到GPU").unwrap();
    let ctx = enter(&db, None, "切到GPU");
    let cp = checkpoint(&ctx);
    db.0.lock()
        .unwrap()
        .execute(
            "DELETE FROM chat_messages WHERE id=?1",
            [&cp.data.conversation_cursor.as_ref().unwrap().id],
        )
        .unwrap();
    begin_turn(&db, "s", "m", "重新install").unwrap();
    let incoming = history_for_entry(&db, "s", Some(&cp)).unwrap();
    assert_eq!(incoming.messages.len(), 1);
    assert_eq!(incoming.messages[0].content.as_str(), Some("重新install"));
    assert!(incoming
        .legacy_reference
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .contains("先测试nogpu"));
}

#[test]
fn live_supplements_are_imported_in_order_once_and_survive_checkpoint_and_compaction() {
    let db = db();
    begin_turn(&db, "s", "model", "原始目标").unwrap();
    let mut ctx = enter(&db, None, "原始目标");
    append_human_intervention(&db, "other", "不能混入其他会话").unwrap();
    append_human_intervention(&db, "s", "把颜色改为蓝色").unwrap();
    let answer = append_human_intervention(&db, "s", "已通过提问消费的回答").unwrap();
    ctx.turns_history.push(ChatMessage { role: "user".into(), content: json!("已通过提问消费的回答") });
    ctx.acknowledge_chat(Some(answer));
    append_human_intervention(&db, "s", "同时保留导出功能").unwrap();
    assert_eq!(import_live_messages(&db, "s", &mut ctx).unwrap(), 2);
    assert_eq!(ctx.user_goal, "原始目标");
    assert!(ctx.effective_goal().contains("把颜色改为蓝色"));
    assert!(ctx.effective_goal().contains("同时保留导出功能"));
    assert!(!ctx.effective_goal().contains("不能混入"));
    assert_eq!(exact_count(&ctx, "已通过提问消费的回答"), 1);
    assert_eq!(import_live_messages(&db, "s", &mut ctx).unwrap(), 0);
    let frozen = checkpoint(&ctx);
    let mut restored = ctx.clone();
    frozen.data.restore_into(&mut restored);
    assert_eq!(restored.effective_goal(), ctx.effective_goal());
    assert_eq!(import_live_messages(&db, "s", &mut restored).unwrap(), 0);
    restored.turns_history.clear(); // 正文压缩/清空不改变消费身份或当前要求
    assert_eq!(import_live_messages(&db, "s", &mut restored).unwrap(), 0);
    assert!(restored.effective_goal().contains("把颜色改为蓝色"));
}

#[test]
fn missing_live_cursor_never_replays_old_messages() {
    let db = db();
    begin_turn(&db, "s", "model", "历史消息").unwrap();
    let mut ctx = SandwichContext::new("SYS".into(), "新任务".into());
    assert_eq!(import_live_messages(&db, "s", &mut ctx).unwrap(), 0);
    assert_eq!(ctx.effective_goal(), "新任务");
}
