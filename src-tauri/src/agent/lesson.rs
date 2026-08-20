use crate::agent::retrieval::{insert_memory_node, MemoryNode};
use crate::db::DbState;
use rusqlite::params;
use serde_json::json;

/// 记录单步召回的 Lesson 列表（建立血缘事实）
pub fn record_lesson_recall(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    recalled_ids: &[String],
) {
    if recalled_ids.is_empty() {
        return;
    }
    if let Ok(conn) = db.0.lock() {
        let recall_id = uuid::Uuid::new_v4().to_string();
        let recalled_json = serde_json::to_string(recalled_ids).unwrap_or_else(|_| "[]".to_string());
        let _ = conn.execute(
            "INSERT INTO agent_lesson_recalls (id, session_id, step_id, recalled_lesson_ids, applied_lesson_ids, outcome, created_at)
             VALUES (?1, ?2, ?3, ?4, '[]', 'pending', ?5)",
            params![
                recall_id,
                session_id,
                step_id as i64,
                recalled_json,
                chrono::Utc::now().to_rfc3339()
            ],
        );
    }
}

/// 针对模型显式声明采纳的 Lesson 进行精准因果负反馈/正反馈打分
/// 核心准则：仅当模型在决策中回显 applied_lessons 时才结算因果责任，绝不冤杀未采纳的好经验
pub fn feedback_on_applied_lessons(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    applied_ids: &[String],
    is_success: bool,
) {
    if applied_ids.is_empty() {
        return;
    }

    if let Ok(conn) = db.0.lock() {
        let applied_json = serde_json::to_string(applied_ids).unwrap_or_else(|_| "[]".to_string());
        let outcome_str = if is_success { "succeeded" } else { "failed" };

        // 1. 更新或补录血缘记录（确保无论当步是否调用 search，采纳事实与执行因果 100% 落盘）
        let updated_rows = conn.execute(
            "UPDATE agent_lesson_recalls
             SET applied_lesson_ids = ?1, outcome = ?2
             WHERE session_id = ?3 AND step_id = ?4",
            params![applied_json, outcome_str, session_id, step_id as i64],
        ).unwrap_or(0);

        if updated_rows == 0 {
            let recall_id = uuid::Uuid::new_v4().to_string();
            let _ = conn.execute(
                "INSERT INTO agent_lesson_recalls (id, session_id, step_id, recalled_lesson_ids, applied_lesson_ids, outcome, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    recall_id,
                    session_id,
                    step_id as i64,
                    applied_json,
                    applied_json,
                    outcome_str,
                    chrono::Utc::now().to_rfc3339()
                ],
            );
        }

        // 2. 精准因果打分与置信度奖惩
        for lesson_id in applied_ids {
            if is_success {
                // 正反馈：采纳且成功 -> 增加命中次数与置信度 (+5, 上限 150)
                let _ = conn.execute(
                    "UPDATE agent_memory_nodes
                     SET hit_count = hit_count + 1,
                         confidence_score = MIN(confidence_score + 5, 150)
                     WHERE id = ?1",
                    params![lesson_id],
                );
                println!("💡 [Lesson 血缘正反馈] Lesson '{}' 显式采纳且当步成功，置信度提升 +5", lesson_id);
            } else {
                // 负反馈：采纳但报错失败 -> 扣减置信度 (-30)
                let _ = conn.execute(
                    "UPDATE agent_memory_nodes
                     SET confidence_score = MAX(confidence_score - 30, 0)
                     WHERE id = ?1",
                    params![lesson_id],
                );
                println!("⚠️ [Lesson 负反馈降权] Lesson '{}' 显式采纳但执行失败，因果扣除 30 分", lesson_id);
            }
        }
    }
}

/// 异步提炼并沉淀避坑经验 (Lesson)
/// 包含写入前去重合并 (Deduplicate on Write)，防止近似重复碎片导致 FTS5 召回稀释
pub fn distill_lesson_async(
    db: DbState,
    session_id: String,
    failed_step_id: usize,
    success_step_id: usize,
    tool: String,
    plan_id: Option<String>,
    lesson_summary: String,
    trigger_keywords: Vec<String>,
) {
    crate::task_registry::TaskRegistry::global().spawn(
        crate::task_registry::TaskKind::LessonWrite,
        format!("lesson-write:{}", session_id),
        async move {
        if let Ok(conn) = db.0.lock() {
            // 1. 写入前去重检查：若同 session 下已有类似总结，直接更新命中计数，避免重复膨胀
            let clean_summary = lesson_summary.trim();
            if clean_summary.is_empty() {
                return;
            }

            let mut existing_id: Option<String> = None;
            let check_sql = "SELECT id FROM agent_memory_nodes WHERE session_id = ?1 AND kind = 'lesson' AND content = ?2 LIMIT 1";
            if let Ok(id) = conn.query_row(check_sql, params![session_id, clean_summary], |row| row.get::<_, String>(0)) {
                existing_id = Some(id);
            }

            if let Some(id) = existing_id {
                let _ = conn.execute(
                    "UPDATE agent_memory_nodes SET hit_count = hit_count + 1 WHERE id = ?1",
                    params![id],
                );
                println!("🔄 [Lesson 合并] 发现同质经验 '{}'，已合并计数", id);
                return;
            }

            let lesson_id = format!("lesson-{}", uuid::Uuid::new_v4());
            let parent_id = format!("step-{}", failed_step_id);

            let metadata = json!({
                "failed_step": failed_step_id,
                "success_step": success_step_id,
                "tool": tool,
                "trigger_keywords": trigger_keywords,
            });

            let node = MemoryNode {
                id: lesson_id,
                parent_id: Some(parent_id),
                level: 2, // Level 2: 阶段摘要 / Lesson
                kind: "lesson".to_string(),
                session_id,
                plan_id,
                step_index: Some(success_step_id),
                content: lesson_summary,
                metadata_json: metadata.to_string(),
                confidence_score: 100,
                hit_count: 1,
                created_at: chrono::Utc::now().to_rfc3339(),
            };

            let keywords_str = trigger_keywords.join(", ");
            if let Err(e) = insert_memory_node(&conn, &node, &keywords_str) {
                eprintln!("⚠️ [Lesson] 异步经验提炼写入失败: {}", e);
            } else {
                println!("💡 [Lesson] 成功沉淀避坑经验: {}", node.content);
            }
        }
    },
    );
}
