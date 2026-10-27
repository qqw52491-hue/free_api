use super::*;
use crate::{
    agent::{
        dispatcher::{is_write_or_side_effect_tool, run_agent_step_with_images},
        mcp::PluginRegistry,
        reviewer_gatekeeper::GitBaseline,
    },
    db::{init_db, DbState},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Fixture {
    root: PathBuf,
    db: DbState,
    context: SandwichContext,
    checkpoint: AgentCheckpoint,
}

#[test]
fn git_leaf_guard_preserves_staged_deletion_and_opposite_index_worktree_changes() {
    let mut f = Fixture::new();
    let path = "config/nested/用户 设置.properties";
    // 使用 --cached 保留工作区文件，之后再移除；这是用户已有的暂存删除。
    assert!(Command::new("git")
        .args(["rm", "--cached", "-q", "--", path])
        .current_dir(&f.root)
        .status()
        .unwrap()
        .success());
    std::fs::remove_file(f.root.join(path)).unwrap();
    f.checkpoint.task_git_baseline = Some(GitBaseline::capture_at_run_start(&f.root));
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git checkout HEAD -- config/"}}),
        &f.guard(),
    );
    assert!(!result.success);
    assert!(!f.root.join(path).exists());

    std::fs::write(f.root.join("owned.rs"), "staged user edit").unwrap();
    assert!(Command::new("git")
        .args(["add", "owned.rs"])
        .current_dir(&f.root)
        .status()
        .unwrap()
        .success());
    std::fs::write(f.root.join("owned.rs"), "committed").unwrap();
    f.checkpoint.task_git_baseline = Some(GitBaseline::capture_at_run_start(&f.root));
    assert!(f
        .checkpoint
        .task_git_baseline
        .as_ref()
        .unwrap()
        .pre_dirty
        .contains_key("owned.rs"));
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git checkout -- owned.rs"}}),
        &f.guard(),
    );
    assert!(!result.success);
    assert_eq!(
        std::fs::read_to_string(f.root.join("owned.rs")).unwrap(),
        "committed"
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("git-leaf-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("config/nested")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("config/nested/用户 设置.properties"), "committed").unwrap();
        std::fs::write(root.join("owned.rs"), "committed").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["add", "."],
            vec!["commit", "-qm", "fixture"],
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::write(
            root.join("config/nested/用户 设置.properties"),
            "preexisting user change",
        )
        .unwrap();
        let context = SandwichContext::new("SYS".into(), "fixture".into());
        let mut checkpoint = AgentCheckpoint::new("git-leaf-guard".into(), &context);
        checkpoint.task_git_baseline = Some(GitBaseline::capture_at_run_start(&root));
        checkpoint.approved_target_files = Some(vec![
            "config/nested/用户 设置.properties".into(),
            "owned.rs".into(),
        ]);
        let db = DbState(Arc::new(Mutex::new(
            init_db(&PathBuf::from(":memory:")).unwrap(),
        )));
        Self {
            root,
            db,
            context,
            checkpoint,
        }
    }
    fn guard(&self) -> Arc<ShellExecutionGuard> {
        Arc::new(ShellExecutionGuard::capture(
            &self.context,
            &self.checkpoint,
            &self.root,
        ))
    }

    fn approve(&mut self, command: &str) {
        let guard = self.guard();
        assert!(
            !self
                .run(
                    json!({"tool":"local_shell","params":{"command":command}}),
                    &guard
                )
                .success
        );
        guard.record_pending(&mut self.checkpoint);
        self.checkpoint
            .git_destructive_mark_asked(&format!("确认执行 `{command}`？"));
        self.checkpoint
            .register_human_reply("是否执行所问操作", "确认执行", false);
        let decision = crate::agent::human_reply::HumanReplyDecision {
            reply_id: self
                .checkpoint
                .pending_human_reply
                .as_ref()
                .unwrap()
                .reply_id
                .clone(),
            git_decisions: vec![crate::agent::human_reply::GitReplyDecision {
                command: command.into(),
                approved: true,
            }],
            plan_approved: None,
            reason: "用户授权所问操作".into(),
        };
        self.checkpoint
            .apply_human_reply_decision(&self.db, &mut self.context, &decision)
            .unwrap();
    }
    fn run(&self, raw: Value, guard: &Arc<ShellExecutionGuard>) -> DispatchResult {
        let mut raw = raw;
        crate::agent::utils::normalize_raw_instruction_value(&mut raw);
        let inst = serde_json::from_value(raw).unwrap();
        run_agent_step_with_images(
            &self.db,
            "git-leaf-guard",
            &inst,
            &PluginRegistry::new(),
            Some(&self.root),
            &Default::default(),
            None,
            Some(guard),
        )
    }
    fn assert_original(&self) {
        assert_eq!(
            std::fs::read_to_string(self.root.join("config/nested/用户 设置.properties")).unwrap(),
            "preexisting user change"
        );
    }
}

#[test]
fn git_leaf_guard_model_permission_is_bound_to_directory_and_target_content() {
    let command = "git checkout -- config/";
    let mut f = Fixture::new();
    f.approve(command);
    let mut second = Fixture::new();
    second.checkpoint.git_destructive_approved_scopes =
        f.checkpoint.git_destructive_approved_scopes.clone();
    assert!(
        !second
            .run(
                json!({"tool":"local_shell","params":{"command":command}}),
                &second.guard()
            )
            .success
    );
    second.assert_original();
    // 无关文件变化不影响已问操作；相关文件的内容变化必须重新取得许可。
    std::fs::write(f.root.join("unrelated.txt"), "background output").unwrap();
    let original_scope = git_operation_scope(command, &f.root).unwrap();
    assert_eq!(
        f.checkpoint.git_destructive_approved_scopes[command],
        original_scope
    );
    std::fs::write(
        f.root.join("config/nested/用户 设置.properties"),
        "new user change",
    )
    .unwrap();
    assert_ne!(
        git_operation_scope(command, &f.root).unwrap(),
        original_scope
    );
    assert!(
        !f.run(
            json!({"tool":"local_shell","params":{"command":command}}),
            &f.guard()
        )
        .success
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("config/nested/用户 设置.properties")).unwrap(),
        "new user change"
    );
}

#[test]
fn git_leaf_guard_does_not_trust_legacy_keyword_confirmations() {
    let mut f = Fixture::new();
    f.checkpoint
        .git_destructive_confirmed
        .push("git checkout -- config/".into());
    assert!(
        !f.run(
            json!({"tool":"local_shell","params":{"command":"git checkout -- config/"}}),
            &f.guard()
        )
        .success
    );
    f.assert_original();
    assert_ne!(
        crate::agent::dispatcher::git_cmd_fingerprint("git checkout -- 'a  b.txt'"),
        crate::agent::dispatcher::git_cmd_fingerprint("git checkout -- 'a b.txt'")
    );
}

#[test]
fn git_leaf_guard_origin_notice_distinguishes_untouched_existing_changes() {
    let f = Fixture::new();
    let baseline = f.checkpoint.task_git_baseline.as_ref().unwrap();
    let notice = baseline.origin_notice(&f.root);
    assert!(notice.contains("当前内容仍与起点一致"));
    assert!(notice.contains(baseline.base_head.as_ref().unwrap()));
    std::fs::write(
        f.root.join("config/nested/用户 设置.properties"),
        "existing + task edit",
    )
    .unwrap();
    assert!(baseline
        .origin_notice(&f.root)
        .contains("当前内容与起点不同"));
}

#[test]
fn git_leaf_guard_restores_owned_file_to_task_start_and_preserves_the_index() {
    let f = Fixture::new();
    let path = "config/nested/用户 设置.properties";
    let base = f
        .checkpoint
        .task_git_baseline
        .as_ref()
        .unwrap()
        .base_head
        .as_ref()
        .unwrap();
    // Agent 在用户原有内容上叠加修改。
    std::fs::write(f.root.join(path), "preexisting user change + task edit").unwrap();
    let before_index = Command::new("git")
        .args(["ls-files", "-s", "-z"])
        .current_dir(&f.root)
        .output()
        .unwrap()
        .stdout;
    let command =
        format!("git restore --source={base} --worktree -- 'config/nested/用户 设置.properties'");
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":command}}),
        &f.guard(),
    );
    assert!(result.success, "{}", result.stderr);
    f.assert_original();
    let after_index = Command::new("git")
        .args(["ls-files", "-s", "-z"])
        .current_dir(&f.root)
        .output()
        .unwrap()
        .stdout;
    assert_eq!(before_index, after_index, "只恢复工作区，不改暂存区");
    assert!(!restores_task_baseline(
        &format!("git restore --source={base} --staged --worktree -- config/"),
        base
    ));
    assert!(!restores_task_baseline(
        "git restore --source=HEAD --worktree -- config/",
        base
    ));
}

#[test]
fn git_leaf_guard_never_treats_a_head_fallback_as_a_verified_workspace_snapshot() {
    let mut f = Fixture::new();
    let snapshot = f
        .checkpoint
        .task_git_baseline
        .as_ref()
        .unwrap()
        .base_head
        .clone()
        .unwrap();
    let head = Command::new("git")
        .args(["rev-parse", &format!("{snapshot}^1")])
        .current_dir(&f.root)
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
    // 模拟快照生成失败后 base_head 回退为 HEAD，已有修改哈希仍在。
    f.checkpoint.task_git_baseline.as_mut().unwrap().base_head = Some(head.clone());
    let result = f.run(json!({"tool":"local_shell","params":{"command":format!("git restore --source={head} --worktree -- 'config/nested/用户 设置.properties'")}}), &f.guard());
    assert!(!result.success);
    f.assert_original();
}

#[test]
fn git_leaf_guard_covers_single_pipeline_parallel_and_nested_tree() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let shell = json!({"tool":"local_shell","params":{"command":"git checkout -- config/"}});
    for raw in [
        shell.clone(),
        json!({"commands":[shell.clone()]}),
        json!({"tool":"pipeline","command":{"commands":[shell.clone()]}}),
        json!({"parallel":[shell.clone()]}),
        json!({"plan_tree":[{"parallel":[shell.clone()]}]}),
    ] {
        let inst: AgentInstruction = serde_json::from_value(raw.clone()).unwrap();
        assert!(is_write_or_side_effect_tool(&inst));
        let guard = f.guard();
        let result = runtime.block_on(async { tokio::task::block_in_place(|| f.run(raw, &guard)) });
        assert!(!result.success, "{}", result.stdout);
        assert!(
            result.stderr.contains("git 破坏命令待人工确认"),
            "{}",
            result.stderr
        );
        f.assert_original();
        let mut checkpoint = f.checkpoint.clone();
        assert!(guard.record_pending(&mut checkpoint));
        assert!(checkpoint
            .git_destructive_pending
            .contains(&"git checkout -- config/".to_string()));
    }
}

#[test]
fn git_leaf_guard_uses_git_pathspec_and_cwd_and_keeps_original_even_if_approved() {
    let f = Fixture::new();
    for (command, cwd) in [
        ("git restore -- config/", "."),
        ("git checkout -- 'config/**/*.properties'", "."),
        ("git checkout -- ':(top)config/'", "config/nested"),
        ("git checkout -- '用户 设置.properties'", "config/nested"),
    ] {
        let result = f.run(
            json!({"tool":"local_shell","params":{"command":command,"cwd":cwd}}),
            &f.guard(),
        );
        assert!(!result.success, "{command}: {}", result.stdout);
        f.assert_original();
    }
    // 同一文件叠加 Agent 修改，不赋予销毁原有内容的权限。
    std::fs::write(
        f.root.join("config/nested/用户 设置.properties"),
        "preexisting user change + agent edit",
    )
    .unwrap();
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git restore -- config/"}}),
        &f.guard(),
    );
    assert!(!result.success);
    assert_eq!(
        std::fs::read_to_string(f.root.join("config/nested/用户 设置.properties")).unwrap(),
        "preexisting user change + agent edit"
    );
}

#[test]
fn git_leaf_guard_allows_owned_and_clean_targets_but_checks_every_shell_segment() {
    let f = Fixture::new();
    std::fs::write(f.root.join("owned.rs"), "agent change").unwrap();
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git checkout -- owned.rs"}}),
        &f.guard(),
    );
    assert!(result.success, "{}", result.stderr);
    assert_eq!(
        std::fs::read_to_string(f.root.join("owned.rs")).unwrap(),
        "committed"
    );
    // 首段是合法清理，不能掩盖后段的外来还原。
    let result = f.run(json!({"tool":"local_shell","params":{"command":"git checkout -- owned.rs && git checkout -- config/"}}), &f.guard());
    assert!(!result.success);
    f.assert_original();
    let mut guard = ShellExecutionGuard::capture(&f.context, &f.checkpoint, &f.root);
    guard.allowed.clear();
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git checkout -- owned.rs"}}),
        &Arc::new(guard),
    );
    assert!(result.success, "{}", result.stderr);
}

#[test]
fn git_leaf_guard_pipeline_keeps_completed_results_and_never_runs_later_steps() {
    let f = Fixture::new();
    let result = f.run(
        json!({"commands":[
            {"tool":"local_shell","params":{"command":"printf earlier-completed"}},
            {"tool":"local_shell","params":{"command":"git checkout -- config/"}},
            {"tool":"local_shell","params":{"command":"touch later-marker"}}
        ]}),
        &f.guard(),
    );
    assert!(!result.success);
    assert!(result.stdout.contains("earlier-completed"));
    assert!(!f.root.join("later-marker").exists());
    f.assert_original();
}

#[test]
fn git_leaf_guard_confirmation_round_trip_and_invalid_params() {
    let mut f = Fixture::new();
    let guard = f.guard();
    let raw = json!({"tool":"local_shell","params":{"command":"git checkout -- config/"}});
    assert!(!f.run(raw.clone(), &guard).success);
    guard.record_pending(&mut f.checkpoint);
    let json = serde_json::to_string(&f.checkpoint).unwrap();
    f.checkpoint = serde_json::from_str(&json).unwrap();
    f.checkpoint
        .git_destructive_mark_asked("是否执行 `git checkout -- config/`？");
    f.checkpoint
        .register_human_reply("是否执行 git checkout -- config/", "确认执行", false);
    let reply_id = f
        .checkpoint
        .pending_human_reply
        .as_ref()
        .unwrap()
        .reply_id
        .clone();
    let decision = crate::agent::human_reply::HumanReplyDecision {
        reply_id,
        git_decisions: vec![crate::agent::human_reply::GitReplyDecision {
            command: "git checkout -- config/".into(),
            approved: true,
        }],
        plan_approved: None,
        reason: "用户明确确认所问命令".into(),
    };
    f.checkpoint
        .apply_human_reply_decision(&f.db, &mut f.context, &decision)
        .unwrap();
    assert!(f.run(raw, &f.guard()).success);
    assert_eq!(
        std::fs::read_to_string(f.root.join("config/nested/用户 设置.properties")).unwrap(),
        "committed"
    );
    let guard = f.guard();
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":21}}),
        &guard,
    );
    assert!(!result.success);
    assert!(!guard.record_pending(&mut f.checkpoint));
}

#[test]
fn git_leaf_guard_baseline_preserves_exact_names_and_readonly_batches_stay_readonly() {
    let f = Fixture::new();
    let names = [" leading space.txt", "line\nbreak.txt", "引号\".txt"];
    for name in names {
        std::fs::write(f.root.join(name), "existing untracked").unwrap();
    }
    let baseline = GitBaseline::capture_at_run_start(&f.root);
    for name in names {
        assert!(baseline.pre_dirty.contains_key(name), "{name:?}");
        assert_ne!(baseline.pre_dirty[name], "<missing>");
    }
    assert!(baseline
        .pre_dirty
        .contains_key("config/nested/用户 设置.properties"));
    let read = json!({"tool":"local_file","params":{"action":"read","path":"owned.rs"}});
    for raw in [
        json!({"commands":[read.clone()]}),
        json!({"parallel":[read.clone()]}),
        json!({"plan_tree":[{"parallel":[read]}]}),
    ] {
        let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
        assert!(!is_write_or_side_effect_tool(&inst));
    }
}

#[test]
fn git_leaf_guard_honors_legacy_quoted_baseline_without_rewriting_it() {
    let mut f = Fixture::new();
    let output = Command::new("git")
        .args(["-c", "core.quotePath=true", "diff", "--name-only", "HEAD"])
        .current_dir(&f.root)
        .output()
        .unwrap();
    let quoted = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(quoted.starts_with('"') && quoted.contains("\\"));
    let baseline = f.checkpoint.task_git_baseline.as_mut().unwrap();
    baseline.pre_dirty.clear();
    baseline
        .pre_dirty
        .insert(quoted.clone(), "<missing>".into());
    let result = f.run(
        json!({"tool":"local_shell","params":{"command":"git checkout -- config/"}}),
        &f.guard(),
    );
    assert!(!result.success);
    f.assert_original();
    assert_eq!(
        f.checkpoint.task_git_baseline.as_ref().unwrap().pre_dirty[&quoted],
        "<missing>"
    );
    assert!(decode_legacy_git_path("\"\\400\"").is_none());
    assert!(decode_legacy_git_path("\"\\q\"").is_none());
}

#[cfg(unix)]
#[test]
fn git_leaf_guard_uses_physical_workspace_alias() {
    let f = Fixture::new();
    let alias = f.root.with_extension("alias");
    std::os::unix::fs::symlink(&f.root, &alias).unwrap();
    let guard = Arc::new(ShellExecutionGuard::capture(
        &f.context,
        &f.checkpoint,
        &alias,
    ));
    let inst: AgentInstruction = serde_json::from_value(
        json!({"tool":"local_shell","params":{"command":"git checkout -- config/"}}),
    )
    .unwrap();
    let result = run_agent_step_with_images(
        &f.db,
        "git-leaf-guard",
        &inst,
        &PluginRegistry::new(),
        Some(&alias),
        &Default::default(),
        None,
        Some(&guard),
    );
    std::fs::remove_file(&alias).unwrap();
    assert!(!result.success);
    assert!(result.stderr.contains("git 破坏命令待人工确认"));
    f.assert_original();
}
