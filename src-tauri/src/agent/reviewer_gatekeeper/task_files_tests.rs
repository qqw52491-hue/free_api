use super::*;
use crate::agent::child_reviewer::retirement_tests::MockReviewer;
use chrono::{Duration, Utc};

struct Fixture {
    mock: MockReviewer,
    sid: String,
    tmp: std::path::PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let mock = MockReviewer::new(vec![json!({"verdict":"passed","score":90,
            "blocking_issues":[],"nit_issues":[],"summary":"核对实际材料"})])
        .await;
        let sid = format!("task-files-{}", uuid::Uuid::new_v4());
        let tmp = crate::agent::path_guard::get_session_tmp_dir(&sid);
        std::fs::create_dir_all(tmp.join("jobs")).unwrap();
        Self { mock, sid, tmp }
    }
    fn context(&self) -> SandwichContext {
        SandwichContext::new("sys".into(), "当前任务".into())
    }
    fn files(
        &self,
        context: &SandwichContext,
        start: chrono::DateTime<Utc>,
        paths: &[String],
    ) -> TaskReviewFiles {
        task_review_files(
            &self.mock.db,
            &self.sid,
            &self.mock.root,
            context,
            start,
            paths,
        )
    }
    fn log(&self, run: &str, content: &str) -> std::path::PathBuf {
        let id = crate::agent::job_engine::begin_job(
            &self.mock.db,
            run,
            &self.sid,
            "shell",
            "probe",
            "{}",
        )
        .unwrap();
        let path = self.tmp.join("jobs").join(format!("{id}.log"));
        std::fs::write(&path, content).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

#[tokio::test]
async fn host_logs_never_upgrade_read_only_finish_in_either_review_mode() {
    let f = Fixture::new().await;
    let log = f.log("previous-run", "");
    let context = f.context();
    let baseline = GitBaseline::non_git();
    let start = Utc::now() - Duration::hours(1);
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.final_review_mode = "mechanical_only".into();
    // 自由会话 workspace == tmp 也不能经基线补扫把日志重新放进材料。
    let (before, files) =
        task_review_snapshot(&f.mock.db, &f.sid, &f.tmp, &context, &baseline, start, &[]);
    assert!(files.paths.is_empty());
    assert!(before.material.is_empty());
    for committee in [false, true] {
        let routing = ModelRoutingConfig {
            stage_review_committee: committee,
            ..Default::default()
        };
        let result = evaluate_finish_diff(
            app.handle(),
            &f.mock.db,
            &context,
            &routing,
            1,
            &f.tmp,
            &baseline,
            start,
            &f.sid,
            &mut rx,
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            Some(&triage),
            None,
            Some("只读配置解答"),
            &[],
        )
        .await;
        assert!(result.is_passed(), "{result:?}");
    }
    assert!(
        f.mock.requests.lock().unwrap().is_empty(),
        "不能追加模型终审"
    );
    std::fs::write(log, "后台后来输出了更多内容").unwrap();
    let after = task_review_snapshot(&f.mock.db, &f.sid, &f.tmp, &context, &baseline, start, &[]).0;
    assert_eq!(before.hash, after.hash, "未交付的日志增长不改变交付材料");
}

#[tokio::test]
async fn registered_products_are_checked_without_scanning_unregistered_tmp_files() {
    let f = Fixture::new().await;
    let start = Utc::now() - Duration::seconds(1);
    for _ in 0..30 {
        f.log("same-run", "");
    }
    std::fs::write(f.tmp.join("broken.xlsx"), "not a zip").unwrap();
    std::fs::write(f.tmp.join("jobs/user-report.log"), "").unwrap();
    std::fs::write(f.tmp.join("unrelated-empty.log"), "").unwrap();
    let mut context = f.context();
    context.record_side_effect_with_paths(1, "local_file".into(), "reports".into(),
        vec!["tmp/broken.xlsx".into(), "tmp/jobs/user-report.log".into()]);
    let files = f.files(&context, start, &[]);
    assert_eq!(
        files.paths.len(),
        2,
        "已登记的用户日志仍检查；未登记临时文件不自动进入审查"
    );
    let anomalies = sniff_task_file_anomalies(&files.paths).join("\n");
    assert!(anomalies.contains("broken.xlsx"));
    assert!(anomalies.contains("user-report.log"));
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.final_review_mode = "mechanical_only".into();
    assert!(super::finish_diff::finish_review_bypass(
        &f.context(),
        &f.sid,
        start,
        Some(&triage),
        &sniff_task_file_anomalies(&files.paths),
        true
    )
    .is_none());
}

#[tokio::test]
async fn logical_task_resume_keeps_products_and_new_task_drops_history() {
    let f = Fixture::new().await;
    let start = Utc::now() - Duration::days(1);
    std::fs::write(f.tmp.join("prior-run-report.md"), "已有成果").unwrap();
    let mut context = f.context();
    context.record_side_effect_with_paths(
        1,
        "local_file".into(),
        "已有成果".into(),
        vec!["tmp/prior-run-report.md".into()],
    );
    let mut cp = crate::agent::checkpoint::AgentCheckpoint::new(f.sid.clone(), &context);
    cp.task_started_at = Some(start);
    cp.task_git_baseline = Some(GitBaseline::non_git());
    let encoded = serde_json::to_string(&cp).unwrap();
    let mut restored: crate::agent::checkpoint::AgentCheckpoint =
        serde_json::from_str(&encoded).unwrap();
    let mut resumed = SandwichContext::new("sys".into(), "换模型继续".into());
    restored.restore_task_for_entry(&mut resumed, "继续");
    assert_eq!(context.history_task_id, resumed.history_task_id);
    assert_eq!(
        f.files(&resumed, restored.task_started_at.unwrap(), &[])
            .paths
            .len(),
        1
    );
    let mut fresh = f.context();
    restored.data.inherit_filling_into(&mut fresh);
    assert_ne!(fresh.history_task_id, resumed.history_task_id);
    assert!(f
        .files(&fresh, Utc::now() + Duration::seconds(1), &[])
        .paths
        .is_empty());
    let explicit = vec!["tmp/prior-run-report.md".into()];
    assert_eq!(
        f.files(&fresh, Utc::now() + Duration::seconds(1), &explicit)
            .paths
            .len(),
        1
    );
}

#[tokio::test]
async fn registered_managed_delivery_is_read_without_widening_directory_permissions() {
    let f = Fixture::new().await;
    let record = crate::agent::artifact_manager::save_managed_artifact(
        &f.mock.db,
        &f.sid,
        Some("old-run"),
        "旧报告",
        "受管报告正文",
        Some("md"),
        None,
        None,
    )
    .unwrap();
    let payload = json!({"artifacts":[{"artifact_id":record.id}]});
    let delivered = delivery_file_paths(&f.mock.db, &f.sid, &payload);
    let selected = f.files(&f.context(), Utc::now() + Duration::hours(1), &delivered);
    assert_eq!(
        selected.paths,
        vec![std::path::PathBuf::from(&record.canonical_path)]
    );
    assert!(selected.notices.is_empty());
    assert!(delivery_file_paths(&f.mock.db, "another-session", &payload).is_empty());
    assert!(
        crate::agent::path_guard::ensure_readable_path(
            &record.canonical_path,
            &f.mock.root,
            Some(&f.sid),
            false
        )
        .is_err(),
        "没有放宽原有通用目录权限"
    );
    crate::agent::artifact_manager::delete_artifact(&f.mock.db, &record.id).unwrap();
}

#[tokio::test]
async fn explicit_old_files_and_host_logs_are_checked_and_fully_fingerprinted() {
    let f = Fixture::new().await;
    let log = f.log("old-run", "");
    let context = f.context();
    let future = Utc::now() + Duration::hours(1);
    let delivered = vec![log.to_string_lossy().into_owned()];
    let selected = f.files(&context, future, &delivered);
    assert_eq!(selected.paths, vec![log.clone()]);
    assert_eq!(sniff_task_file_anomalies(&selected.paths).len(), 1);
    let path = f.mock.root.join("old-report.md");
    let prefix = "原始报告内容".repeat(1500);
    std::fs::write(&path, format!("{prefix}OLD")).unwrap();
    let delivered = vec![path.to_string_lossy().into_owned()];
    let before = task_review_snapshot(
        &f.mock.db,
        &f.sid,
        &f.mock.root,
        &context,
        &GitBaseline::non_git(),
        future,
        &delivered,
    )
    .0;
    std::fs::write(&path, format!("{prefix}NEW")).unwrap();
    let after = task_review_snapshot(
        &f.mock.db,
        &f.sid,
        &f.mock.root,
        &context,
        &GitBaseline::non_git(),
        future,
        &delivered,
    )
    .0;
    assert_ne!(before.hash, after.hash, "变化在展示节选之外仍须影响指纹");
    assert!(!after.changed_paths_since(&before).is_empty());
}

#[tokio::test]
async fn recorded_tmp_write_survives_old_mtime_and_other_session_is_not_read() {
    let f = Fixture::new().await;
    let path = f.tmp.join("preserved-mtime.md");
    std::fs::write(&path, "本任务写入").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86400)),
        )
        .unwrap();
    let mut context = f.context();
    context.record_side_effect_with_paths(
        1,
        "local_file".into(),
        "写入".into(),
        vec!["tmp/preserved-mtime.md".into()],
    );
    assert_eq!(
        f.files(&context, Utc::now() - Duration::seconds(1), &[])
            .paths,
        vec![path]
    );
    let other =
        crate::agent::path_guard::get_session_tmp_dir("other-task-files-session").join("secret.md");
    let selected = f.files(
        &f.context(),
        Utc::now() + Duration::hours(1),
        &[other.to_string_lossy().into_owned()],
    );
    assert!(selected.paths.is_empty());
    assert!(!selected.notices.is_empty());
}

#[tokio::test]
async fn explicit_delivery_reaches_completion_review_and_content_change_invalidates_cache() {
    use crate::agent::finalization::{evaluate_completion, CompletionMode};
    let f = Fixture::new().await;
    let report = f.mock.root.join("old-delivery.md");
    std::fs::write(&report, "早先生成的交付正文").unwrap();
    let mut context = f.context();
    let mut cp = crate::agent::checkpoint::AgentCheckpoint::new(f.sid.clone(), &context);
    let baseline = GitBaseline::non_git();
    let mut routing = ModelRoutingConfig::default();
    routing.role_models.insert("reviewer".into(), "mock".into());
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let inst: crate::agent::types::AgentInstruction =
        serde_json::from_value(json!({"tool":"finish",
        "command":{"summary":"再次交付已有报告","artifacts":[{"path":report}]}}))
        .unwrap();
    let start = Utc::now() + Duration::seconds(1);
    for i in 0..3 {
        if i == 2 {
            std::fs::write(&report, "交付正文发生变化，需要重新核对").unwrap();
        }
        let result = evaluate_completion(
            app.handle(),
            &f.mock.db,
            &mut context,
            &mut cp,
            "delivery-run",
            &f.sid,
            i + 1,
            &f.mock.root,
            &baseline,
            start,
            &routing,
            &mut rx,
            &inst,
            CompletionMode::Finish,
            false,
            false,
        )
        .await;
        assert!(result.receipt.is_ready(), "{:?}", result.receipt);
        cp.pending_finalization = Some(result.receipt);
        assert_eq!(
            f.mock.requests.lock().unwrap().len(),
            if i < 2 { 1 } else { 2 }
        );
    }
    let requests = f.mock.requests.lock().unwrap();
    assert!(requests[0].to_string().contains("早先生成的交付正文"));
    assert!(requests[1].to_string().contains("交付正文发生变化"));
}
