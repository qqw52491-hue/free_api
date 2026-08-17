use crate::agent::retrieval::{insert_memory_node, MemoryNode};
use crate::db::DbState;
use serde_json::json;

/// 异步提炼并沉淀避坑经验 (Lesson)
/// 在后台执行，完全不阻塞主 Agent 运行循环
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
    tokio::spawn(async move {
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
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        if let Ok(conn) = db.0.lock() {
            let keywords_str = trigger_keywords.join(", ");
            if let Err(e) = insert_memory_node(&conn, &node, &keywords_str) {
                eprintln!("⚠️ [Lesson] 异步经验提炼写入失败: {}", e);
            } else {
                println!("💡 [Lesson] 成功沉淀避坑经验: {}", node.content);
            }
        }
    });
}
