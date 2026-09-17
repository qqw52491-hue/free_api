    use super::*;

    fn workspace() -> (std::path::PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("free-api-writer-{}", uuid::Uuid::new_v4()));
        let root = base.join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("package.json"), "{\"scripts\":{\"test\":\"node --test\"}}").unwrap();
        std::fs::write(root.join("src/sum.mjs"), "export function sum(a, b) { return a - b; }\n").unwrap();
        std::fs::create_dir_all(root.join("test")).unwrap();
        std::fs::write(
            root.join("test/sum.test.mjs"),
            "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { sum } from '../src/sum.mjs';\ntest('sum adds', () => { assert.equal(sum(1, 2), 3); });\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        (base, root)
    }

    #[test]
    fn parse_writer_request_requires_test_command() {
        let ok = parse_writer_request(&json!({"task": "修复 sum", "test_command": "npm test"})).unwrap();
        assert_eq!(ok.test_command, "npm test");
        assert!(ok.context_excerpts.is_none());
        assert!(parse_writer_request(&json!({"task": "修复 sum"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "a; b"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "npm test", "focus_paths": ["1","2","3","4","5","6"]})).is_err());

        // 带有 context_excerpts
        let with_ctx = parse_writer_request(&json!({
            "task": "修复 sum 函数边界",
            "test_command": "npm test",
            "context_excerpts": {
                "background": "sum.mjs 未对负数做容错",
                "todo_slice": ["修复负数逻辑", "通过单元测试"],
                "file_hints": ["src/sum.mjs"]
            }
        })).unwrap();
        assert!(with_ctx.context_excerpts.is_some());
        let ctx = with_ctx.context_excerpts.unwrap();
        assert_eq!(ctx.background.as_deref(), Some("sum.mjs 未对负数做容错"));
        assert_eq!(ctx.todo_slice.len(), 2);
    }

    #[test]
    fn render_writer_initial_prompt_partitions_or_fallbacks() {
        let req_simple = WriterRequest {
            task: "修复 bug".into(),
            test_command: "npm test".into(),
            focus_paths: vec!["src/main.rs".into()],
            context_excerpts: None,
            hard_scope: String::new(),
        };
        let p1 = render_writer_initial_prompt(Path::new("/tmp/copy"), "src/main.rs", &req_simple);
        assert!(p1.starts_with("副本工作区根目录：/tmp/copy\n关注路径：src/main.rs"));

        let req_rich = WriterRequest {
            task: "修复 bug".into(),
            test_command: "npm test".into(),
            focus_paths: vec!["src/main.rs".into()],
            context_excerpts: Some(crate::agent::types::DispatchContext {
                background: Some("这是详细背景".into()),
                todo_slice: vec!["步骤1".into()],
                file_hints: vec!["src/main.rs".into()],
                prior_findings: vec!["之前已发现 panic".into()],
            }),
            hard_scope: String::new(),
        };
        let p2 = render_writer_initial_prompt(Path::new("/tmp/copy"), "src/main.rs", &req_rich);
        assert!(p2.contains("### 背景信息\n这是详细背景"));
        assert!(p2.contains("### 相关计划与待办\n- 步骤1"));
        assert!(p2.contains("### 既有结论与线索\n- 之前已发现 panic"));
        assert!(p2.contains("### 相关文件提示\n- src/main.rs"));
        assert!(p2.contains("### 修改任务\n修复 bug"));
    }

    #[test]
    fn isolated_copy_skips_git_and_enforces_budget() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy1");
        let (files, bytes) = create_isolated_copy(&root, &copy).unwrap();
        assert!(files >= 2 && bytes > 0);
        assert!(!copy.join(".git").exists(), ".git 必须被排除");
        assert!(copy.join("src/sum.mjs").exists());
        assert!(copy.join("package.json").exists());
    }

    #[tokio::test]
    async fn write_file_records_before_snapshot_and_refuses_escape() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy2");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        // 越界写入被拒绝
        assert!(execute_writer_action(&copy, "write_file", &json!({
            "path": "../evil.txt", "content": "boom"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());

        // hard_scope 物理围栏测试：只允许修改 src/，试图写 package.json 被拦截
        assert!(execute_writer_action(&copy, "write_file", &json!({
            "path": "package.json", "content": "{}"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.is_err());

        // 正常写入：留底 + 落盘
        let out = execute_writer_action(&copy, "write_file", &json!({
            "path": "src/sum.mjs", "content": "export function sum(a, b) { return a + b; }\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.unwrap();
        assert!(out.contains("已写入 src/sum.mjs"));
        assert_eq!(writes, 1);
        let before = changes.get("src/sum.mjs").unwrap();
        assert!(before.content.contains("return a - b;"), "首次写入前必须留底原内容");
        assert!(before.existed_before, "既有文件的 existed_before 必须为 true");
        // 新建文件：留底为空且 existed_before=false
        execute_writer_action(&copy, "write_file", &json!({
            "path": "src/new-helper.mjs", "content": "export const ok = true;\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.unwrap();
        assert!(changes.contains_key("src/new-helper.mjs"));
        assert_eq!(
            changes.get("src/new-helper.mjs").map(|b| b.existed_before),
            Some(false),
            "新建文件的 existed_before 必须为 false"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn run_command_tracks_test_evidence_and_blocks_non_allowlisted() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy3");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "rm -rf ."}), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());
        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "npm test > out.log"}), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());

        execute_writer_action(&copy, "run_command", &json!({"command": "node --test"}), &mut changes, &mut writes, "node --test", &mut test_evidence, "").await.unwrap();
        let (command, passed, _) = test_evidence.expect("测试命令必须被记录为证据");
        assert_eq!(command, "node --test");
        // 测试会失败（sum 还没修），passed 应为 false
        assert!(!passed);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn line_diff_outputs_minus_and_plus_lines() {
        let diff = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert!(diff.contains("- b"));
        assert!(diff.contains("+ B"));
        assert!(diff.contains("+ d"));
        assert!(!diff.contains("- a"));
    }

    #[test]
    fn test_evaluate_file_conflict_matrix() {
        // 1. 主工作区干净无第三方改动：可直接重放
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v1"),
            ConflictStatus::CleanReplay
        );

        // 2. 主工作区已被修改且内容与 writer 产物完全一致：无需操作
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v2"),
            ConflictStatus::AlreadyApplied
        );

        // 3. 主工作区被修改为第三方内容（三者互不相等）：真冲突，禁止盲目覆盖
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v3"),
            ConflictStatus::Conflict
        );
    }

    #[test]
    fn test_dehydrate_child_messages_bounds() {
        let short_msg = json!({"role": "user", "content": "短文本"});
        let huge_text = "A".repeat(10_000);
        let long_msg = json!({"role": "user", "content": huge_text});

        let msgs = vec![short_msg, long_msg];
        let dehydrated = dehydrate_child_messages(&msgs);

        assert_eq!(dehydrated[0]["content"], "短文本");
        let long_content = dehydrated[1]["content"].as_str().unwrap();
        assert!(long_content.len() < 5000);
        assert!(long_content.contains("数据较长已脱水，省略 6000 字符"));
    }

    #[tokio::test]
    async fn test_init_git_baseline_and_diff_extraction() {
        let (base, root) = workspace();
        let copy = base.join(".writer-git-test");
        create_isolated_copy(&root, &copy).unwrap();

        let ok = init_git_baseline(&copy).await;
        if ok {
            // 修改文件
            std::fs::write(copy.join("src/sum.mjs"), "export function sum(a, b) { return a + b + 0; }\n").unwrap();
            let _ = tokio::process::Command::new("git").arg("add").arg("-A").current_dir(&copy).output().await;
            let out = tokio::process::Command::new("git")
                .args(&["diff", "--cached", "--unified=3"])
                .current_dir(&copy)
                .output()
                .await
                .unwrap();
            let diff_str = String::from_utf8_lossy(&out.stdout);
            let extracted = extract_file_diff(&diff_str, "src/sum.mjs");
            assert!(extracted.is_some());
            let fd = extracted.unwrap();
            assert!(fd.contains("diff --git a/src/sum.mjs b/src/sum.mjs"));
            assert!(fd.contains("+export function sum"));
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_cleanup_child_workspaces_removes_matching_dirs_only() {
        let (base, root) = workspace();
        let copy1 = base.join(".writer-test-cleanup-1");
        let copy2 = base.join(".writer-test-cleanup-2");
        let safe_keep = base.join("safe_keep_dir");
        std::fs::create_dir_all(&copy1).unwrap();
        std::fs::create_dir_all(&copy2).unwrap();
        std::fs::create_dir_all(&safe_keep).unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let conn = state.0.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'succeeded', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-run-clean-1",
                "parent-run-clean",
                "session-clean",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy1.to_string_lossy(),
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'succeeded', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-run-clean-2",
                "parent-run-clean",
                "session-clean",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy2.to_string_lossy(),
            ],
        ).unwrap();
        drop(conn);

        assert!(copy1.exists());
        assert!(copy2.exists());
        assert!(safe_keep.exists());

        cleanup_child_workspaces(&state, "parent-run-clean");

        assert!(!copy1.exists(), "已登记的 copy1 副本必须被回收");
        assert!(!copy2.exists(), "已登记的 copy2 副本必须被回收");
        assert!(safe_keep.exists(), "非 writer 目录绝不能被误删");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_cleanup_orphaned_writer_workspaces_on_startup() {
        let (base, root) = workspace();
        let copy_db = base.join(".writer-test-orphan-db");
        let copy_disk_only = root.join(".writer-test-orphan-disk");
        let safe_keep = root.join("src");
        std::fs::create_dir_all(&copy_db).unwrap();
        std::fs::create_dir_all(&copy_disk_only).unwrap();
        std::fs::create_dir_all(&safe_keep).unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let conn = state.0.lock().unwrap();
        // 注册 agent_projects
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path, created_at, updated_at) VALUES ('p1', 'proj', ?1, ?2, ?2)",
            rusqlite::params![root.to_string_lossy(), chrono::Utc::now().to_rfc3339()],
        ).unwrap();
        // 模拟上次崩溃遗留的 running 任务
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-orphan-1",
                "parent-orphan",
                "session-orphan",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy_db.to_string_lossy(),
            ],
        ).unwrap();
        drop(conn);

        assert!(copy_db.exists());
        assert!(copy_disk_only.exists());
        assert!(safe_keep.exists());

        let cleaned = cleanup_orphaned_writer_workspaces(&state);
        assert!(cleaned >= 2, "至少清理 2 处孤儿子目录，实际清理: {}", cleaned);
        assert!(!copy_db.exists(), "DB 中的僵尸目录必须被清理");
        assert!(!copy_disk_only.exists(), "磁盘扫描发现的孤儿目录必须被清理");
        assert!(safe_keep.exists(), "安全业务目录绝不可被误删");

        // 验证 status 被置为 interrupted
        let conn = state.0.lock().unwrap();
        let status: String = conn.query_row(
            "SELECT status FROM agent_child_runs WHERE id = 'child-orphan-1'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(status, "interrupted", "running 状态必须被标记为 interrupted");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_refresh_file_conflicts_catches_runtime_changes() {
        let (base, root) = workspace();
        let file_a = root.join("a.txt");
        std::fs::write(&file_a, "original content\n").unwrap();

        let spawn_hash = compute_file_hash(&file_a, false).await;
        let changed_file = ChangedFile {
            path: "a.txt".to_string(),
            bytes_before: 10,
            bytes_after: 20,
            diff: String::new(),
            spawn_hash: Some(spawn_hash.clone()),
            result_hash: Some("result_hash_abc".to_string()),
            root_hash: Some(spawn_hash.clone()),
            conflict_status: Some(ConflictStatus::CleanReplay),
            existed_before: Some(true),
        };

        let mut files = vec![changed_file];
        // 1. 父工程内容未变时，现算为 CleanReplay
        refresh_file_conflicts(&mut files, &root, false).await;
        assert_eq!(files[0].conflict_status, Some(ConflictStatus::CleanReplay));

        // 2. 期间父工程被其他步骤修改
        std::fs::write(&file_a, "modified by root or user\n").unwrap();
        refresh_file_conflicts(&mut files, &root, false).await;
        assert_eq!(files[0].conflict_status, Some(ConflictStatus::Conflict), "现算必须检测到中间发生的冲突");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_execute_writer_action_edit_file() {
        let (base, root) = workspace();
        let file = root.join("src/sum.mjs");

        let mut changes = BTreeMap::new();
        let mut writes_used = 0;
        let mut test_evidence = None;

        // 唯一匹配替换成功
        let p_ok = json!({
            "path": "src/sum.mjs",
            "target_content": "return a - b;",
            "replacement_content": "return a + b;"
        });
        let res = execute_writer_action(
            &root,
            "edit_file",
            &p_ok,
            &mut changes,
            &mut writes_used,
            "npm test",
            &mut test_evidence,
            "",
        )
        .await;

        assert!(res.is_ok(), "edit_file should succeed: {:?}", res);
        assert_eq!(writes_used, 1);
        assert!(changes.contains_key("src/sum.mjs"));

        let updated = std::fs::read_to_string(&file).unwrap();
        assert!(updated.contains("return a + b;"));

        // 未找到报错
        let p_not_found = json!({
            "path": "src/sum.mjs",
            "target_content": "non_existent_code();",
            "replacement_content": "foo();"
        });
        let res_err = execute_writer_action(
            &root,
            "edit_file",
            &p_not_found,
            &mut changes,
            &mut writes_used,
            "npm test",
            &mut test_evidence,
            "",
        )
        .await;
        assert!(res_err.is_err());
        assert!(res_err.unwrap_err().contains("目标文本未找到"));

        let _ = std::fs::remove_dir_all(base);
    }

    /// 构造一个已终态 writer 行（result_json + workspace_copy），供 apply 测试使用
    fn seed_writer_run(
        state: &DbState,
        child_run_id: &str,
        session_id: &str,
        root: &Path,
        copy: &Path,
        result_json: &str,
    ) {
        let conn = state.0.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy, result_json) VALUES (?1, ?2, ?3, ?4, ?5, 'succeeded', ?6, 'writer', ?7, ?8)",
            rusqlite::params![
                child_run_id,
                "parent-apply",
                session_id,
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy.to_string_lossy(),
                result_json,
            ],
        )
        .unwrap();
    }

    fn make_writer_result(child_run_id: &str, copy: &Path, changed_files: Vec<ChangedFile>) -> String {
        let result = WriterResult {
            child_run_id: child_run_id.to_string(),
            status: "succeeded".into(),
            findings: vec![],
            confidence: "confirmed".into(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 1,
            changed_files,
            test_command: "npm test".into(),
            test_status: "passed".into(),
            test_output_tail: String::new(),
            workspace_copy: copy.to_string_lossy().to_string(),
            patch_ref: None,
        };
        serde_json::to_string(&result).unwrap()
    }

    #[tokio::test]
    async fn test_apply_writer_result_copy_path_and_conflict_refusal() {
        let (base, root) = workspace();
        let copy = base.join(".writer-apply-ok");
        create_isolated_copy(&root, &copy).unwrap();

        // writer 在副本内修复 sum 并新建 helper
        std::fs::write(
            copy.join("src/sum.mjs"),
            "export function sum(a, b) { return a + b; }\n",
        )
        .unwrap();
        std::fs::write(copy.join("src/new-helper.mjs"), "export const ok = true;\n").unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 指纹口径与 apply 内部一致：以 git 二进制可用性为准
        let ga = is_git_available(&root).await;
        let before_sum = std::fs::read_to_string(root.join("src/sum.mjs")).unwrap();
        let spawn_sum = compute_str_hash(&before_sum, ga, &copy).await;
        let result_sum = compute_file_hash(&copy.join("src/sum.mjs"), ga).await;
        let result_new = compute_file_hash(&copy.join("src/new-helper.mjs"), ga).await;

        let result_json = make_writer_result(
            "child-apply-ok",
            &copy,
            vec![
                ChangedFile {
                    path: "src/sum.mjs".into(),
                    bytes_before: before_sum.len(),
                    bytes_after: 52,
                    diff: String::new(),
                    conflict_status: None,
                    spawn_hash: Some(spawn_sum),
                    result_hash: Some(result_sum),
                    root_hash: None,
                    existed_before: Some(true),
                },
                ChangedFile {
                    path: "src/new-helper.mjs".into(),
                    bytes_before: 0,
                    bytes_after: 21,
                    diff: String::new(),
                    conflict_status: None,
                    spawn_hash: Some("empty/deleted".into()),
                    result_hash: Some(result_new),
                    root_hash: None,
                    existed_before: Some(false),
                },
            ],
        );
        seed_writer_run(&state, "child-apply-ok", "session-apply", &root, &copy, &result_json);

        // 1. 干净合入：覆盖修改 + 新建文件
        let report = apply_writer_result(&state, "child-apply-ok", "session-apply", &root)
            .await
            .unwrap();
        assert!(report.contains("机械合入完成"), "{}", report);
        assert_eq!(
            std::fs::read_to_string(root.join("src/sum.mjs")).unwrap(),
            "export function sum(a, b) { return a + b; }\n"
        );
        assert!(root.join("src/new-helper.mjs").exists(), "新建文件必须被创建");

        // 2. 幂等：再次 apply 全部 AlreadyApplied
        let report2 = apply_writer_result(&state, "child-apply-ok", "session-apply", &root)
            .await
            .unwrap();
        assert!(report2.contains("无需重复合入"), "{}", report2);

        // 3. 冲突拒绝：另一 writer 基于旧基线，而父文件已被第三方改动 → 整体拒绝且不落盘
        let copy2 = base.join(".writer-apply-conflict");
        create_isolated_copy(&root, &copy2).unwrap();
        std::fs::write(
            copy2.join("src/sum.mjs"),
            "export function sum(a, b) { return a + b + 0; }\n",
        )
        .unwrap();
        // 父工作区当前内容是 writer1 的结果；writer2 的 spawn 基线是原始 buggy 版本
        let stale_spawn = compute_str_hash(
            "export function sum(a, b) { return a - b; }\n",
            ga,
            &copy2,
        )
        .await;
        let result2 = compute_file_hash(&copy2.join("src/sum.mjs"), ga).await;
        let result_json2 = make_writer_result(
            "child-apply-conflict",
            &copy2,
            vec![ChangedFile {
                path: "src/sum.mjs".into(),
                bytes_before: 51,
                bytes_after: 56,
                diff: String::new(),
                conflict_status: None,
                spawn_hash: Some(stale_spawn),
                result_hash: Some(result2),
                root_hash: None,
                existed_before: Some(true),
            }],
        );
        seed_writer_run(&state, "child-apply-conflict", "session-apply", &root, &copy2, &result_json2);

        let err = apply_writer_result(&state, "child-apply-conflict", "session-apply", &root)
            .await
            .unwrap_err();
        assert!(err.contains("整体拒绝"), "{}", err);
        // 冲突时绝不落盘：父文件仍是 writer1 的内容
        assert_eq!(
            std::fs::read_to_string(root.join("src/sum.mjs")).unwrap(),
            "export function sum(a, b) { return a + b; }\n"
        );

        // 4. 会话归属校验：别的会话的 writer 不能合入
        let err2 = apply_writer_result(&state, "child-apply-ok", "other-session", &root)
            .await
            .unwrap_err();
        assert!(err2.contains("不属于当前会话"), "{}", err2);

        let _ = std::fs::remove_dir_all(base);
    }
