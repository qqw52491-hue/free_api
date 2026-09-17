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
