use super::*;
use crate::agent::{
    agent_mailbox::{self, Actor},
    checkpoint::{load_checkpoint, AgentCheckpoint},
    child_task::{self, runtime, team_view},
    child_receipt::collect_task,
    context::SandwichContext,
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MOCK_CODEX: &str = r#"#!/usr/bin/env python3
import json, sys, time
from pathlib import Path

def send(**message):
    print(json.dumps(message), flush=True)

def event(method, **params):
    send(method=method, params={"threadId":"fixture-thread", **params})

def wait_for(path):
    for _ in range(1200):
        if Path(path).exists(): return
        time.sleep(.01)
    raise TimeoutError(path)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        send(id=1, result={})
    elif method == "config/read":
        send(id=2, result={"config":{"mcp_servers":{}}})
    elif method == "thread/start":
        opts = request["params"]
        profile = opts["permissions"]
        assert opts["approvalPolicy"] == "never" and opts["ephemeral"]
        assert opts["config"]["permissions"][profile]["filesystem"][opts["cwd"]] == "read"
        send(id=3, result={"thread":{"id":"fixture-thread"},"activePermissionProfile":{"id":profile},
                           "approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":False}})
    elif method == "turn/start":
        assert request["params"]["input"][0]["text"] == "查调用点"
        send(id=4, result={"turn":{"id":"fixture-turn"}})
        Path("mock-turn-started").touch()
        wait_for("mock-show-wait")
        event("thread/status/changed", status={"type":"active","activeFlags":["waitingOnUserInput"]})
        wait_for("mock-show-running")
        event("thread/status/changed", status={"type":"active","activeFlags":[]})
        wait_for("mock-finish")
        event("item/completed", turnId="fixture-turn",
              item={"type":"agentMessage","phase":"final_answer","text":"模拟结论：找到两个调用点。"})
        event("turn/completed", turn={"id":"fixture-turn","status":"completed","items":[]})
        break
"#;

async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("mock Codex did not reach the expected phase");
}

#[tokio::test]
async fn external_codex_progress_and_parent_receipt_with_local_process() {
    let workdir =
        Temp(std::env::temp_dir().join(format!("external-codex-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&workdir.0).unwrap();
    let command = workdir.0.join("mock-codex");
    std::fs::write(&command, MOCK_CODEX).unwrap();
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = DbState(Arc::new(Mutex::new(
        crate::db::init_db(&workdir.0.join("tasks.db")).unwrap(),
    )));
    let app = tauri::test::mock_app();
    let params = json!({"executor":"project_external","task":"查调用点"});
    let metadata = json!({"name":"本地 Codex","kind":"codex"});
    let binding = register(
        &db,
        "parent-session",
        "parent-run",
        &workdir.0,
        "external-dispatch".into(),
        &params,
        &metadata,
    )
    .unwrap();
    let parent = Actor::new(&binding.parent_run, &binding.parent_session);
    let id = &binding.child_run;
    let status = || child_task::status(&db, &binding.parent_session, id).unwrap();
    runtime::spawn_external(
        app.handle().clone(),
        db.clone(),
        binding.clone(),
        Request {
            command,
            workspace: workdir.0.clone(),
            prompt: "查调用点".into(),
        },
        None,
    )
    .unwrap();

    until(|| workdir.0.join("mock-turn-started").exists()).await;
    assert_eq!(status()["activity"], "running");
    assert_eq!(status()["external_agent"], metadata);
    assert_eq!(status()["task"], "查调用点");
    assert_eq!(status()["result_available"], false);
    let (_interrupt, mut rx) = tokio::sync::watch::channel(0);
    let wait_db = db.clone();
    let wait_id = id.clone();
    let waiter = tokio::spawn(async move {
        agent_mailbox::wait(&wait_db, &parent, Some(&wait_id), 30, &mut rx).await
    });
    std::fs::write(workdir.0.join("mock-show-wait"), "").unwrap();
    until(|| status()["activity"] == "waiting_external").await;
    assert!(status()["external_wait_reason"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(!waiter.is_finished());
    std::fs::write(workdir.0.join("mock-show-running"), "").unwrap();
    until(|| status()["activity"] == "running").await;
    assert!(status()["external_wait_reason"].is_null());
    std::fs::write(workdir.0.join("mock-finish"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(8), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let complete = status();
    assert_eq!(complete["execution_status"], "succeeded");
    assert_eq!(complete["handoff_accepted"], true);
    assert_eq!(complete["result_available"], true);
    assert!(complete["collected_at"].is_null());
    let result = child_task::result(&db, &binding.parent_session, id).unwrap();
    assert_eq!(result["delivery"]["output"], "模拟结论：找到两个调用点。");

    let mut context = SandwichContext::new("system".into(), "parent goal".into());
    let mut checkpoint = AgentCheckpoint::new(binding.parent_session.clone(), &context);
    assert!(!collect_task(&db, "wrong-parent", id, &mut checkpoint, &mut context).unwrap());
    assert!(collect_task(
        &db,
        &binding.parent_session,
        id,
        &mut checkpoint,
        &mut context
    )
    .unwrap());
    let observed =
        serde_json::to_value(team_view::observe(&db, &binding.parent_session).unwrap()).unwrap();
    assert_eq!(observed["receipts"][0]["child_run_id"], *id);
    assert!(observed["receipts"][0]["collected_at"].is_string());
    let loaded = load_checkpoint(&db, &binding.parent_session)
        .unwrap()
        .unwrap();
    let before = serde_json::to_string(&loaded.data).unwrap();
    assert!(before.contains("模拟结论：找到两个调用点。"));
    assert!(collect_task(
        &db,
        &binding.parent_session,
        id,
        &mut checkpoint,
        &mut context
    )
    .unwrap());
    assert_eq!(serde_json::to_string(&checkpoint.data).unwrap(), before);

    let orphan = register(
        &db,
        &binding.parent_session,
        &binding.parent_run,
        &workdir.0,
        "expired-dispatch".into(),
        &params,
        &metadata,
    )
    .unwrap();
    assert_eq!(interrupt_orphans(&db.writer().unwrap()).unwrap(), 1);
    let interrupted = child_task::result(&db, &binding.parent_session, &orphan.child_run).unwrap();
    assert_eq!(interrupted["execution_status"], "cancelled");
    assert_eq!(interrupted["host_error"], INTERRUPTED);
    assert_eq!(status()["execution_status"], "succeeded");
}
