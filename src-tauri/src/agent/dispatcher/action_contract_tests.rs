use super::*;
use crate::agent::checkpoint::{AgentCheckpoint, PlanExecutionMode};
use crate::agent::context::SandwichContext;
use crate::agent::triage::TriageVerdict;
use std::path::PathBuf;
use std::sync::Mutex;

struct Fixture {
    workspace: PathBuf,
    session: String,
    db: DbState,
    registry: PluginRegistry,
}

impl Fixture {
    fn new() -> Self {
        let session = format!("action-contract-{}", uuid::Uuid::new_v4());
        let workspace = std::env::temp_dir().join(&session);
        std::fs::create_dir_all(&workspace).unwrap();
        Self {
            workspace,
            session,
            db: DbState(Arc::new(Mutex::new(
                crate::db::init_db(&PathBuf::from(":memory:")).unwrap(),
            ))),
            registry: PluginRegistry::new(),
        }
    }

    fn execute(&self, value: serde_json::Value) -> (AgentInstruction, DispatchResult) {
        let instruction = serde_json::from_value(value).unwrap();
        let result = run_agent_step(
            &self.db,
            &self.session,
            &instruction,
            &self.registry,
            Some(&self.workspace),
        );
        (instruction, result)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let runs: Vec<String> = {
            let conn = self.db.0.lock().unwrap();
            let mut stmt = conn.prepare("SELECT DISTINCT parent_run_id FROM agent_background_jobs").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
        };
        for run in runs {
            let _ = crate::agent::job_engine::cancel_all_running_jobs(&self.db, &run);
        }
        let _ = std::fs::remove_dir_all(&self.workspace);
        let _ =
            std::fs::remove_dir_all(crate::agent::path_guard::get_session_tmp_dir(&self.session));
        let _ = std::fs::remove_dir_all(
            crate::agent::artifact_manager::get_managed_artifacts_root().join(&self.session),
        );
    }
}

#[test]
fn state_and_native_namespace_errors_explain_the_correct_shape_without_execution() {
    let f = Fixture::new();
    let (_, result) = f.execute(json!({"tool":"todo_update","params":{"items":[]}}));
    assert!(!result.success);
    assert!(result.stderr.contains("顶层数组字段"));
    assert!(result.stderr.contains("本次工具调用未执行"));
    let (_, result) = f.execute(json!({"tool":"project_resources/list_resources","params":{}}));
    assert!(!result.success);
    assert!(result.stderr.contains("params.action"));
    assert!(result.stderr.contains("本次未执行"));
    let (_, result) = f.execute(json!({"tool":"task_store","params":{"action":"list_resources"}}));
    assert!(result.success, "{}", result.stderr);
}

#[test]
fn readonly_declarations_cannot_hide_real_file_writes_or_shell_effects() {
    let f = Fixture::new();
    for container in ["command", "params"] {
        let path = format!("{container}.txt");
        let mut raw = json!({"tool":"local_file","action":"write","read_only":true,
            "metadata":{"readonly":true}});
        raw[container] = json!({"path":path,"content":"new","readonly":true,
            "meta":{"read_only":true}});
        let (inst, result) = f.execute(raw);
        assert!(is_write_or_side_effect_tool(&inst));
        assert!(result.success, "{}", result.stderr);
        assert_eq!(std::fs::read_to_string(f.workspace.join(&path)).unwrap(), "new");

        let (inst, result) = f.execute(json!({"tool":"local_file","action":"read",
            "params":{"path":path,"read_only":true}}));
        assert!(!is_write_or_side_effect_tool(&inst));
        assert!(result.success);
        assert!(result.stdout.contains("new"));
    }
    let shell: AgentInstruction = serde_json::from_value(json!({"tool":"local_shell",
        "command":{"command":"rm fixture.txt","read_only":true}})).unwrap();
    assert!(is_write_or_side_effect_tool(&shell));
}

#[test]
fn host_run_ownership_survives_single_batch_parallel_and_timeout_dispatch() {
    use crate::agent::job_engine::{has_running_jobs, poll_targeted_inbox};
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let _entered = rt.enter();
    let f = Fixture::new();
    let bg = json!({"tool":"local_shell","params":{"command":"sleep 2; printf host-owned",
        "background":true,"run_id":"model-supplied-wrong-run"}});
    let cases = [
        bg.clone(),
        json!({"commands":[bg.clone()]}),
        json!({"parallel":[bg.clone(),bg]}),
        json!({"tool":"local_shell","params":{"command":"sleep 2; printf host-owned","timeout_secs":1}}),
    ];
    for (index, value) in cases.into_iter().enumerate() {
        let run_id = format!("host-run-{index}");
        let inst: AgentInstruction = serde_json::from_value(value).unwrap();
        let result = run_agent_step_internal(&f.db, &f.session, &inst, &f.registry,
            Some(&f.workspace), None, Some(&run_id), None);
        assert!(result.success, "{} {}", result.route, result.stderr);
        let ids: Vec<String> = {
            let conn = f.db.0.lock().unwrap();
            let mut stmt = conn.prepare("SELECT id FROM agent_background_jobs WHERE parent_run_id=?1 AND parent_session_id=?2").unwrap();
            stmt.query_map(rusqlite::params![run_id,f.session], |r| r.get(0)).unwrap()
                .collect::<Result<_, _>>().unwrap()
        };
        assert_eq!(ids.len(), if index == 2 { 2 } else { 1 });
        assert!(has_running_jobs(&f.db, &run_id));
        assert!(!has_running_jobs(&f.db, &f.session));
        assert!(poll_targeted_inbox(&f.db, "another-run", &ids).unwrap().is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut completed = Vec::new();
        while completed.len() < ids.len() && std::time::Instant::now() < deadline {
            completed.extend(rt.block_on(crate::agent::job_engine::wait_for_jobs(
                &f.db, &run_id, Some(&ids), 5, None)).unwrap());
        }
        assert_eq!(completed.len(), ids.len());
        assert!(completed.iter().all(|r| r.status == "succeeded" && r.result_json.contains("host-owned")));
        assert!(!has_running_jobs(&f.db, &run_id));
    }
}

#[test]
fn cancellation_targets_the_host_run_and_missing_context_never_starts_background_work() {
    let f = Fixture::new();
    let raw = json!({"tool":"local_shell","params":{"command":"sleep 30","background":true}});
    let inst: AgentInstruction = serde_json::from_value(raw.clone()).unwrap();
    let (_, result) = f.execute(raw);
    assert!(!result.success);
    assert_eq!(result.route, "local_shell:missing_run_context");
    for run in ["host-a", "host-b"] {
        let result = run_agent_step_internal(&f.db, &f.session, &inst, &f.registry,
            Some(&f.workspace), None, Some(run), None);
        assert!(result.success, "{}", result.stderr);
    }
    assert_eq!(crate::agent::job_engine::cancel_all_running_jobs(&f.db, "host-a").unwrap(), 1);
    assert!(!crate::agent::job_engine::has_running_jobs(&f.db, "host-a"));
    assert!(crate::agent::job_engine::has_running_jobs(&f.db, "host-b"));
    assert_eq!(crate::agent::job_engine::cancel_all_running_jobs(&f.db, "host-b").unwrap(), 1);
}

#[test]
fn action_contract_defaults_and_explicit_params_match_real_dispatch() {
    let f = Fixture::new();
    std::fs::write(f.workspace.join("note.md"), "# 原文\n\n**保留**").unwrap();
    let (read, result) = f.execute(json!({"tool":"local_file","command":{"path":"note.md"}}));
    assert_eq!(read.declared_action(), None);
    assert_eq!(read.get_action(), "read");
    assert!(!is_write_or_side_effect_tool(&read));
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("**保留**"));

    for container in ["params", "command"] {
        let path = format!("{container}.md");
        let mut request = json!({"tool":"local_file"});
        request[container] = json!({"action":"write","path":path,"content":"# 新文件\n"});
        let (write, result) = f.execute(request);
        assert_eq!(write.declared_action().as_deref(), Some("write"));
        assert_eq!(write.get_action(), "write");
        assert!(is_write_or_side_effect_tool(&write));
        assert!(result.success, "{}", result.stderr);
        assert_eq!(
            std::fs::read_to_string(f.workspace.join(path)).unwrap(),
            "# 新文件\n"
        );
    }
}

#[test]
fn action_contract_missing_write_action_does_not_guess_or_modify_files() {
    let f = Fixture::new();
    std::fs::write(f.workspace.join("keep.md"), "用户原文").unwrap();
    for path in ["keep.md", "new.md"] {
        for explicit in [None, Some("local_file")] {
            let mut raw = json!({"tool":"local_file","command":{"path":path,"content":"新正文"}});
            if let Some(action) = explicit {
                raw["action"] = json!(action);
            }
            let (instruction, result) = f.execute(raw);
            assert_eq!(instruction.declared_action().as_deref(), explicit);
            assert!(!result.success);
            assert_eq!(
                result.route,
                crate::agent::tools::input_validation::INVALID_INPUT_ROUTE
            );
            assert!(!result.stderr.contains("你把"));
            assert!(result.stderr.contains("未修改"));
            assert!(result.stderr.contains("action='write'"));
            assert_eq!(
                std::fs::read_to_string(f.workspace.join("keep.md")).unwrap(),
                "用户原文"
            );
            assert!(!f.workspace.join("new.md").exists());
        }
    }
}

#[test]
fn action_contract_artifact_default_does_not_infer_registration_from_path() {
    let f = Fixture::new();
    let file = f.workspace.join("report.md");
    std::fs::write(&file, "# 报告\n").unwrap();
    let (instruction, result) =
        f.execute(json!({"tool":"artifact_manager","params":{"path":file}}));
    assert_eq!(instruction.declared_action(), None);
    assert_eq!(instruction.get_action(), "create_document");
    assert!(!result.success);
    assert!(result.stderr.contains("默认 create_document"));
    assert!(result.stderr.contains("register_file"));
    let count: i64 =
        f.db.0
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM agent_artifacts", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
    for tool in ["artifact_manager", "artifact_store"] {
        let (instruction, result) =
            f.execute(json!({"tool":tool,"params":{"action":"register_file","path":file}}));
        assert_eq!(instruction.get_action(), "register_file");
        assert!(result.success, "{}", result.stderr);
    }
    let (instruction, result) = f.execute(json!({"tool":"artifact_manager","params":{"title":"created.md","content":"# 正文\n\n**加粗**"}}));
    assert_eq!(instruction.get_action(), "create_document");
    assert!(result.success, "{}", result.stderr);
    let result: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    let path = result["canonical_path"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "# 正文\n\n**加粗**");
}

#[test]
fn action_contract_fallback_tmp_write_succeeds_without_approving_the_project() {
    let f = Fixture::new();
    let context = SandwichContext::new("system".into(), "生成复杂 Markdown 测试文件".into());
    let mut checkpoint = AgentCheckpoint::new("action-contract-run".into(), &context);
    checkpoint.plan_mode = PlanExecutionMode::Auto;
    checkpoint.triage_verdict = Some(TriageVerdict::fallback("分流模型调用超时(>45s)"));
    let raw = json!({"tool":"local_file","action":"write","command":{"path":"tmp/complex_test.md","content":"# 测试\n\n**加粗**\n"}});
    let instruction = serde_json::from_value(raw.clone()).unwrap();
    let required = |cp: &AgentCheckpoint, inst: &AgentInstruction| {
        crate::agent::plan_gate::pre_write_plan_required(
            cp,
            &context,
            inst,
            &f.session,
            &f.workspace,
        )
    };
    assert!(!required(&checkpoint, &instruction));
    let (_, result) = f.execute(raw);
    assert!(result.success, "{}", result.stderr);
    assert!(checkpoint.submitted_plan.is_none());
    assert!(!checkpoint.plan_phase.is_approved());
    let output = crate::agent::path_guard::get_session_tmp_dir(&f.session).join("complex_test.md");
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "# 测试\n\n**加粗**\n"
    );

    let source = serde_json::from_value(json!({"tool":"local_file","action":"write","params":{"path":"src/main.rs","content":"fn main() {}"}})).unwrap();
    assert!(required(&checkpoint, &source));
    for mode in [
        PlanExecutionMode::PlanApproval,
        PlanExecutionMode::CrossReview,
    ] {
        checkpoint.plan_mode = mode;
        assert!(required(&checkpoint, &instruction));
    }
    checkpoint.plan_mode = PlanExecutionMode::Auto;
    let mut verdict = TriageVerdict::fallback("test");
    verdict.fallback = None;
    checkpoint.triage_verdict = Some(verdict);
    assert!(
        required(&checkpoint, &instruction),
        "有效的技术方案判决仍须遵守"
    );
    checkpoint.triage_verdict = Some(TriageVerdict::fallback("test"));
    checkpoint.stage_review_blockers_open = 1;
    assert!(crate::agent::plan_gate::stage_blocker_interception(&checkpoint, &f.db).is_some());
}

#[test]
fn action_contract_fallback_tmp_exemption_rejects_escape_and_other_sessions() {
    let f = Fixture::new();
    let root = crate::agent::path_guard::get_session_tmp_dir(&f.session);
    for path in [
        "tmp/../outside.md".to_string(),
        "session-tmp://../outside.md".into(),
        crate::agent::path_guard::get_session_tmp_dir("other-session")
            .join("report.md")
            .to_string_lossy()
            .into_owned(),
        f.workspace.join("note.md").to_string_lossy().into_owned(),
    ] {
        let inst = serde_json::from_value(
            json!({"tool":"local_file","action":"write","params":{"path":path,"content":"x"}}),
        )
        .unwrap();
        assert!(!crate::agent::path_guard::is_confined_session_tmp_write(
            &inst,
            &f.session,
            &f.workspace
        ));
    }
    assert!(!root.exists(), "适用性检查不得创建临时目录");
    for path in [
        "tmp/nested/new.md".to_string(),
        "session-tmp://nested/new.md".into(),
        root.join("nested/new.md").to_string_lossy().into_owned(),
    ] {
        let inst = serde_json::from_value(
            json!({"tool":"local_file","action":"write","params":{"path":path,"content":"x"}}),
        )
        .unwrap();
        assert!(crate::agent::path_guard::is_confined_session_tmp_write(
            &inst,
            &f.session,
            &f.workspace
        ));
    }
    let relative = serde_json::from_value(
        json!({"tool":"local_file","action":"write","params":{"path":"new.md","content":"x"}}),
    )
    .unwrap();
    assert!(crate::agent::path_guard::is_confined_session_tmp_write(
        &relative, &f.session, &root
    ));
    assert!(!root.exists(), "不存在的嵌套目标也应无副作用判定");
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&f.workspace, root.join("workspace-link")).unwrap();
        let inst = serde_json::from_value(json!({"tool":"local_file","action":"write","params":{"path":"tmp/workspace-link/note.md","content":"x"}})).unwrap();
        assert!(!crate::agent::path_guard::is_confined_session_tmp_write(
            &inst,
            &f.session,
            &f.workspace
        ));
    }
}
