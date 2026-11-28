//! Atomically admit a phone submission against the workspace the phone observed.

use super::run_spawn::{spawn_admitted_run, ChannelLaunch};
use super::session_ops::channel_task_routing;
use super::*;
use crate::agent::control::SessionExecutionOwner;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use uuid::Uuid;

type HttpError = (StatusCode, Json<Value>);

#[derive(Deserialize, Serialize)]
pub(crate) struct MobileTaskReq {
    request_id: String,
    session_id: Option<String>,
    // An explicit null declares scratch. An omitted field makes ownership ambiguous.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    project_id: Option<String>,
    goal: String,
    title: Option<String>,
    model_id: Option<String>,
    model_routing: Option<PartialRouting>,
}

#[derive(Serialize)]
pub(super) struct MobileSession {
    pub id: String,
    pub title: String,
    pub model_id: String,
    pub project_id: Option<String>,
    project_name: Option<String>,
    root_path: Option<String>,
}

/// Read this exact conversation, even if it falls outside the capped mobile sidebar list.
pub(super) fn mobile_session_on(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<MobileSession>> {
    conn.query_row(
        "SELECT s.id, s.title, s.model_id, p.id, p.name, p.root_path
         FROM chat_sessions s
         LEFT JOIN agent_project_sessions aps ON aps.session_id=s.id
         LEFT JOIN agent_projects p ON p.id=aps.project_id
         WHERE s.id=?1
           AND (p.id IS NOT NULL OR (s.project_origin_id IS NULL AND aps.project_id IS NULL))
           AND NOT EXISTS (
             SELECT 1 FROM agent_child_runs c WHERE c.id=substr(?1, 6)
               AND s.id='task-' || c.id
           )",
        [id],
        |row| {
            Ok(MobileSession {
                id: row.get(0)?,
                title: row.get(1)?,
                model_id: row.get(2)?,
                project_id: row.get(3)?,
                project_name: row.get(4)?,
                root_path: row.get(5)?,
            })
        },
    )
    .optional()
}

fn failure(status: StatusCode, code: &str, message: &str) -> HttpError {
    (status, Json(json!({ "code": code, "error": message })))
}

fn invalid(message: &str) -> HttpError {
    failure(StatusCode::BAD_REQUEST, "invalid_submission", message)
}

fn session_unavailable() -> HttpError {
    failure(
        StatusCode::NOT_FOUND,
        "session_unavailable",
        "原会话或所属项目已移除，未启动任务。",
    )
}

fn check_session_scope(
    conn: &Connection,
    session_id: &str,
    project_id: Option<&str>,
) -> Result<(), HttpError> {
    let session = mobile_session_on(conn, session_id)
        .map_err(internal_error)?
        .ok_or_else(session_unavailable)?;
    if session.project_id.as_deref() != project_id {
        return Err(failure(
            StatusCode::CONFLICT,
            "session_project_mismatch",
            "会话所属项目已变更，请重新打开后提交。本次未启动任务。",
        ));
    }
    Ok(())
}

/// Run under the workspace resolution lock. A project removed between admission and
/// launch must fail before the workspace resolver can fall back to a temporary directory.
pub(crate) fn verify_mobile_admission_workspace(
    conn: &Connection,
    run_id: &str,
    session_id: &str,
) -> Result<(), String> {
    // Isolated stores can use the main loop without initializing the channel schema.
    let has_table: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='channel_mobile_submissions')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !has_table {
        return Ok(());
    }
    let expected: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT session_id, project_id FROM channel_mobile_submissions WHERE run_id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some((expected_session, expected_project)) = expected {
        if expected_session != session_id {
            return Err("手机任务的会话归属已变化，未进入工作区。".into());
        }
        check_session_scope(conn, session_id, expected_project.as_deref()).map_err(
            |(_, Json(error))| {
                error["error"]
                    .as_str()
                    .unwrap_or("手机任务的工作区归属校验失败")
                    .to_owned()
            },
        )?;
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
struct MobileReceipt {
    run_id: String,
    session_id: String,
    project_id: Option<String>,
    llm_retries: u32,
}

impl MobileReceipt {
    fn response(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "session_id": self.session_id,
            "workspace": if self.project_id.is_some() { "project" } else { "scratch" },
            "llm_retries": self.llm_retries,
        })
    }
}

type PendingLaunch = (SessionExecutionOwner, ChannelLaunch);

fn normalize_request(req: &mut MobileTaskReq) -> Result<String, HttpError> {
    req.request_id = Uuid::parse_str(&req.request_id)
        .map_err(|_| invalid("提交标识无效"))?
        .to_string();
    req.goal = req.goal.trim().to_owned();
    if req.goal.is_empty() {
        return Err(invalid("任务内容不能为空"));
    }
    for field in [&mut req.session_id, &mut req.project_id] {
        if let Some(id) = field {
            *id = id.trim().to_owned();
            if id.is_empty() {
                return Err(invalid("会话或项目标识不能为空"));
            }
        }
    }
    for field in [&mut req.title, &mut req.model_id] {
        *field = field
            .take()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
    }
    let bytes = serde_json::to_vec(req).map_err(internal_error)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn admit_mobile_task(
    db: &DbState,
    config: &ChannelConfig,
    mut req: MobileTaskReq,
) -> Result<(MobileReceipt, Option<PendingLaunch>), HttpError> {
    let request_hash = normalize_request(&mut req)?;
    let mut conn = db.0.lock().map_err(internal_error)?;
    let tx = conn.transaction().map_err(internal_error)?;
    let previous = tx
        .query_row(
            "SELECT request_hash, run_id, session_id, project_id, llm_retries
             FROM channel_mobile_submissions WHERE request_id=?1",
            [&req.request_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    MobileReceipt {
                        run_id: row.get(1)?,
                        session_id: row.get(2)?,
                        project_id: row.get(3)?,
                        llm_retries: row.get(4)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(internal_error)?;
    if let Some((stored_hash, receipt)) = previous {
        if stored_hash != request_hash {
            return Err(failure(
                StatusCode::CONFLICT,
                "request_conflict",
                "提交标识已绑定另一份任务内容，本次未启动任务。",
            ));
        }
        return Ok((receipt, None));
    }

    let session_id = if let Some(session_id) = req.session_id {
        session_id
    } else {
        let title = req.title.unwrap_or_else(|| truncate_chars(&req.goal, 28));
        let model_id = req
            .model_id
            .or_else(|| {
                req.model_routing
                    .as_ref()
                    .and_then(|routing| routing.pro.clone())
            })
            .unwrap_or_else(|| {
                if config.model_id.trim().is_empty() {
                    config.pro.clone()
                } else {
                    config.model_id.clone()
                }
            });
        if model_id.trim().is_empty() {
            return Err(invalid("请为新会话选择模型"));
        }
        let session = if let Some(project_id) = &req.project_id {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_projects WHERE id=?1)",
                    [project_id],
                    |row| row.get(0),
                )
                .map_err(internal_error)?;
            if !exists {
                return Err(failure(
                    StatusCode::NOT_FOUND,
                    "project_unavailable",
                    "项目已移除，未创建会话或启动任务。",
                ));
            }
            crate::commands::projects::create_project_session_row(
                &tx, project_id, &title, &model_id,
            )
        } else {
            crate::commands::projects::create_plain_session_row(&tx, &title, &model_id)
        }
        .map_err(internal_error)?;
        session.id
    };
    let run_id = Uuid::new_v4().to_string();
    let owner = SessionExecutionOwner::acquire(&session_id, &run_id).map_err(|_| {
        failure(
            StatusCode::CONFLICT,
            "session_in_flight",
            "该会话已有在途任务，本次未启动。",
        )
    })?;
    check_session_scope(&tx, &session_id, req.project_id.as_deref())?;
    let routing = channel_task_routing(config, &tx, &session_id, req.model_routing.as_ref())?;
    let llm_retries = config
        .llm_retries
        .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
        .clamp(1, 100);
    tx.execute(
        "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
         VALUES (?1, ?2, ?3, 'running', '[]', ?4, ?4)",
        params![run_id, session_id, req.goal, now_ts()],
    ).map_err(internal_error)?;
    tx.execute(
        "INSERT INTO channel_mobile_submissions
         (request_id, request_hash, run_id, session_id, project_id, llm_retries)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            req.request_id,
            request_hash,
            run_id,
            session_id,
            req.project_id,
            llm_retries
        ],
    )
    .map_err(internal_error)?;
    tx.commit().map_err(internal_error)?;
    let workspace_override = req
        .project_id
        .is_none()
        .then(|| effective_scratch_dir(config));
    let receipt = MobileReceipt {
        run_id,
        session_id: session_id.clone(),
        project_id: req.project_id,
        llm_retries,
    };
    Ok((
        receipt,
        Some((
            owner,
            ChannelLaunch {
                session_id,
                goal: req.goal,
                routing,
                workspace_override,
                llm_retries: config.llm_retries,
                action_milestone_steps: config.action_milestone_steps,
                phase_push: config.phase_pushes,
                plan_mode: None,
                caller_notifies: false,
            },
        )),
    ))
}

pub(crate) async fn handler_mobile_task(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    Json(req): Json<MobileTaskReq>,
) -> Result<Json<Value>, HttpError> {
    let config = state.request_config();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let new_session = req.session_id.is_none();
    let db = state.app.state::<DbState>();
    let (receipt, launch) = admit_mobile_task(db.inner(), &config, req)?;
    if let Some((owner, launch)) = launch {
        if new_session {
            notify_session_event(&receipt.session_id, "created");
        }
        spawn_admitted_run(
            state.app.clone(),
            receipt.run_id.clone(),
            owner,
            true,
            launch,
        );
    }
    Ok(Json(receipt.response()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn memory_db() -> DbState {
        DbState(Arc::new(Mutex::new(
            crate::db::init_db(&PathBuf::from(":memory:")).unwrap(),
        )))
    }

    fn config() -> ChannelConfig {
        ChannelConfig {
            model_id: "test-model".into(),
            llm_retries: Some(5),
            ..ChannelConfig::default()
        }
    }

    fn request(session: Option<&str>, project: Option<&str>) -> Value {
        json!({
            "request_id": Uuid::new_v4().to_string(),
            "session_id": session,
            "project_id": project,
            "goal": "本地验证",
            "model_routing": { "pro": "test-model", "context_budget_mode": "deep_coding" },
        })
    }

    fn admit(
        db: &DbState,
        request: &Value,
    ) -> Result<(MobileReceipt, Option<PendingLaunch>), HttpError> {
        admit_mobile_task(
            db,
            &config(),
            serde_json::from_value(request.clone()).unwrap(),
        )
    }

    #[test]
    fn mobile_retries_reuse_one_session_run_and_receipt_after_lost_ack_and_restart() {
        let fixture = std::env::temp_dir().join(format!("mobile-admission-{}", Uuid::new_v4()));
        let db_path = fixture.join("state.sqlite");
        let request = request(None, None);
        let receipt =
            {
                let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&db_path).unwrap())));
                let (receipt, launch) = admit(&db, &request).unwrap();
                let (owner, launch) = launch.unwrap();
                assert_eq!(launch.goal, "本地验证");
                assert!(launch.workspace_override.is_some());
                let (reused, duplicate_launch) = admit(&db, &request).unwrap();
                assert_eq!(reused, receipt);
                assert!(duplicate_launch.is_none());
                drop(owner);
                db.0.lock().unwrap().execute(
                "UPDATE agent_run_traces SET goal='替换目标',status='succeeded' WHERE id=?1",
                [&receipt.run_id],
            ).unwrap();
                receipt
            };

        let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&db_path).unwrap())));
        let (reused, launch) = admit(&db, &request).unwrap();
        assert_eq!(reused, receipt);
        assert!(launch.is_none());
        {
            let conn = db.0.lock().unwrap();
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM chat_sessions", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM agent_run_traces", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            crate::db::delete_session_completely(&conn, &receipt.session_id, Some(&fixture))
                .unwrap();
        }
        let (reused, launch) = admit(&db, &request).unwrap();
        assert_eq!(reused, receipt);
        assert!(
            launch.is_none(),
            "a retry cannot bring a deleted conversation back"
        );
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM chat_sessions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn mobile_request_identity_binds_routing_goal_and_conversation() {
        let db = memory_db();
        let request = request(None, None);
        let (receipt, launch) = admit(&db, &request).unwrap();
        drop(launch);
        for (field, value) in [
            ("model_routing", json!({ "pro": "another-model" })),
            ("goal", json!("不同任务")),
            ("session_id", json!(receipt.session_id)),
        ] {
            let mut conflict = request.clone();
            conflict[field] = value;
            let (status, Json(body)) = admit(&db, &conflict).err().unwrap();
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["code"], "request_conflict");
        }
    }

    #[test]
    fn mobile_cannot_change_or_resurrect_project_workspace() {
        let db = memory_db();
        let (project, session) = {
            let conn = db.0.lock().unwrap();
            let path = std::env::temp_dir();
            let project = crate::commands::projects::create_project_row(
                &conn,
                "实际项目",
                path.to_str().unwrap(),
            )
            .unwrap();
            let session = crate::commands::projects::create_project_session_row(
                &conn,
                &project.id,
                "项目旧会话",
                "test-model",
            )
            .unwrap();
            (project, session)
        };
        let (_, Json(body)) = admit(&db, &request(Some(&session.id), None)).err().unwrap();
        assert_eq!(body["code"], "session_project_mismatch");
        let (receipt, launch) = admit(&db, &request(Some(&session.id), Some(&project.id))).unwrap();
        let (owner, launch) = launch.unwrap();
        assert!(
            launch.workspace_override.is_none(),
            "project retains its own workspace and resources"
        );
        {
            let conn = db.0.lock().unwrap();
            verify_mobile_admission_workspace(&conn, &receipt.run_id, &session.id).unwrap();
            conn.execute("DELETE FROM agent_projects WHERE id=?1", [&project.id])
                .unwrap();
            assert!(mobile_session_on(&conn, &session.id).unwrap().is_none());
            assert!(
                verify_mobile_admission_workspace(&conn, &receipt.run_id, &session.id).is_err()
            );
        }
        drop(owner);
        for scope in [Some(project.id.as_str()), None] {
            let (_, Json(body)) = admit(&db, &request(Some(&session.id), scope))
                .err()
                .unwrap();
            assert_eq!(
                body["code"], "session_unavailable",
                "detached history is never scratch"
            );
        }
        let (_, Json(body)) = admit(&db, &request(Some("deleted-session"), None))
            .err()
            .unwrap();
        assert_eq!(body["code"], "session_unavailable");
        let (_, Json(body)) = admit(&db, &request(None, Some(&project.id))).err().unwrap();
        assert_eq!(body["code"], "project_unavailable");
        let conn = db.0.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM chat_sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_run_traces", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn mobile_requires_an_explicit_project_or_scratch_declaration() {
        let mut input = request(None, None);
        assert!(serde_json::from_value::<MobileTaskReq>(input.clone()).is_ok());
        input.as_object_mut().unwrap().remove("project_id");
        assert!(serde_json::from_value::<MobileTaskReq>(input).is_err());
    }

    #[test]
    fn mobile_rejects_exact_internal_review_session_identity() {
        let db = memory_db();
        let conn = db.0.lock().unwrap();
        conn.execute_batch(
            "INSERT INTO chat_sessions(id,title,model_id,created_at,updated_at)
                 VALUES ('task-review-1','内部审查','test-model','now','now'),
                        ('task-review-10','普通会话近似 ID','test-model','now','now');
             INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind)
                 VALUES ('review-1','parent','parent-session','/tmp','test-model','succeeded','now','review');"
        ).unwrap();
        assert!(mobile_session_on(&conn, "task-review-1").unwrap().is_none());
        assert!(mobile_session_on(&conn, "task-review-10")
            .unwrap()
            .is_some());
        drop(conn);
        let (_, Json(body)) = admit(&db, &request(Some("task-review-1"), None))
            .err()
            .unwrap();
        assert_eq!(body["code"], "session_unavailable");
    }
}
