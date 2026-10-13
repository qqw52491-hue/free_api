use super::*;

use super::*;

fn temp_dir() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("test-local-file-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn mock_db() -> DbState {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)))
}

#[test]
fn test_local_file_write_and_read() {
    let ws = temp_dir();
    let db = mock_db();

    let write_p = json!({
        "path": "hello.txt",
        "content": "line 1\nline 2\nline 3"
    });
    let res_w = dispatch(&db, "test_session", "write", &write_p, Some(&ws));
    assert!(res_w.success, "write should succeed: {}", res_w.stderr);

    let read_p = json!({
        "path": "hello.txt",
        "start_line": 2,
        "end_line": 2
    });
    let res_r = dispatch(&db, "test_session", "read", &read_p, Some(&ws));
    assert!(res_r.success, "read should succeed: {}", res_r.stderr);
    assert!(res_r.stdout.contains("line 2"));
    assert!(!res_r.stdout.contains("line 1"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_local_file_edit_success_and_ambiguity_protection() {
    let ws = temp_dir();
    let db = mock_db();

    let write_p = json!({
        "path": "code.rs",
        "content": "fn foo() {}\nfn bar() {}\nfn foo() {}"
    });
    dispatch(&db, "test_session", "write", &write_p, Some(&ws));

    // 尝试替换重名项 "fn foo() {}" -> 应该报错且告知行号 [1, 3]
    let ambiguous_p = json!({
        "path": "code.rs",
        "target_content": "fn foo() {}",
        "replacement_content": "fn foo_unique() {}"
    });
    let res_amb = dispatch(&db, "test_session", "edit", &ambiguous_p, Some(&ws));
    assert!(!res_amb.success);
    assert!(res_amb.stderr.contains("目标文本不唯一"));
    assert!(res_amb.stderr.contains("行号: [1, 3]") || res_amb.stderr.contains("[1, 3]"));

    // 唯一替换
    let unique_p = json!({
        "path": "code.rs",
        "target_content": "fn bar() {}",
        "replacement_content": "fn bar_renamed() {}"
    });
    let res_uni = dispatch(&db, "test_session", "edit", &unique_p, Some(&ws));
    assert!(res_uni.success, "edit should succeed: {}", res_uni.stderr);
    assert!(res_uni.stdout.contains("精准替换了第 2 至 2 行"));

    // 验证文件已被修改
    let content = std::fs::read_to_string(ws.join("code.rs")).unwrap();
    assert!(content.contains("fn bar_renamed() {}"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_local_file_edit_matches_crlf_file_with_lf_target() {
    let ws = temp_dir();
    let db = mock_db();
    std::fs::write(ws.join("page.vue"), "<script setup>\r\nimport a from 'a'\r\n\r\nconst x = 1\r\n").unwrap();

    // 模型给的 target 是 LF，文件是 CRLF：应自动适配并保持文件原有 CRLF
    let p = json!({
        "path": "page.vue",
        "target_content": "import a from 'a'\n\nconst x = 1",
        "replacement_content": "import a from 'a'\nimport b from 'b'\n\nconst x = 1"
    });
    let res = dispatch(&db, "test_session", "edit", &p, Some(&ws));
    assert!(res.success, "CRLF 文件应能用 LF target 编辑: {}", res.stderr);
    let content = std::fs::read_to_string(ws.join("page.vue")).unwrap();
    assert_eq!(content, "<script setup>\r\nimport a from 'a'\r\nimport b from 'b'\r\n\r\nconst x = 1\r\n");

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_local_file_mislabeled_write_action_demotes_to_read() {
    let ws = temp_dir();
    let db = mock_db();
    std::fs::write(ws.join("probe.txt"), "alpha\nbeta").unwrap();

    // 实测复现（2026-09-29）：外层 action 槽误填 "edit"、params.action 显式
    // 声明 "read" 且无内容字段 → 按内层真实意图纠错为读，而不是按 edit
    // 报缺参错误或被写入门禁误判拦截。
    let read_p = json!({
        "action": "read",
        "path": "probe.txt",
        "start_line": 2,
        "end_line": 2
    });
    let res = dispatch(&db, "test_session", "edit", &read_p, Some(&ws));
    assert!(res.success, "纠错后应按 read 执行: {}", res.stderr);
    assert!(res.stdout.contains("beta"));

    // 对照：内层声明 read 但带内容字段 = 意图矛盾不猜，保留外层 edit 语义
    // （此处 target_content 唯一命中会真实执行替换——证明没有静默吞掉写意图）。
    let conflict_p = json!({
        "action": "read",
        "path": "probe.txt",
        "target_content": "alpha",
        "replacement_content": "gamma"
    });
    let res2 = dispatch(&db, "test_session", "edit", &conflict_p, Some(&ws));
    assert!(res2.success, "带内容字段应按 edit 语义执行: {}", res2.stderr);
    let content = std::fs::read_to_string(ws.join("probe.txt")).unwrap();
    assert!(content.contains("gamma"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_local_file_invalid_action_rejected() {
    let ws = temp_dir();
    let db = mock_db();

    let p = json!({"path": "a.txt"});
    let res = dispatch(&db, "test_session", "patch", &p, Some(&ws));
    assert!(!res.success);
    assert!(res.stderr.contains("非法动作"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_toolname_in_action_slot_recovers_as_read() {
    let ws = temp_dir();
    let db = mock_db();
    std::fs::write(ws.join("note.txt"), "hello").unwrap();

    // 读形状（行窗、无内容字段）→ 按缺省 read 处置，不再报错
    let p = json!({"action": "local_file", "path": "note.txt", "start_line": 1, "end_line": 5});
    let res = dispatch(&db, "s1", "local_file", &p, Some(&ws));
    assert!(res.success, "toolname-action 读形状应恢复为 read，实得: {}", res.stderr);
    assert!(res.stdout.contains("hello"));

    // 仅 path 无行窗——仍按 read 恢复（read 是 action 缺省值且非破坏）
    let p2 = json!({"action": "local_file", "path": "note.txt"});
    let res2 = dispatch(&db, "s1", "local_file", &p2, Some(&ws));
    assert!(res2.success, "toolname-action 仅 path 也应恢复 read");

    // 带内容字段（写意图不明）→ 不猜，回错并附范例
    let p3 = json!({"action": "local_file", "path": "note.txt", "content": "x"});
    let res3 = dispatch(&db, "s1", "local_file", &p3, Some(&ws));
    assert!(!res3.success);
    assert!(res3.stderr.contains("非法动作"));
    assert!(res3.stderr.contains("action='read'"), "错误应含矫正范例: {}", res3.stderr);

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_optimistic_lock_normal_read_write_succeeds() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_opt_normal";

    // 写入初始文件
    let p_write = json!({"path": "file.txt", "content": "initial content"});
    let res = dispatch(&db, session, "write", &p_write, Some(&ws));
    assert!(res.success);

    // Agent 读取该文件，自动沉淀 baseline
    let p_read = json!({"path": "file.txt"});
    let res_read = dispatch(&db, session, "read", &p_read, Some(&ws));
    assert!(res_read.success);

    // 未发生外部篡改，Agent 再次正常修改 -> 顺利通过
    let p_edit = json!({
        "path": "file.txt",
        "target_content": "initial",
        "replacement_content": "updated"
    });
    let res_edit = dispatch(&db, session, "edit", &p_edit, Some(&ws));
    assert!(res_edit.success, "正常连续修改应通过: {}", res_edit.stderr);

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_optimistic_lock_external_modification_intercepted() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_opt_conflict";

    // 写入初始文件并读取
    let p_write = json!({"path": "file.txt", "content": "initial content"});
    dispatch(&db, session, "write", &p_write, Some(&ws));

    let p_read = json!({"path": "file.txt"});
    dispatch(&db, session, "read", &p_read, Some(&ws));

    // 模拟外部（用户在IDE或外部格式化工具）修改了该文件
    std::fs::write(ws.join("file.txt"), "external user modified content").unwrap();

    // Agent 试图执行全量写入，未带 force -> 应该被并发冲突拦截！
    let p_write2 = json!({"path": "file.txt", "content": "agent overwrite"});
    let res_conflict = dispatch(&db, session, "write", &p_write2, Some(&ws));
    assert!(!res_conflict.success);
    assert!(res_conflict.stderr.contains("并发冲突拦截"));
    assert!(res_conflict.stderr.contains("哈希不匹配"));

    // Agent 试图执行局部编辑，未带 force -> 同样应被拦截！
    let p_edit = json!({
        "path": "file.txt",
        "target_content": "external",
        "replacement_content": "agent edit"
    });
    let res_edit_conflict = dispatch(&db, session, "edit", &p_edit, Some(&ws));
    assert!(!res_edit_conflict.success);
    assert!(res_edit_conflict.stderr.contains("并发冲突拦截"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_optimistic_lock_force_overwrite_bypasses() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_opt_force";

    // 写入初始文件并读取
    let p_write = json!({"path": "file.txt", "content": "initial content"});
    dispatch(&db, session, "write", &p_write, Some(&ws));

    let p_read = json!({"path": "file.txt"});
    dispatch(&db, session, "read", &p_read, Some(&ws));

    // 模拟外部修改
    std::fs::write(ws.join("file.txt"), "external user modified content").unwrap();

    // 传入 force: true -> 成功绕过拦截强行覆盖
    let p_force = json!({"path": "file.txt", "content": "forced content", "force": true});
    let res_force = dispatch(&db, session, "write", &p_force, Some(&ws));
    assert!(res_force.success, "带 force 应绕过冲突拦截: {}", res_force.stderr);

    let content = std::fs::read_to_string(ws.join("file.txt")).unwrap();
    assert_eq!(content, "forced content");

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_unindexed_existing_file_write_intercepted_and_force_bypasses() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_unindexed_write";

    // 用户预先在工作区创建了既有文件（会话此前从未 read 过该文件）
    std::fs::write(ws.join("existing.txt"), "pre-existing critical code").unwrap();

    // Agent 在无基线情况下试图全量写入覆写 -> 必须被硬拦截！
    let p_blind_write = json!({"path": "existing.txt", "content": "blind hallucinated content"});
    let res_blind = dispatch(&db, session, "write", &p_blind_write, Some(&ws));
    assert!(!res_blind.success, "未读过的既有文件盲目全量覆写必须被拦截");
    assert!(res_blind.stderr.contains("全量覆盖保护拦截"));
    assert!(res_blind.stderr.contains("缺少内容基线"));

    // 显式传入 force: true -> 成功覆盖
    let p_force_write = json!({
        "path": "existing.txt",
        "content": "forced intentional content",
        "force": true
    });
    let res_force = dispatch(&db, session, "write", &p_force_write, Some(&ws));
    assert!(res_force.success, "带 force: true 必须允许强制覆盖");
    assert_eq!(
        std::fs::read_to_string(ws.join("existing.txt")).unwrap(),
        "forced intentional content"
    );

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_unindexed_existing_file_edit_succeeds_and_records_baseline() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_unindexed_edit";

    // 用户预先在工作区创建了既有文件
    std::fs::write(ws.join("config.ini"), "port = 8080\nthreads = 4\n").unwrap();

    // 局部替换：即使没有预先 read 基线，凭借 target_content 唯一锚点也允许精准替换
    let p_edit = json!({
        "path": "config.ini",
        "target_content": "threads = 4",
        "replacement_content": "threads = 8"
    });
    let res_edit = dispatch(&db, session, "edit", &p_edit, Some(&ws));
    assert!(res_edit.success, "无基线但局部替换锚点唯一应放行: {}", res_edit.stderr);

    // 替换成功后已自动建立基线，若外部篡改，下一次修改即刻受并发锁保护
    std::fs::write(ws.join("config.ini"), "tampered by outside").unwrap();
    let p_edit_again = json!({
        "path": "config.ini",
        "target_content": "threads = 8",
        "replacement_content": "threads = 16"
    });
    let res_again = dispatch(&db, session, "edit", &p_edit_again, Some(&ws));
    assert!(!res_again.success);
    assert!(res_again.stderr.contains("并发冲突拦截"));

    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn test_self_created_file_lifecycle_and_session_cleanup() {
    let ws = temp_dir();
    let db = mock_db();
    let session = "test_self_created";

    // Agent 在本会话首次写入一个新文件（磁盘原本不存在） -> 顺利通过并自动沉淀基线
    let p_new = json!({"path": "new_tool.rs", "content": "fn run() {}"});
    let res_new = dispatch(&db, session, "write", &p_new, Some(&ws));
    assert!(res_new.success, "自建新文件应直接成功");

    // 随后在同一会话中再次写入此自建文件 -> 判定为自有文件且基线匹配，顺利通过
    let p_overwrite = json!({"path": "new_tool.rs", "content": "fn run() { println!(\"ok\"); }"});
    let res_over = dispatch(&db, session, "write", &p_overwrite, Some(&ws));
    assert!(res_over.success, "同会话自建文件再次覆写应正常放行: {}", res_over.stderr);

    // 执行会话级清理 cleanup_session_baselines
    cleanup_session_baselines(session);

    // 清理后，基线表不再包含该会话记录；此时该文件已存在磁盘，再次尝试不带 force 写入 -> 触发覆盖保护拦截！
    let res_after_cleanup = dispatch(&db, session, "write", &p_overwrite, Some(&ws));
    assert!(!res_after_cleanup.success, "清理基线后既有文件写入应被拦截");
    assert!(res_after_cleanup.stderr.contains("全量覆盖保护拦截"));

    let _ = std::fs::remove_dir_all(ws);
}


#[test]
fn reversed_read_range_returns_parameters_error_without_panic() {
    let ws = temp_dir();
    let db = mock_db();
    std::fs::write(ws.join("lines.txt"), (1..=2000).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
    for action in ["read", "", "local_file"] {
        for params in [
            json!({"path":"lines.txt", "start_line":1960, "end_line":298}),
            json!({"path":"lines.txt", "startLine":1960, "endLine":298}),
        ] {
            let result = dispatch(&db, "reversed-window", action, &params, Some(&ws));
            assert!(!result.success);
            assert_eq!(result.route, super::super::input_validation::INVALID_INPUT_ROUTE);
            assert!(result.stderr.contains("1960") && result.stderr.contains("298"));
            assert!(result.stderr.contains("尚未读取文件"));
        }
    }
    // Reject mechanically invalid intervals before file access, even with a nonexistent path.
    let result = execute_read(&ws, "reversed-window", &json!({"path":"absent", "start_line":9,"end_line":3}));
    assert_eq!(result.route, super::super::input_validation::INVALID_INPUT_ROUTE);
    std::fs::remove_dir_all(ws).unwrap();
}

#[test]
fn valid_read_range_defaults_aliases_and_edges_stay_supported() {
    let ws = temp_dir();
    let db = mock_db();
    std::fs::write(ws.join("lines.txt"), "first\nsecond\nthird\n").unwrap();
    std::fs::write(ws.join("empty.txt"), "").unwrap();
    for params in [
        json!({"path":"lines.txt"}),
        json!({"path":"lines.txt", "start_line":2,"end_line":2}),
        json!({"path":"lines.txt", "startLine":2,"endLine":999}),
        json!({"path":"lines.txt", "start_line":0,"end_line":0}),
        json!({"path":"empty.txt"}),
        json!({"path":"empty.txt", "start_line":1,"end_line":3}),
        json!({"path":"lines.txt", "start_line":9}),
    ] {
        let result = dispatch(&db, "valid-window", "read", &params, Some(&ws));
        assert!(result.success, "{params}: {}", result.stderr);
    }
    let result = dispatch(&db, "valid-window", "read", &json!({"path":"lines.txt","startLine":2,"endLine":2}), Some(&ws));
    assert!(result.stdout.contains("second") && !result.stdout.contains("first") && !result.stdout.contains("third"));
    std::fs::remove_dir_all(ws).unwrap();
}
