use super::*;
use crate::agent::{
    checkpoint::{load_checkpoint, save_checkpoint},
    context::SandwichContext,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

struct Fixture {
    root: PathBuf,
    primary: PathBuf,
    extra: PathBuf,
    db: crate::db::DbState,
    sid: String,
    context: SandwichContext,
    cp: AgentCheckpoint,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("task-start-{}", uuid::Uuid::new_v4()));
        let primary = root.join("primary");
        let extra = root.join("extra");
        for p in [&primary, &extra] {
            std::fs::create_dir_all(p).unwrap();
            init(p);
        }
        let db = crate::db::DbState(Arc::new(Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        let sid = format!("task-start-{}", uuid::Uuid::new_v4());
        let context = SandwichContext::new("sys".into(), "task".into());
        let mut cp = AgentCheckpoint::new(sid.clone(), &context);
        cp.task_git_baseline = Some(GitBaseline::capture_at_run_start(&primary));
        save_checkpoint(&db, &mut cp).unwrap();
        workspace_access::bind_conversation(&sid, &primary, vec![]);
        Self {
            root,
            primary,
            extra,
            db,
            sid,
            context,
            cp,
        }
    }
    fn dir(&self, access: DirectoryAccess) -> ProjectDirectory {
        ProjectDirectory {
            path: self.extra.to_string_lossy().into_owned(),
            access,
        }
    }
    fn grant(&self, access: DirectoryAccess) {
        let dirs = vec![self.dir(access)];
        let conn = self.db.0.lock().unwrap();
        capture_new_writable_directories_on(&conn, &self.sid, &self.primary, &dirs).unwrap();
        workspace_access::bind_conversation(&self.sid, &self.primary, dirs);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        workspace_access::revoke_session(&self.sid);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn init(root: &Path) {
    git(root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), "original\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
}

#[test]
fn dynamic_grant_is_durable_before_write_and_survives_stale_save() {
    let mut f = Fixture::new();
    f.grant(DirectoryAccess::ReadOnly);
    assert!(load_checkpoint(&f.db, &f.sid)
        .unwrap()
        .unwrap()
        .task_additional_git_baselines
        .is_empty());
    std::fs::write(f.extra.join("a.txt"), "USER\n").unwrap();
    f.grant(DirectoryAccess::ReadWrite);
    let saved = load_checkpoint(&f.db, &f.sid).unwrap().unwrap();
    let start =
        saved.task_additional_git_baselines[&f.dir(DirectoryAccess::ReadWrite).path].clone();
    assert!(start.unchanged_since_baseline(&f.extra, "a.txt"));
    std::fs::write(f.extra.join("a.txt"), "USER\nAGENT\n").unwrap();
    save_checkpoint(&f.db, &mut f.cp).unwrap(); // 内存仍是授权前的副本。
    let restored = load_checkpoint(&f.db, &f.sid).unwrap().unwrap();
    assert_eq!(
        restored
            .task_additional_git_baselines
            .values()
            .next()
            .unwrap()
            .base_head,
        start.base_head
    );
    f.grant(DirectoryAccess::ReadOnly);
    f.grant(DirectoryAccess::ReadWrite);
    assert_eq!(
        load_checkpoint(&f.db, &f.sid)
            .unwrap()
            .unwrap()
            .task_additional_git_baselines
            .values()
            .next()
            .unwrap()
            .base_head,
        start.base_head
    );
}

#[test]
fn review_finds_unregistered_shell_changes_and_partial_commits_in_extra_repo() {
    let f = Fixture::new();
    f.grant(DirectoryAccess::ReadWrite);
    std::fs::write(f.extra.join("a.txt"), "committed change\n").unwrap();
    git(&f.extra, &["add", "."]);
    git(&f.extra, &["commit", "-qm", "partial"]);
    std::fs::write(f.extra.join("b space 中文.txt"), "pending\n").unwrap();
    let (snapshot, files) = crate::agent::reviewer_gatekeeper::task_review_snapshot(
        &f.db,
        &f.sid,
        &f.primary,
        &f.context,
        f.cp.task_git_baseline.as_ref().unwrap(),
        chrono::Utc::now(),
        &[],
    );
    assert!(f.context.side_effects.is_empty());
    assert!(files.paths.contains(&f.extra.join("a.txt")));
    assert!(files.paths.contains(&f.extra.join("b space 中文.txt")));
    assert!(snapshot.material.contains("committed change"));
    let hash = snapshot.hash;
    std::fs::write(f.extra.join("b space 中文.txt"), "changed again\n").unwrap();
    assert_ne!(
        hash,
        crate::agent::reviewer_gatekeeper::task_review_snapshot(
            &f.db,
            &f.sid,
            &f.primary,
            &f.context,
            f.cp.task_git_baseline.as_ref().unwrap(),
            chrono::Utc::now(),
            &[]
        )
        .0
        .hash
    );
}

#[test]
fn task_boundary_and_revocation_do_not_reuse_old_material() {
    let f = Fixture::new();
    f.grant(DirectoryAccess::ReadWrite);
    std::fs::write(f.extra.join("a.txt"), "private change").unwrap();
    let new_context = SandwichContext::new("sys".into(), "new task".into());
    assert!(
        saved_task_baselines(&f.db, &f.sid, &new_context.history_task_id)
            .unwrap()
            .is_empty()
    );
    workspace_access::bind_conversation(&f.sid, &f.primary, vec![]);
    let (s, files) = crate::agent::reviewer_gatekeeper::task_review_snapshot(
        &f.db,
        &f.sid,
        &f.primary,
        &f.context,
        f.cp.task_git_baseline.as_ref().unwrap(),
        chrono::Utc::now(),
        &[],
    );
    assert!(!s.material.contains("private change"));
    assert!(files.paths.is_empty());
    assert!(!files.notices.is_empty());
}

#[test]
fn sibling_directories_in_one_repo_keep_scoped_start_and_protect_user_edits() {
    let f = Fixture::new();
    let a = f.extra.join("a");
    let b = f.extra.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("same.txt"), "A").unwrap();
    std::fs::write(b.join("same.txt"), "B").unwrap();
    git(&f.extra, &["add", "."]);
    git(&f.extra, &["commit", "-qm", "siblings"]);
    std::fs::write(b.join("same.txt"), "USER").unwrap();
    workspace_access::bind_conversation(
        &f.sid,
        &a,
        vec![ProjectDirectory {
            path: b.to_string_lossy().into_owned(),
            access: DirectoryAccess::ReadWrite,
        }],
    );
    let mut cp = AgentCheckpoint::new(f.sid.clone(), &f.context);
    cp.task_git_baseline = Some(GitBaseline::capture_at_run_start(&a));
    cp.task_additional_git_baselines = GitBaseline::capture_additional_git_baselines(&f.sid, &a);
    assert!(
        cp.task_additional_git_baselines[&b.to_string_lossy().into_owned()]
            .pre_dirty
            .contains_key("same.txt")
    );
    let mut context = f.context.clone();
    context.record_side_effect_with_paths(
        1,
        "local_file".into(),
        "edit".into(),
        vec![b.join("same.txt").to_string_lossy().into_owned()],
    );
    std::fs::write(b.join("same.txt"), "USER+AGENT").unwrap();
    let guard = crate::agent::dispatcher::ShellExecutionGuard::capture(&context, &cp, &a);
    assert!(guard
        .check(
            "local_shell",
            "exec",
            &serde_json::json!({"command":"git checkout -- same.txt","cwd":b}),
            &a
        )
        .is_some());
    assert!(cp
        .task_git_baseline
        .as_ref()
        .unwrap()
        .collect_changed_file_paths(&a, chrono::Utc::now())
        .is_empty());
}

#[test]
fn initial_dirty_untracked_deletion_and_unusual_paths_are_compared() {
    let f = Fixture::new();
    std::fs::write(f.extra.join("old.txt"), "user untracked").unwrap();
    let baseline = GitBaseline::capture_at_run_start(&f.extra);
    std::fs::remove_file(f.extra.join("old.txt")).unwrap();
    std::fs::write(f.extra.join(" space\n中文.txt"), "new").unwrap();
    let paths = baseline.collect_changed_file_paths(&f.extra, chrono::Utc::now());
    assert!(paths.contains(&"old.txt".into()));
    assert!(paths.contains(&" space\n中文.txt".into()));
    assert!(baseline
        .collect_git_diff(&f.extra)
        .unwrap()
        .0
        .contains("old.txt"));
}

#[test]
fn empty_repository_detects_staged_changes_without_head() {
    let f = Fixture::new();
    let root = f.root.join("empty");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("existing.txt"), "user staged").unwrap();
    git(&root, &["add", "."]);
    let baseline = GitBaseline::capture_at_run_start(&root);
    assert!(baseline.unchanged_since_baseline(&root, "existing.txt"));
    std::fs::write(root.join("new.txt"), "agent staged").unwrap();
    git(&root, &["add", "."]);
    let paths = baseline.collect_changed_file_paths(&root, chrono::Utc::now());
    assert_eq!(paths, vec!["new.txt"]);
    assert!(baseline
        .collect_git_diff(&root)
        .unwrap()
        .0
        .contains("agent staged"));
}

#[test]
fn five_writable_projects_keep_independent_starts_and_ignore_read_only_reference() {
    let f = Fixture::new();
    let mut dirs = Vec::new();
    for n in 0..6 {
        let root = f.root.join(format!("project-{n}"));
        std::fs::create_dir(&root).unwrap();
        init(&root);
        dirs.push(ProjectDirectory {
            path: root.to_string_lossy().into_owned(),
            access: if n < 5 {
                DirectoryAccess::ReadWrite
            } else {
                DirectoryAccess::ReadOnly
            },
        });
    }
    workspace_access::bind_conversation(&f.sid, &f.primary, dirs.clone());
    let captured = GitBaseline::capture_additional_git_baselines(&f.sid, &f.primary);
    assert_eq!(captured.len(), 5);
    for dir in dirs.iter().take(5) {
        assert!(captured[&dir.path]
            .collect_changed_file_paths(Path::new(&dir.path), chrono::Utc::now())
            .is_empty());
    }
}

#[cfg(unix)]
#[test]
fn retargeted_directory_does_not_capture_another_project() {
    let f = Fixture::new();
    let registered = f.dir(DirectoryAccess::ReadWrite);
    std::fs::rename(&f.extra, f.root.join("old")).unwrap();
    std::os::unix::fs::symlink(&f.primary, &f.extra).unwrap();
    workspace_access::bind_conversation(&f.sid, &f.primary, vec![registered]);
    assert!(GitBaseline::capture_additional_git_baselines(&f.sid, &f.primary).is_empty());
}

#[test]
fn new_logical_task_drops_prior_directory_starts() {
    let mut f = Fixture::new();
    f.grant(DirectoryAccess::ReadWrite);
    save_checkpoint(&f.db, &mut f.cp).unwrap();
    let new_context = SandwichContext::new("sys".into(), "next task".into());
    let mut next = AgentCheckpoint::new(f.sid.clone(), &new_context);
    next.version = f.cp.version;
    save_checkpoint(&f.db, &mut next).unwrap();
    assert!(load_checkpoint(&f.db, &f.sid)
        .unwrap()
        .unwrap()
        .task_additional_git_baselines
        .is_empty());
}

#[cfg(unix)]
#[test]
fn git_material_does_not_follow_untracked_symlink_to_another_directory() {
    let f = Fixture::new();
    let baseline = GitBaseline::capture_at_run_start(&f.extra);
    std::fs::write(f.primary.join("private.txt"), "OUTSIDE_SECRET_CONTENT").unwrap();
    std::os::unix::fs::symlink(f.primary.join("private.txt"), f.extra.join("link.txt")).unwrap();
    let material = baseline.collect_git_diff(&f.extra).unwrap().0;
    assert!(material.contains("link.txt"));
    assert!(!material.contains("OUTSIDE_SECRET_CONTENT"));
}
