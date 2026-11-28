//! Atomically admit a phone submission against the workspace the phone observed.

use super::run_spawn::{spawn_admitted_run, ChannelLaunch};
use super::session_ops::channel_task_routing;
use super::*;
use crate::agent::checkpoint::AgentPhase;
use crate::agent::control::SessionExecutionOwner;
use crate::agent::conversation::TaskStartMode;
use crate::agent::router::MobileRunSettings;
use crate::agent::tools::role_manager;
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
    // Skipping absent fields preserves the fingerprint of previously admitted requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_card: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_start_mode: Option<TaskStartMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_checkpoint: Option<MobileCheckpointIdentity>,
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

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct MobileCheckpointIdentity {
    task_id: String,
    version: i64,
}

#[derive(Serialize)]
pub(super) struct MobileTaskCheckpoint {
    #[serde(flatten)]
    identity: MobileCheckpointIdentity,
    phase: String,
}

pub(super) fn mobile_task_checkpoint_on(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<MobileTaskCheckpoint>> {
    // Old snapshots lack a durable task ID. Use a stable row identity until recovery saves one.
    conn.query_row(
        "SELECT COALESCE(NULLIF(CAST(json_extract(snapshot_json, '$.data.history_task_id') AS TEXT), ''),
                         'legacy:' || run_id || ':' || created_at), version, phase
         FROM agent_checkpoints WHERE run_id=?1 AND phase<>?2 AND json_valid(snapshot_json)",
        params![session_id, AgentPhase::Done.as_str()],
        |row| {
            Ok(MobileTaskCheckpoint {
                identity: MobileCheckpointIdentity {
                    task_id: row.get(0)?,
                    version: row.get(1)?,
                },
                phase: row.get(2)?,
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
) -> Result<MobileSession, HttpError> {
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
    Ok(session)
}

fn validate_mobile_model(conn: &Connection, id: &str) -> Result<(), HttpError> {
    let enabled: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM models m JOIN platforms p ON p.id=m.platform_id
               WHERE m.id=?1 AND m.enabled=1)",
            [id],
            |row| row.get(0),
        )
        .map_err(internal_error)?;
    if !enabled {
        return Err(invalid("所选主模型不存在或已停用，请重新选择。未启动任务。"));
    }
    let model = crate::llm::ModelConfig::from_conn(conn, id).map_err(internal_error)?;
    if model.base_url.trim().is_empty() || model.model_name.trim().is_empty() {
        return Err(invalid("所选主模型缺少接口地址或模型名称，请补全配置。未启动任务。"));
    }
    Ok(())
}

fn mobile_task_routing(
    conn: &Connection,
    config: &ChannelConfig,
    session: &MobileSession,
    overrides: Option<&PartialRouting>,
    role_name: Option<&str>,
    new_session: bool,
) -> Result<ModelRoutingConfig, HttpError> {
    let mut overrides = overrides.cloned().unwrap_or_default();
    let session_model = session.model_id.trim();
    let primary = overrides.pro.as_deref().map(str::trim).unwrap_or(session_model);
    if new_session && primary != session_model {
        validate_mobile_model(conn, session_model)?;
    }
    validate_mobile_model(conn, primary)?;
    overrides.pro = Some(primary.to_owned());
    let role_card = role_name
        .map(|name| {
            role_manager::resolve_enabled_card_on(conn, name)
                .map_err(internal_error)?
                .ok_or_else(|| invalid("所选角色卡不存在或已停用，请重新选择。未启动任务。"))
        })
        .transpose()?;
    let mut routing = channel_task_routing(config, conn, &session.id, Some(&overrides))?;
    routing.mobile_run = Some(Box::new(MobileRunSettings { role_card }));
    Ok(routing)
}

fn check_start_intent(
    conn: &Connection,
    session_id: &str,
    mode: Option<TaskStartMode>,
    expected: Option<&MobileCheckpointIdentity>,
) -> Result<(), HttpError> {
    let Some(mode) = mode else {
        return if expected.is_none() {
            Ok(())
        } else {
            Err(invalid("请指定继续原任务或新开一轮。"))
        };
    };
    let checkpoint = mobile_task_checkpoint_on(conn, session_id).map_err(internal_error)?;
    if checkpoint.as_ref().map(|checkpoint| &checkpoint.identity) != expected
        || (mode == TaskStartMode::Resume && checkpoint.is_none())
    {
        return Err(failure(
            StatusCode::CONFLICT,
            "checkpoint_changed",
            "原任务状态已变更或无法续跑，请刷新会话后重新选择。本次未启动任务。",
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
        if req.task_start_mode == Some(TaskStartMode::Resume) {
            req.goal = "继续原任务".into();
        } else {
            return Err(invalid("任务内容不能为空"));
        }
    }
    for field in [&mut req.session_id, &mut req.project_id] {
        if let Some(id) = field {
            *id = id.trim().to_owned();
            if id.is_empty() {
                return Err(invalid("会话或项目标识不能为空"));
            }
        }
    }
    for field in [&mut req.title, &mut req.model_id, &mut req.role_card] {
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

    let new_session = req.session_id.is_none();
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
    let session = check_session_scope(&tx, &session_id, req.project_id.as_deref())?;
    check_start_intent(
        &tx,
        &session_id,
        req.task_start_mode,
        req.expected_checkpoint.as_ref(),
    )?;
    let routing = mobile_task_routing(
        &tx, config, &session, req.model_routing.as_ref(), req.role_card.as_deref(), new_session,
    )?;
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
                task_start_mode: req.task_start_mode,
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
        let conn = crate::db::init_db(&PathBuf::from(":memory:")).unwrap();
        seed_test_models(&conn);
        DbState(Arc::new(Mutex::new(conn)))
    }

    fn seed_test_models(conn: &Connection) {
        conn.execute(
            "INSERT INTO platforms (id,name,base_url,api_key,created_at)
             VALUES ('mobile-test-provider','local','http://127.0.0.1:9/v1','local-test-only','now')",
            [],
        ).unwrap();
        for (id, enabled) in [
            ("test-model", true), ("alternate-model", true), ("disabled-model", false),
        ] {
            conn.execute(
                "INSERT INTO models(id,platform_id,name,display_name,enabled,created_at)
                 VALUES (?1,'mobile-test-provider',?1,?1,?2,'now')",
                params![id, enabled],
            ).unwrap();
        }
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
    fn mobile_uses_live_session_model_and_preserves_default_after_explicit_override() {
        use crate::agent::context::types::{ChatMessage, SandwichContext};
        use crate::agent::router::{AgentRouter, RoutingTier};

        let db = memory_db();
        let channel = ChannelConfig {
            pro: "channel-pro".into(),
            flash: "channel-flash".into(),
            vision: "channel-vision".into(),
            ..config()
        };
        let session = {
            let conn = db.0.lock().unwrap();
            let session = crate::commands::projects::create_plain_session_row(
                &conn, "测试会话", "alternate-model",
            ).unwrap();
            assert_eq!(channel_task_routing(&channel, &conn, &session.id, None).unwrap().pro, "channel-pro");
            session
        };
        let submit = |routing| {
            let mut input = request(Some(&session.id), None);
            input["model_routing"] = routing;
            admit_mobile_task(&db, &channel, serde_json::from_value(input).unwrap()).unwrap()
        };
        let (receipt, launch) = submit(json!({"pro": "test-model"}));
        let (owner, launch) = launch.unwrap();
        assert_eq!(launch.routing.pro, "test-model");
        crate::agent::conversation::begin_existing_turn_for_run(
            &db, &session.id, &launch.routing.pro, &launch.goal, &receipt.run_id,
        ).unwrap();
        drop(owner);
        {
            let conn = db.0.lock().unwrap();
            assert_eq!(mobile_session_on(&conn, &session.id).unwrap().unwrap().model_id, "alternate-model");
            assert_eq!(
                conn.query_row("SELECT model_id FROM chat_messages WHERE run_id=?1", [&receipt.run_id], |row| row.get::<_, String>(0)).unwrap(),
                "test-model",
            );
        }

        let (_, launch) = submit(json!({"context_budget_mode": "cost_saving", "role_models": {"executor": "flash"}}));
        let (owner, launch) = launch.unwrap();
        assert_eq!(launch.routing.pro, "alternate-model");
        assert_eq!(launch.routing.flash, "channel-flash");
        assert_eq!(launch.routing.vision, "channel-vision");
        assert_eq!(launch.routing.context_budget_mode, "cost_saving");
        let mut context = SandwichContext::new("system".into(), "goal".into());
        context.turns_history.push(ChatMessage {role: "assistant".into(), content: json!("an older step")});
        let decision = AgentRouter::route_for_step(&context, &launch.routing, 3);
        assert_eq!(decision.tier, RoutingTier::Pro);
        assert_eq!(decision.model_id, "alternate-model");
        context.add_image_feedback("source", "data:image/png;base64,QUJD");
        let decision = AgentRouter::route_for_step(&context, &launch.routing, 4);
        assert_eq!(decision.tier, RoutingTier::Vision);
        assert_eq!(decision.model_id, "channel-vision");
        drop(owner);

        db.0.lock().unwrap().execute(
            "UPDATE chat_sessions SET model_id='test-model' WHERE id=?1", [&session.id],
        ).unwrap();
        let (_, launch) = submit(json!(null));
        assert_eq!(launch.unwrap().1.routing.pro, "test-model");
    }

    #[test]
    fn mobile_rejects_stale_model_or_role_and_replays_an_accepted_receipt() {
        let db = memory_db();
        role_manager::upsert_card(&db, &json!({"name": "local_specialist", "label": "本地专家"})).unwrap();
        let mut input = request(None, None);
        input["role_card"] = json!("local_specialist");
        let (receipt, launch) = admit(&db, &input).unwrap();
        drop(launch);
        role_manager::toggle_card(&db, &json!("local_specialist")).unwrap();
        db.0.lock().unwrap().execute("UPDATE models SET enabled=0 WHERE id='test-model'", []).unwrap();
        let (reused, launch) = admit(&db, &input).unwrap();
        assert_eq!(reused, receipt);
        assert!(launch.is_none());

        for model in ["missing-model", "disabled-model"] {
            let mut input = request(None, None);
            input["model_routing"]["pro"] = json!(model);
            let (status, Json(body)) = admit(&db, &input).err().unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["code"], "invalid_submission");
            assert!(body["error"].as_str().unwrap().contains("模型"));
        }
        for role in ["missing-role", "local_specialist"] {
            let mut input = request(None, None);
            input["model_routing"]["pro"] = json!("alternate-model");
            input["role_card"] = json!(role);
            let (status, Json(body)) = admit(&db, &input).err().unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["code"], "invalid_submission");
            assert!(body["error"].as_str().unwrap().contains("角色卡"));
        }
        let conn = db.0.lock().unwrap();
        for table in ["chat_sessions", "agent_run_traces", "channel_mobile_submissions"] {
            assert_eq!(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        }
    }

    #[tokio::test]
    async fn mobile_selected_role_reaches_local_provider_after_checkpoint_restore() {
        use crate::agent::checkpoint::AgentCheckpoint;
        use crate::agent::context::SandwichContext;
        use crate::agent::router::AgentRouter;
        use tauri::Manager;

        let db = memory_db();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = tokio::spawn(async move {
            let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(
                move |Json(body): Json<Value>| {
                    captured.lock().unwrap().push(body);
                    async {
                        let frame = json!({"choices": [{
                            "delta": {"content": r#"{"tool":"local_file","command":{"action":"read","path":"example.txt"}}"#},
                            "finish_reason": "stop"
                        }]});
                        ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                         format!("data: {frame}\n\ndata: [DONE]\n\n"))
                    }
                },
            ));
            axum::serve(listener, app).await.unwrap();
        });
        {
            let conn = db.0.lock().unwrap();
            conn.execute("UPDATE platforms SET base_url=?1 WHERE id='mobile-test-provider'", [format!("http://{address}/v1")]).unwrap();
            conn.execute(
                "INSERT INTO agent_custom_instructions(id,kind,name,content,created_at,updated_at)
                 VALUES ('mobile-fixture','skill','mobile_handoff_fixture','LOCAL_ROLE_SKILL','now','now')",
                [],
            ).unwrap();
        }
        role_manager::upsert_card(&db, &json!({
            "name": "local_handoff", "label": "本地交接专家", "model_id": "test-model",
            "persona_prompt": "LOCAL_ROLE_PERSONA", "scope_hints": "LOCAL_ROLE_SCOPE",
            "allowed_skills": "mobile_handoff_fixture",
        })).unwrap();
        let mut input = request(None, None);
        input["model_id"] = json!("alternate-model");
        input["model_routing"] = json!({"context_budget_mode": "cost_saving"});
        input["role_card"] = json!("local_handoff");
        let (receipt, launch) = admit(&db, &input).unwrap();
        let (_owner, launch) = launch.unwrap();
        let mut old = SandwichContext::new("system baseline".into(), launch.goal.clone());
        old.active_role_context = "DISCARDED_ROLE".into();
        let mut checkpoint = AgentCheckpoint::new(receipt.session_id.clone(), &old);
        crate::agent::checkpoint::save_checkpoint(&db, &mut checkpoint).unwrap();
        let checkpoint = crate::agent::checkpoint::load_checkpoint(&db, &receipt.session_id)
            .unwrap().unwrap();

        let mut resumed = SandwichContext::new("system baseline".into(), launch.goal.clone());
        let card = launch.routing.mobile_run.as_deref().unwrap().role_card.as_ref().unwrap();
        resumed.active_role_context = role_manager::main_agent_role_context(
            &db, card, launch.workspace_override.as_deref().unwrap(),
        );
        checkpoint.restore_context(&mut resumed);
        let mut fresh = SandwichContext::new("system baseline".into(), "new turn".into());
        checkpoint.data.inherit_filling_into(&mut fresh);
        assert_eq!(fresh.assemble_messages()[0].content, "system baseline");

        let app = tauri::test::mock_builder().manage(db.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let state = app.state::<DbState>();
        let model = AgentRouter::route_for_step(&resumed, &launch.routing, 4);
        let (_tx, mut rx) = tokio::sync::watch::channel(0);
        let (instruction, _, _, _) = crate::agent::utils::llm_client::call_llm(
            &resumed, &state, model.model_id, "Pro", None, 4, &receipt.session_id,
            Some(&receipt.run_id), false, 1, "auto", false, &mut rx,
        ).await.unwrap();
        assert_eq!(instruction.get_tool(), "local_file");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "alternate-model");
        assert_eq!(requests[0]["messages"][0]["role"], "system");
        let prompt = requests[0]["messages"][0]["content"].as_str().unwrap();
        for expected in ["system baseline", "LOCAL_ROLE_PERSONA", "LOCAL_ROLE_SCOPE", "LOCAL_ROLE_SKILL"] {
            assert!(prompt.contains(expected), "missing {expected}");
        }
        assert!(!prompt.contains("DISCARDED_ROLE"));
        server.abort();
    }

    fn saved_mobile_checkpoint(db: &DbState) -> crate::agent::checkpoint::AgentCheckpoint {
        let session = {
            let conn = db.0.lock().unwrap();
            crate::commands::projects::create_plain_session_row(
                &conn,
                "本地续跑会话",
                "test-model",
            )
            .unwrap()
        };
        let context = crate::agent::context::SandwichContext::new("system".into(), "原任务".into());
        let mut checkpoint = crate::agent::checkpoint::AgentCheckpoint::new(session.id, &context);
        checkpoint.mark_failed("上次停止".into(), &context);
        crate::agent::checkpoint::save_checkpoint(db, &mut checkpoint).unwrap();
        checkpoint
    }

    fn checkpoint_metadata(db: &DbState, session_id: &str) -> Value {
        let conn = db.0.lock().unwrap();
        json!(mobile_task_checkpoint_on(&conn, session_id).unwrap())
    }

    #[test]
    fn mobile_explicit_resume_and_new_reach_the_same_checkpoint_choice_as_desktop() {
        let db = memory_db();
        let checkpoint = saved_mobile_checkpoint(&db);
        let current = checkpoint_metadata(&db, &checkpoint.run_id);
        assert_eq!(
            current,
            json!({ "task_id": checkpoint.data.history_task_id,
                    "version": checkpoint.version, "phase": "failed" })
        );
        let expected = json!({ "task_id": current["task_id"], "version": current["version"] });
        let mut resume = request(Some(&checkpoint.run_id), None);
        resume["task_start_mode"] = json!("resume");
        resume["expected_checkpoint"] = expected.clone();
        resume["goal"] = json!("");
        let (receipt, launch) = admit(&db, &resume).unwrap();
        let (owner, launch) = launch.unwrap();
        assert_eq!(launch.goal, "继续原任务");
        assert_eq!(launch.task_start_mode, Some(TaskStartMode::Resume));
        assert!(!crate::agent::conversation::should_start_new_turn(
            Some(&checkpoint), launch.task_start_mode,
        )
        .unwrap());

        let mut new = request(Some(&checkpoint.run_id), None);
        new["task_start_mode"] = json!("new");
        new["expected_checkpoint"] = expected;
        let (_, Json(body)) = admit(&db, &new).err().unwrap();
        assert_eq!(body["code"], "session_in_flight");
        drop(owner);
        let (next, launch) = admit(&db, &new).unwrap();
        let (_owner, launch) = launch.unwrap();
        assert_eq!(launch.task_start_mode, Some(TaskStartMode::New));
        assert!(crate::agent::conversation::should_start_new_turn(
            Some(&checkpoint), launch.task_start_mode,
        )
        .unwrap());
        assert_ne!(receipt.run_id, next.run_id);
        assert_eq!(receipt.session_id, next.session_id);
    }

    #[test]
    fn mobile_rejects_stale_intent_but_replays_accepted_resume_after_checkpoint_advances() {
        let db = memory_db();
        let mut checkpoint = saved_mobile_checkpoint(&db);
        let mut resume = request(Some(&checkpoint.run_id), None);
        resume["task_start_mode"] = json!("resume");
        resume["expected_checkpoint"] = checkpoint_metadata(&db, &checkpoint.run_id);
        let (receipt, launch) = admit(&db, &resume).unwrap();
        drop(launch);

        crate::agent::checkpoint::save_checkpoint(&db, &mut checkpoint).unwrap();
        let (same_receipt, replay) = admit(&db, &resume).unwrap();
        assert_eq!(same_receipt, receipt);
        assert!(replay.is_none());

        let mut modified_replay = resume.clone();
        modified_replay["task_start_mode"] = json!("new");
        let (_, Json(body)) = admit(&db, &modified_replay).err().unwrap();
        assert_eq!(body["code"], "request_conflict");

        for mode in ["resume", "new"] {
            let mut stale = resume.clone();
            stale["request_id"] = json!(Uuid::new_v4().to_string());
            stale["task_start_mode"] = json!(mode);
            let (status, Json(body)) = admit(&db, &stale).err().unwrap();
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["code"], "checkpoint_changed");
            stale["expected_checkpoint"]["version"] = json!(checkpoint.version);
            stale["expected_checkpoint"]["task_id"] = json!("different-task-same-version");
            let (_, Json(body)) = admit(&db, &stale).err().unwrap();
            assert_eq!(body["code"], "checkpoint_changed");
        }
        let conn = db.0.lock().unwrap();
        for table in ["agent_run_traces", "channel_mobile_submissions"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
        }
    }

    #[test]
    fn mobile_checkpoint_is_stable_for_legacy_rows_and_omits_completed_tasks() {
        let db = memory_db();
        let mut checkpoint = saved_mobile_checkpoint(&db);
        db.0.lock().unwrap().execute(
            "UPDATE agent_checkpoints SET snapshot_json=json_remove(snapshot_json, '$.data.history_task_id')
             WHERE run_id=?1",
            [&checkpoint.run_id],
        ).unwrap();
        let legacy = checkpoint_metadata(&db, &checkpoint.run_id);
        assert!(legacy["task_id"].as_str().unwrap().starts_with("legacy:"));
        assert_eq!(legacy, checkpoint_metadata(&db, &checkpoint.run_id));
        let mut legacy_resume = request(Some(&checkpoint.run_id), None);
        legacy_resume["task_start_mode"] = json!("resume");
        legacy_resume["expected_checkpoint"] = legacy;
        let (_, launch) = admit(&db, &legacy_resume).unwrap();
        assert!(launch.is_some());
        drop(launch);

        let corrupt = saved_mobile_checkpoint(&db);
        db.0.lock().unwrap().execute(
            "UPDATE agent_checkpoints SET snapshot_json='invalid' WHERE run_id=?1",
            [&corrupt.run_id],
        ).unwrap();
        assert!(checkpoint_metadata(&db, &corrupt.run_id).is_null());

        let context = crate::agent::context::SandwichContext::new("system".into(), "原任务".into());
        checkpoint.mark_done(&context);
        crate::agent::checkpoint::save_checkpoint(&db, &mut checkpoint).unwrap();
        assert!(checkpoint_metadata(&db, &checkpoint.run_id).is_null());
        let mut intent = request(Some(&checkpoint.run_id), None);
        intent["task_start_mode"] = json!("resume");
        intent["expected_checkpoint"] = Value::Null;
        let (_, Json(body)) = admit(&db, &intent).err().unwrap();
        assert_eq!(body["code"], "checkpoint_changed");
        intent["task_start_mode"] = json!("new");
        let (_, launch) = admit(&db, &intent).unwrap();
        assert!(launch.is_some());
    }

    #[test]
    fn mobile_legacy_request_replays_a_pre_upgrade_fingerprint() {
        let db = memory_db();
        let mut request = request(None, None);
        request["model_routing"] = Value::Null;
        let (receipt, launch) = admit(&db, &request).unwrap();
        drop(launch);
        // The pre-intent endpoint fingerprinted this normalized seven-field serialization.
        let pre_upgrade_bytes = format!(
            r#"{{"request_id":{},"session_id":null,"project_id":null,"goal":"本地验证","title":null,"model_id":null,"model_routing":null}}"#,
            request["request_id"]
        );
        let old_hash = format!("{:x}", Sha256::digest(pre_upgrade_bytes.as_bytes()));
        db.0.lock().unwrap().execute(
            "UPDATE channel_mobile_submissions SET request_hash=?1 WHERE run_id=?2",
            params![old_hash, receipt.run_id],
        ).unwrap();
        let (replayed, launch) = admit(&db, &request).unwrap();
        assert_eq!(replayed, receipt);
        assert!(launch.is_none());
    }

    #[test]
    fn mobile_retries_reuse_one_session_run_and_receipt_after_lost_ack_and_restart() {
        let fixture = std::env::temp_dir().join(format!("mobile-admission-{}", Uuid::new_v4()));
        let db_path = fixture.join("state.sqlite");
        let request = request(None, None);
        let receipt =
            {
                let conn = crate::db::init_db(&db_path).unwrap();
                seed_test_models(&conn);
                let db = DbState(Arc::new(Mutex::new(conn)));
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
