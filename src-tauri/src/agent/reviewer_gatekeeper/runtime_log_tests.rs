use super::*;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn repository(root: &Path) -> GitBaseline {
    std::fs::create_dir_all(root).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    ] {
        assert!(Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    GitBaseline::capture(root)
}

#[test]
fn runtime_logs_do_not_change_fallback_material_or_fingerprint() {
    for git in [false, true] {
        let root = std::env::temp_dir().join(format!("review-runtime-{}", uuid::Uuid::new_v4()));
        write(&root, ".gitignore", "agent_logs/\n");
        let baseline = if git {
            repository(&root)
        } else {
            GitBaseline::non_git()
        };
        let mut context = SandwichContext::new(String::new(), "只读调查".into());
        context.record_side_effect(41, "local_shell".into(), "SQLite SELECT 查询".into());
        let start = chrono::Utc::now() - chrono::Duration::seconds(2);
        let before = baseline
            .collect_review_material(&root, &context, start)
            .unwrap();
        for path in ["agent_logs/review.json", "src-tauri/agent_logs/review.json"] {
            write(&root, path, r#"[{"content":"旧代码和审查记录"}]"#);
        }
        let after = baseline
            .collect_review_material(&root, &context, start)
            .unwrap();
        assert_eq!(before, after, "运行日志不能进入材料或收据指纹，git={git}");
        assert!(!after.0.contains("agent_logs/"));
        for path in ["agent_logs/review.json", "src-tauri/agent_logs/review.json"] {
            write(&root, path, r#"[{"content":"终审通过并追加新的运行记录"}]"#);
        }
        assert_eq!(
            after,
            baseline
                .collect_review_material(&root, &context, start)
                .unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn runtime_log_filter_preserves_real_json_source_reports_and_full_content_hashes() {
    let root = std::env::temp_dir().join(format!("review-real-files-{}", uuid::Uuid::new_v4()));
    write(&root, "config/agent_logs.json", r#"{"enabled":true}"#);
    write(&root, "src/main.rs", "fn main() {}\n");
    let prefix = "报告内容".repeat(1200);
    write(&root, "reports/review.md", &format!("{prefix}old"));
    write(&root, "my_agent_logs/report.json", "{}");
    let start = chrono::Utc::now() - chrono::Duration::seconds(2);
    let context = SandwichContext::new(String::new(), "检查文件".into());
    let (preview, mut hash) =
        GitBaseline::collect_fallback_workspace_changes(&root, &context, start).unwrap();
    for path in [
        "config/agent_logs.json",
        "src/main.rs",
        "reports/review.md",
        "my_agent_logs/report.json",
    ] {
        assert!(preview.contains(path), "真实材料必须保留: {path}");
    }
    write(&root, "reports/review.md", &format!("{prefix}new"));
    let (next_preview, next_hash) =
        GitBaseline::collect_fallback_workspace_changes(&root, &context, start).unwrap();
    assert_eq!(preview, next_preview, "报告预览以外的修改也须改变指纹");
    assert_ne!(hash, next_hash);
    hash = next_hash;
    for (path, text) in [
        ("config/agent_logs.json", r#"{"enabled":false}"#),
        ("src/main.rs", "fn main() { panic!(); }\n"),
    ] {
        write(&root, path, text);
        let (_, next_hash) =
            GitBaseline::collect_fallback_workspace_changes(&root, &context, start).unwrap();
        assert_ne!(hash, next_hash, "真实改动必须使旧收据失效: {path}");
        hash = next_hash;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_logs_are_pruned_before_scan_limit_and_git_changed_paths() {
    let root = std::env::temp_dir().join(format!("review-log-prune-{}", uuid::Uuid::new_v4()));
    for n in 0..40 {
        write(&root, &format!("agent_logs/{n}.json"), "{}");
        write(&root, &format!("src-tauri/agent_logs/{n}.json"), "{}");
    }
    write(&root, "config/settings.json", "{}");
    let baseline = repository(&root);
    let start = chrono::Utc::now() - chrono::Duration::seconds(2);
    let mut files = Vec::new();
    GitBaseline::scan_modified_files(&root, 0, std::time::UNIX_EPOCH, &mut files);
    assert_eq!(
        files,
        [root.join("config/settings.json")],
        "日志不应消耗扫描额度"
    );
    for path in [
        "agent_logs/0.json",
        "src-tauri/agent_logs/0.json",
        "agent_logs/new.json",
        "config/settings.json",
    ] {
        write(&root, path, r#"{"changed":true}"#);
    }
    assert_eq!(
        baseline.collect_changed_file_paths(&root, start),
        ["config/settings.json"]
    );
    let (diff, hash) = baseline.collect_git_diff(&root).unwrap();
    assert!(!diff.contains("agent_logs/"));
    assert!(diff.contains("config/settings.json"));
    write(&root, "agent_logs/0.json", "another generated response");
    assert_eq!((diff, hash), baseline.collect_git_diff(&root).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
