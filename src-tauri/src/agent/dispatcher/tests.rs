    use super::*;
    use crate::db::{init_db, DbState};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn mcp_service_name_requires_an_explicit_subtool_without_parameter_inference() {
        assert!(mcp_service_requires_concrete_tool("remote-http-mcp", ""));
        assert!(mcp_service_requires_concrete_tool(
            "remote-http-mcp",
            "remote-http-mcp"
        ));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp/read_text_file",
            ""
        ));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp",
            "read_text_file"
        ));
    }

    #[test]
    fn code_read_guard_bounds_unscoped_and_large_requests() {
        let (unscoped, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs"}),
        );
        assert_eq!(unscoped["startLine"], 1);
        assert_eq!(unscoped["endLine"], MAX_CODE_READ_LINES);
        assert!(notice.unwrap().contains("正常预算边界"));

        let (large, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 500, "endLine": 1000}),
        );
        assert_eq!(large["startLine"], 500);
        assert_eq!(large["endLine"], 739);
        assert!(notice.is_some());

        let (bounded, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 40, "endLine": 100}),
        );
        assert_eq!(bounded["endLine"], 100);
        assert!(notice.is_none());
    }

    #[test]
    fn test_model_manager_crud_and_safety_confirm() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(Arc::new(Mutex::new(conn)));

        // 1. 测试 list_specs
        let p_list = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_specs", &p_list);
        assert!(res.success, "list_specs should succeed: {}", res.stderr);
        assert!(
            res.stdout.contains("DeepSeek-V3") || res.stdout.contains("DeepSeek"),
            "should contain default specs"
        );

        // 2. 测试 upsert_spec
        let p_upsert = serde_json::json!({
            "name": "Custom-Test-Model",
            "context_window": 65536,
            "max_tokens": 8192,
            "supports_thinking": true,
            "default_temperature": 0.6,
            "description": "A unit test model spec"
        });
        let res = crate::agent::tools::model_manager::dispatch(&db, "upsert_spec", &p_upsert);
        assert!(res.success, "upsert_spec should succeed: {}", res.stderr);

        // 3. 测试 delete_spec 无 confirm 拦截
        let p_del_no_confirm = serde_json::json!({ "name": "Custom-Test-Model" });
        let res =
            crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_no_confirm);
        assert!(
            !res.success,
            "delete_spec without confirm must fail for safety"
        );
        assert!(res.stderr.contains("confirm: true"));

        // 4. 测试 delete_spec 带 confirm: true
        let p_del_confirm = serde_json::json!({ "name": "Custom-Test-Model", "confirm": true });
        let res = crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_confirm);
        assert!(
            res.success,
            "delete_spec with confirm must succeed: {}",
            res.stderr
        );

        // 5. 测试 list_platforms
        let p_platforms = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_platforms", &p_platforms);
        assert!(res.success, "list_platforms should succeed: {}", res.stderr);
    }

    #[test]
    fn test_prompts_tools_files_exist() {
        let candidate_dirs = [
            std::path::PathBuf::from("src-tauri/prompts/tools"),
            std::path::PathBuf::from("prompts/tools"),
            std::path::PathBuf::from("../src-tauri/prompts/tools"),
        ];

        let mut found_dir = None;
        for d in &candidate_dirs {
            if d.exists() && d.is_dir() {
                found_dir = Some(d.clone());
                break;
            }
        }

        let dir = found_dir.expect("prompts/tools directory must exist in candidate paths");
        let model_manager_path = dir.join("model_manager.md");
        assert!(
            model_manager_path.exists(),
            "model_manager.md must exist in {:?}",
            dir
        );
        let content = std::fs::read_to_string(&model_manager_path).expect("read model_manager.md");
        assert!(
            content.contains("<summary>"),
            "model_manager.md must contain <summary>"
        );
        assert!(
            content.contains("upsert_spec"),
            "model_manager.md must describe upsert_spec"
        );

        let mcp_manager_path = dir.join("mcp_manager.md");
        assert!(
            mcp_manager_path.exists(),
            "mcp_manager.md must exist in {:?}",
            dir
        );
        let mcp_content = std::fs::read_to_string(&mcp_manager_path).expect("read mcp_manager.md");
        assert!(
            mcp_content.contains("add_http"),
            "mcp_manager.md must describe add_http"
        );

        let skill_manager_path = dir.join("skill_manager.md");
        assert!(
            skill_manager_path.exists(),
            "skill_manager.md must exist in {:?}",
            dir
        );
        let skill_content =
            std::fs::read_to_string(&skill_manager_path).expect("read skill_manager.md");
        assert!(
            skill_content.contains("add"),
            "skill_manager.md must describe add"
        );
    }

    #[test]
    fn test_mcp_and_skill_manager_crud() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 1. MCP Manager List & Toggle & Delete
        let res_list =
            crate::agent::tools::mcp_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_list.success);

        // Delete without confirm
        let res_del_fail = crate::agent::tools::mcp_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "fake_mcp" }),
        );
        assert!(!res_del_fail.success);
        assert!(res_del_fail.stderr.contains("confirm: true"));

        // Delete with confirm，但目标不存在：必须诚实失败而非伪报成功
        let res_del_ok = crate::agent::tools::mcp_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "fake_mcp", "confirm": true }),
        );
        assert!(!res_del_ok.success);
        assert!(res_del_ok.stderr.contains("未找到"));

        // 2. Skill Manager Add & List & Delete
        let res_add_skill = crate::agent::tools::skill_manager::dispatch(
            &db,
            "add",
            &serde_json::json!({
                "name": "单元测试技能",
                "content": "编写高覆盖率的 Rust 与 Vue 单元测试",
                "kind": "skill",
                "description": "用于编写单元测试"
            }),
        );
        assert!(res_add_skill.success);
        assert!(res_add_skill.stdout.contains("单元测试技能"));

        let res_skill_list =
            crate::agent::tools::skill_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_skill_list.success);
        assert!(res_skill_list.stdout.contains("单元测试技能"));

        let res_skill_del = crate::agent::tools::skill_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "单元测试技能", "confirm": true }),
        );
        assert!(res_skill_del.success);
    }

    #[test]
    fn test_host_security_rejection_forbidden_workspace_switch() {
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let registry = PluginRegistry::new();
        let p = serde_json::json!({ "path": "/some/other/workspace" });

        // 1. 通用会话未关联项目，不触发项目锁定硬拦截（放行给外部 MCP 路由）
        let res_general = execute_command_inner(&db, "general-session", "switch_workspace", "", &p, &registry, None);
        assert_ne!(res_general.route, "host_rejection:project_session_workspace_locked");

        // 2. 模拟项目会话绑定
        {
            let conn = db.0.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS agent_projects (id TEXT PRIMARY KEY, name TEXT, root_path TEXT, created_at TEXT, updated_at TEXT);
                 CREATE TABLE IF NOT EXISTS agent_project_sessions (project_id TEXT, session_id TEXT, created_at TEXT);
                 INSERT INTO agent_projects VALUES ('p1', 'proj', '/tmp', 'now', 'now');
                 INSERT INTO agent_project_sessions VALUES ('p1', 'proj-session', 'now');"
            ).unwrap();
        }

        let res = execute_command_inner(&db, "proj-session", "switch_workspace", "", &p, &registry, None);
        assert!(!res.success);
        assert!(res.stderr.contains("宿主安全保护"));
        assert_eq!(res.route, "host_rejection:project_session_workspace_locked");
    }

    #[test]
    fn test_code_read_guard_supports_offset_and_snippet() {
        // 1. 预算内的 offset + limit (50..149 共 100 行 <= 240)
        let p_within = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 100
        });
        let (guarded_within, notice_within) = guard_code_read_range("get_code_snippet", p_within);
        assert_eq!(
            guarded_within.get("startLine").and_then(|v| v.as_u64()),
            Some(50)
        );
        assert_eq!(
            guarded_within.get("endLine").and_then(|v| v.as_u64()),
            Some(149)
        );
        assert!(
            notice_within.is_none(),
            "within-budget range should not need truncation notice"
        );

        // 2. 超出预算的 offset + limit (50..5000)
        let p_over = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 5000
        });
        let (guarded_over, notice_over) = guard_code_read_range("get_code_snippet", p_over);
        assert_eq!(
            guarded_over.get("startLine").and_then(|v| v.as_u64()),
            Some(50)
        );
        assert_eq!(
            guarded_over.get("endLine").and_then(|v| v.as_u64()),
            Some(289)
        ); // 50 + 240 - 1
        assert!(
            notice_over.is_some(),
            "over-budget range must include truncation guard notice"
        );
    }

    #[test]
    fn input_readonly_flags_do_not_override_tool_classification() {
        for flag in [
            json!({"read_only":true}),
            json!({"readonly":true}),
            json!({"metadata":{"read_only":true}}),
            json!({"meta":{"readonly":true}}),
        ] {
            for tool in ["write_file", "preview_saved_logs", "remote/exec_sql"] {
                let inst: AgentInstruction = serde_json::from_value(json!({
                    "tool":tool,"params":flag,"read_only":true,"metadata":{"readonly":true}
                })).unwrap();
                assert!(is_write_or_side_effect_tool(&inst), "{tool}: {flag}");
            }
        }
        let read: AgentInstruction = serde_json::from_value(json!({
            "tool":"remote/read_file","params":{"path":"a.rs","read_only":true}
        })).unwrap();
        assert!(!is_write_or_side_effect_tool(&read));
    }

    #[test]
    fn test_local_file_mislabeled_outer_action_demotes_to_read() {
        use crate::agent::types::AgentInstruction;
        use serde_json::json;

        // 实测复现（2026-09-29）：模型发出外层 action="edit"、内层
        // command.action="read" 的纯读请求，被外层标签误判为写并撞
        // src-tauri/** 高危门禁。内层显式 read + 无内容字段 = 按只读归类。
        let mut inst_read = AgentInstruction::default();
        inst_read.tool = Some("local_file".to_string());
        inst_read.action = Some("edit".to_string());
        inst_read.command = Some(json!({
            "action": "read",
            "path": "src-tauri/src/agent/plan_submit.rs",
            "start_line": 151,
            "end_line": 300
        }));
        assert!(
            !is_write_or_side_effect_tool(&inst_read),
            "外层 edit + 内层 read + 无内容字段的 local_file 必须按只读归类"
        );

        // 外层 write + 内层 list 同样降级（列目录不可写）
        let mut inst_list = AgentInstruction::default();
        inst_list.tool = Some("local_file".to_string());
        inst_list.action = Some("write".to_string());
        inst_list.command = Some(json!({ "action": "list", "path": "src" }));
        assert!(!is_write_or_side_effect_tool(&inst_list));

        // 对照 1：内层 read 但带内容字段 = 写意图与只读声明矛盾，不猜，仍按写走门禁
        let mut inst_conflict = AgentInstruction::default();
        inst_conflict.tool = Some("local_file".to_string());
        inst_conflict.action = Some("edit".to_string());
        inst_conflict.command = Some(json!({
            "action": "read",
            "path": "src/foo.rs",
            "target_content": "old",
            "replacement_content": "new"
        }));
        assert!(
            is_write_or_side_effect_tool(&inst_conflict),
            "带内容字段的矛盾声明必须按写处理（fail-closed）"
        );

        // 对照 2：外层写动词且无内层 action 声明 —— 无法证实只读意图，仍按写处理
        let mut inst_write = AgentInstruction::default();
        inst_write.tool = Some("local_file".to_string());
        inst_write.action = Some("edit".to_string());
        inst_write.command = Some(json!({
            "path": "src/foo.rs",
            "target_content": "a",
            "replacement_content": "b"
        }));
        assert!(is_write_or_side_effect_tool(&inst_write));

        // 对照 3：常规 read 不受影响
        let mut inst_normal = AgentInstruction::default();
        inst_normal.tool = Some("local_file".to_string());
        inst_normal.command = Some(json!({ "action": "read", "path": "a.rs" }));
        assert!(!is_write_or_side_effect_tool(&inst_normal));
    }

    #[test]
    fn test_is_safe_read_only_shell_command() {
        use crate::agent::types::AgentInstruction;
        use serde_json::json;

        // 1. 纯只读 curl 查网络/天气 -> 识别为安全只读放行
        assert!(is_safe_read_only_shell_command("curl https://wttr.in/Shanghai"));
        assert!(is_safe_read_only_shell_command("curl -s https://api.weather.com/data"));
        assert!(is_safe_read_only_shell_command("wget -qO- https://example.com"));

        // 2. 纯只读 git status / diff / log / branch -> 放行
        assert!(is_safe_read_only_shell_command("git status"));
        assert!(is_safe_read_only_shell_command("git diff --stat"));
        assert!(is_safe_read_only_shell_command("git log -n 5"));
        assert!(is_safe_read_only_shell_command("git branch -a"));

        // 3. 常见探针与复合命令 (&&, ;, |) -> 放行
        assert!(is_safe_read_only_shell_command("ls -la && pwd"));
        assert!(is_safe_read_only_shell_command("cat src/main.rs | grep fn"));
        assert!(is_safe_read_only_shell_command("echo hello; date"));
        assert!(is_safe_read_only_shell_command("node -v"));
        assert!(is_safe_read_only_shell_command("cargo --version"));

        // 4. 危险操作一票否决：文件输出重定向 -> 拦截 (判为写操作)
        assert!(!is_safe_read_only_shell_command("curl https://foo.com > output.txt"));
        assert!(!is_safe_read_only_shell_command("echo hello >> README.md"));

        // 5. 危险管道与提权注入 -> 拦截
        assert!(!is_safe_read_only_shell_command("curl https://foo.com | bash"));
        assert!(!is_safe_read_only_shell_command("sudo ls"));
        assert!(!is_safe_read_only_shell_command("rm -rf src/"));
        assert!(!is_safe_read_only_shell_command("sed -i 's/a/b/g' file.txt"));

        // 6. 文本替换与删除拦截 (用户明确指定只拦截这类修改行为)
        assert!(!is_safe_read_only_shell_command("sed -i 's/foo/bar/g' src/main.rs"));
        assert!(!is_safe_read_only_shell_command("perl -pi -e 's/foo/bar/g' file.txt"));
        assert!(!is_safe_read_only_shell_command("rm -rf target"));
        assert!(!is_safe_read_only_shell_command("rmdir tmp"));
        assert!(!is_safe_read_only_shell_command("truncate -s 0 test.txt"));

        // 7. 常规开发与测试构建命令 -> 放行（不作为代码修改工具拦截）
        assert!(is_safe_read_only_shell_command("cargo build"));
        assert!(is_safe_read_only_shell_command("npm run build"));
        assert!(is_safe_read_only_shell_command("npm test"));
        assert!(is_safe_read_only_shell_command("git status"));
        assert!(is_safe_read_only_shell_command("git diff"));

        // 8. 复合命令中若含有删除/替换/重定向写盘 -> 全链条一票否决
        assert!(!is_safe_read_only_shell_command("git status && rm -rf tmp"));
        assert!(!is_safe_read_only_shell_command("ls -l; sed -i 's/a/b/g' config.json"));

        // 9. 集成到 is_write_or_side_effect_tool 验证
        let mut inst_curl = AgentInstruction::default();
        inst_curl.tool = Some("remote-hub/shell_exec".to_string());
        inst_curl.command = Some(json!({ "command": "curl https://wttr.in/Shanghai" }));
        assert!(!is_write_or_side_effect_tool(&inst_curl), "只读 curl 应该放行为只读操作");

        let mut inst_test_shell = AgentInstruction::default();
        inst_test_shell.tool = Some("remote-hub/shell_exec".to_string());
        inst_test_shell.command = Some(json!({ "command": "cargo test" }));
        assert!(!is_write_or_side_effect_tool(&inst_test_shell), "测试与构建命令不属于修改代码写盘，应放行");

        let mut inst_sed_shell = AgentInstruction::default();
        inst_sed_shell.tool = Some("remote-hub/shell_exec".to_string());
        inst_sed_shell.command = Some(json!({ "command": "sed -i 's/old/new/g' src/lib.rs" }));
        assert!(is_write_or_side_effect_tool(&inst_sed_shell), "sed -i 就地修改文件属于破坏性写盘，必须受门禁保护");

        // 10. 用户实测场景: bash -c 复合天气/网络探针多命令
        let mut inst_bash_c = AgentInstruction::default();
        inst_bash_c.tool = Some("remote-hub/shell_exec".to_string());
        inst_bash_c.command = Some(json!({
            "command": "bash",
            "args": [
                "-c",
                "date; curl -s -m 5 'http://ip-api.com/json'; echo ''; curl -s -m 5 'wttr.in/?format=3'"
            ],
            "cwd": "/home/w/文档/free_api"
        }));
        assert!(!is_write_or_side_effect_tool(&inst_bash_c), "bash -c 复合只读命令必须放行为只读操作");

        // 11. bash -c 若内部含有写操作 -> 必须拦截
        let mut inst_bash_c_write = AgentInstruction::default();
        inst_bash_c_write.tool = Some("remote-hub/shell_exec".to_string());
        inst_bash_c_write.command = Some(json!({
            "command": "bash",
            "args": [
                "-c",
                "date; rm -rf target"
            ]
        }));
        assert!(is_write_or_side_effect_tool(&inst_bash_c_write), "bash -c 包含 rm 必须判定为写操作受门禁保护");

        // 12. 带有 command 与 args 拆分传递的形式 (如 curl -s http://...)
        let mut inst_split_args = AgentInstruction::default();
        inst_split_args.tool = Some("run_command".to_string());
        inst_split_args.command = Some(json!({
            "command": "curl",
            "args": ["-s", "-m", "5", "http://ip-api.com/json"]
        }));
        assert!(!is_write_or_side_effect_tool(&inst_split_args), "curl + args 拆分传递只读必须放行");

        // 13. 用户实测场景: 2>/dev/null 丢弃输出、管道与引号内部字符，必须安全放行
        let mut inst_sh_c = AgentInstruction::default();
        inst_sh_c.tool = Some("remote-hub/shell_exec".to_string());
        inst_sh_c.command = Some(json!({
            "command": "sh",
            "args": [
                "-c",
                "curl -s --noproxy '*' -m 4 http://whois.pconline.com.cn/ipJson.jsp | iconv -f gbk -t utf-8 2>/dev/null || curl -s -m 4 https://cip.cc"
            ],
            "cwd": "/home/w/文档/free_api/src-tauri",
            "shell_mode": true
        }));
        assert!(!is_write_or_side_effect_tool(&inst_sh_c), "带 2>/dev/null 的网络探针与编码转换必须作为只读放行");
    }

    #[test]
    fn test_git_worktree_destructive() {
        use crate::agent::dispatcher::shell_safety::is_safe_read_only_shell_command;

        // 2026-09-25 实测事故回归：终审指令"撤销无关修改保持提交纯净"被执行成
        // `git checkout --`，销毁了 eval_runner.mjs 85 行未提交工作且全程未进
        // 副作用台账（判为只读）。此类命令必须归类为破坏性写操作。

        // 销毁未提交工作/改写历史 → 拦截
        assert!(!is_safe_read_only_shell_command("git checkout -- scripts/eval_runner.mjs"));
        assert!(!is_safe_read_only_shell_command("git checkout -- ."));
        assert!(!is_safe_read_only_shell_command("git checkout ."));
        assert!(!is_safe_read_only_shell_command("git restore src/lib.rs"));
        assert!(!is_safe_read_only_shell_command("git restore --staged --worktree ."));
        assert!(!is_safe_read_only_shell_command("git clean -fd"));
        assert!(!is_safe_read_only_shell_command("git reset --hard HEAD~1"));
        assert!(!is_safe_read_only_shell_command("git reset --keep"));
        assert!(!is_safe_read_only_shell_command("git push --force origin main"));
        assert!(!is_safe_read_only_shell_command("git push -f"));
        assert!(!is_safe_read_only_shell_command("git push --force-with-lease"));
        assert!(!is_safe_read_only_shell_command("git branch -D feature-x"));
        assert!(!is_safe_read_only_shell_command("git stash drop"));
        assert!(!is_safe_read_only_shell_command("git stash clear"));
        assert!(!is_safe_read_only_shell_command("git stash pop"));
        assert!(!is_safe_read_only_shell_command("git rm -r --cached ."));
        assert!(!is_safe_read_only_shell_command("git mv a.rs b.rs"));

        // 复合命令藏 git 破坏段 → 全链条一票否决
        assert!(!is_safe_read_only_shell_command("git status && git checkout -- foo.rs"));

        // git 只读与非破坏写 → 放行（不误伤正常流程）
        assert!(is_safe_read_only_shell_command("git status"));
        assert!(is_safe_read_only_shell_command("git diff --stat"));
        assert!(is_safe_read_only_shell_command("git log --oneline -5"));
        assert!(is_safe_read_only_shell_command("git checkout -b feat/x"));
        assert!(is_safe_read_only_shell_command("git checkout main"));
        assert!(is_safe_read_only_shell_command("git add src-tauri/src/commands/tasks.rs"));
        assert!(is_safe_read_only_shell_command("git commit -m msg"));
        assert!(is_safe_read_only_shell_command("git stash"));
        assert!(is_safe_read_only_shell_command("git stash list"));
        assert!(is_safe_read_only_shell_command("git branch -d done"));

        // 非 git 语境不串味：restore/clean 等词出现在其他命令里不误拦
        assert!(is_safe_read_only_shell_command("cargo clean"));
        assert!(is_safe_read_only_shell_command("npm run clean"));
    }

    #[test]
    fn test_git_destructive_review_triage() {
        use crate::agent::dispatcher::{
            git_cmd_fingerprint, git_destructive_review, GitDestructiveVerdict,
        };

        // 不带 -- 的 checkout <文件路径> 同样是还原路径（实测事故 2026-10-10）；分支名/切分支不拦
        assert_eq!(
            git_destructive_review(
                "git checkout src/main/java/LicenseUtils.java src/main/resources/a.txt"
            ),
            GitDestructiveVerdict::Targets {
                paths: vec![
                    "src/main/java/LicenseUtils.java".to_string(),
                    "src/main/resources/a.txt".to_string()
                ],
                dirty_gated: true,
            }
        );
        assert_eq!(
            git_destructive_review("git checkout LicenseUtils.java"),
            GitDestructiveVerdict::Targets {
                paths: vec!["LicenseUtils.java".to_string()],
                dirty_gated: true,
            }
        );
        for ok in [
            "git checkout main",
            "git checkout feature/x",
            "git checkout origin/main",
            "git checkout release/1.0",
            "git checkout v1.2",
            "git checkout -b feat/x.y",
        ] {
            assert_eq!(git_destructive_review(ok), GitDestructiveVerdict::NotDestructive, "{ok}");
        }

        // 显式路径 → Targets（可逐条对账；checkout/restore 类脏门槛）
        assert_eq!(
            git_destructive_review("git checkout -- scripts/eval_runner.mjs"),
            GitDestructiveVerdict::Targets {
                paths: vec!["scripts/eval_runner.mjs".to_string()],
                dirty_gated: true,
            }
        );
        assert_eq!(
            git_destructive_review("git checkout -- a.rs b.rs"),
            GitDestructiveVerdict::Targets {
                paths: vec!["a.rs".to_string(), "b.rs".to_string()],
                dirty_gated: true,
            }
        );
        assert_eq!(
            git_destructive_review("git restore --worktree src/lib.rs"),
            GitDestructiveVerdict::Targets {
                paths: vec!["src/lib.rs".to_string()],
                dirty_gated: true,
            }
        );
        assert_eq!(
            git_destructive_review("git mv old.rs new.rs"),
            GitDestructiveVerdict::Targets {
                paths: vec!["old.rs".to_string(), "new.rs".to_string()],
                dirty_gated: false,
            }
        );

        // 无路径/整树/历史/远端形态 → Pathless（影响面不可界定，强制人工确认）
        for cmd in [
            "git checkout .",
            "git checkout -- .",
            "git checkout --",
            "git restore .",
            "git clean -fd",
            "git reset --hard HEAD~1",
            "git push -f",
            "git push --force-with-lease",
            "git branch -D feat",
            "git stash drop",
            "git stash clear",
        ] {
            assert_eq!(
                git_destructive_review(cmd),
                GitDestructiveVerdict::Pathless,
                "{cmd} 应判定 Pathless"
            );
        }

        // 变更但不销毁（pop/apply 冲突时 git 自护）→ SideEffect：入账不请人
        assert_eq!(
            git_destructive_review("git stash pop"),
            GitDestructiveVerdict::SideEffect
        );
        assert_eq!(
            git_destructive_review("git stash apply stash@{1}"),
            GitDestructiveVerdict::SideEffect
        );

        // 歧义/非破坏/试运行 → NotDestructive
        for cmd in [
            "git checkout main",
            "git checkout -b feat",
            "git checkout --orphan x",
            "git status",
            "git stash",
            "git stash list",
            "git branch -d done",
            "git commit -m msg",
            "git clean -n",          // dry-run 不落盘
            "git push -n origin main", // push -n 试运行
            "echo git checkout -- x", // 非 git 段首
            "cargo clean",
        ] {
            assert_eq!(
                git_destructive_review(cmd),
                GitDestructiveVerdict::NotDestructive,
                "{cmd} 应放行"
            );
        }

        // 指纹归一化：空白压缩后原样，ask_human 引用靠它机械核对
        assert_eq!(
            git_cmd_fingerprint("git   checkout   --   a.rs"),
            "git checkout -- a.rs"
        );
    }

    #[test]
    fn mcp_model_output_single_text_exact() {
        let text = "第一行 含 空白 \n\n第二行\t制表符\n末尾换行\n";
        let result = json!({
            "content": [{"type": "text", "text": text}],
            "isError": false,
        });
        assert_eq!(format_mcp_model_output(&result), text);

        let legacy_no_type = json!({
            "content": [{"text": text}],
        });
        assert_eq!(format_mcp_model_output(&legacy_no_type), text);
    }

    #[test]
    fn mcp_model_output_error_text_keeps_legacy_plain_output() {
        let result = json!({
            "content": [{"type": "text", "text": "工具内部错误: quota"}],
            "isError": true,
        });
        assert_eq!(format_mcp_model_output(&result), "工具内部错误: quota");
    }

    #[test]
    fn mcp_model_output_multiple_text_blocks_in_order() {
        let result = json!({
            "content": [
                {"type": "text", "text": "第一段"},
                {"type": "text", "text": "第二段"},
            ],
            "isError": false,
        });
        assert_eq!(format_mcp_model_output(&result), "第一段\n\n第二段");
    }

    #[test]
    fn mcp_model_output_structured_omitted_when_text_exists() {
        let result = json!({
            "content": [{"type": "text", "text": "概要正文"}],
            "structuredContent": {"rows": [1, 2, 3]},
            "_meta": {"secret": "should-not-appear"},
        });
        let output = format_mcp_model_output(&result);
        assert_eq!(output, "概要正文");
        assert!(!output.contains("rows"));
        assert!(!output.contains("should-not-appear"));
    }

    #[test]
    fn mcp_model_output_structured_shown_when_no_text() {
        let structured = json!({"rows": [1, 2], "note": "仅结构化"});
        let result = json!({
            "content": [{"type": "image", "data": "aGk=", "mimeType": "image/png"}],
            "structuredContent": structured,
        });
        let output = format_mcp_model_output(&result);
        assert!(output.contains("image"));
        let parsed: serde_json::Value =
            serde_json::from_str(&output[output.find('{').unwrap()..]).unwrap();
        assert_eq!(parsed, structured);
    }

    #[test]
    fn mcp_model_output_image_audio_described_without_base64() {
        let sentinel = "aGVsbG8tZnJlZS1hcGktc2VudGluZWw=";
        let result = json!({
            "content": [
                {"type": "image", "data": sentinel, "mimeType": "image/png"},
                {"type": "audio", "data": sentinel, "mimeType": "audio/wav",
                 "annotations": {"audience": ["user"]}},
            ],
        });
        let output = format_mcp_model_output(&result);
        assert!(!output.contains(sentinel));
        assert!(output.contains("image"));
        assert!(output.contains("audio"));
        assert!(output.contains("image/png"));
        assert!(output.contains("audio/wav"));
        assert!(output.contains(&sentinel.chars().count().to_string()));
        assert!(output.contains("annotations"));
    }

    #[test]
    fn mcp_model_output_embedded_resource_text_included_blob_excluded() {
        let blob = "YmluYXJ5LWJsb2Itc2VudGluZWw=";
        let result = json!({
            "content": [
                {"type": "resource", "resource": {"uri": "file:///tmp/a.txt", "mimeType": "text/plain", "text": "内嵌资源正文"}},
                {"type": "resource", "resource": {"uri": "file:///tmp/x.bin", "mimeType": "application/octet-stream", "blob": blob}},
            ],
        });
        let output = format_mcp_model_output(&result);
        assert!(output.contains("内嵌资源正文"));
        assert!(output.contains("file:///tmp/a.txt"));
        assert!(output.contains("file:///tmp/x.bin"));
        assert!(output.contains(&blob.chars().count().to_string()));
        assert!(!output.contains(blob));
    }

    #[test]
    fn mcp_model_output_resource_link_described() {
        let result = json!({
            "content": [{
                "type": "resource_link",
                "name": "report.csv",
                "title": "季度报表",
                "uri": "file:///tmp/report.csv",
                "mimeType": "text/csv",
                "description": "含逗号分隔数据",
            }],
        });
        let output = format_mcp_model_output(&result);
        for needle in ["report.csv", "季度报表", "file:///tmp/report.csv", "text/csv", "含逗号分隔数据"] {
            assert!(output.contains(needle), "缺描述字段: {}", needle);
        }
    }

    #[test]
    fn mcp_model_output_unknown_block_bounded_no_raw_leak() {
        let result = json!({
            "content": [
                {"type": "vendor-x", "secret_payload": "TOP-SECRET-BASE64-BLOB"},
                "not-an-object",
            ],
        });
        let output = format_mcp_model_output(&result);
        assert!(output.contains("vendor-x"));
        assert!(output.contains("#0"));
        assert!(output.contains("#1"));
        assert!(output.contains("output_id"));
        assert!(!output.contains("TOP-SECRET-BASE64-BLOB"));
    }

    #[test]
    fn mcp_model_output_empty_fallback_nonempty() {
        for result in [
            json!({}),
            json!({"content": []}),
            json!({"content": "not-an-array"}),
            serde_json::Value::Null,
        ] {
            let output = format_mcp_model_output(&result);
            assert!(!output.is_empty());
            assert!(output.contains("output_id"));
        }
    }

    #[test]
    fn mcp_raw_evidence_direct_sidecar_round_trip() {
        let arguments = json!({"q": "shanghai", "units": "metric"});
        let result = json!({
            "content": [
                {"type": "text", "text": "概要"},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
            ],
            "structuredContent": {"rows": [1, 2]},
            "_meta": {"trace": "abc"},
            "isError": false,
            "vendorExtension": {"x": 1},
        });
        let view = mcp_raw_result_view("weather", "get", &arguments, &result);
        assert_eq!(view["kind"], "mcp_raw_results");
        assert_eq!(view["items"].as_array().unwrap().len(), 1);
        let item = &view["items"][0];
        assert_eq!(item["plugin"], "weather");
        assert_eq!(item["tool"], "get");
        assert_eq!(item["arguments"], arguments);
        assert_eq!(item["result"], result);

        let mut view_opt = Some(view);
        let durable = take_mcp_durable_output(&mut view_opt).expect("必须消费精确 sidecar");
        assert!(view_opt.is_none());
        let parsed: serde_json::Value = serde_json::from_str(&durable).unwrap();
        assert_eq!(parsed["items"][0]["result"]["_meta"], json!({"trace": "abc"}));
        assert_eq!(parsed["items"][0]["result"]["structuredContent"], json!({"rows": [1, 2]}));
        assert_eq!(parsed["items"][0]["result"]["content"][1]["data"], "aGVsbG8=");
    }

    #[test]
    fn mcp_raw_evidence_aggregation_preserves_call_order() {
        let mut items = Vec::new();
        let first = mcp_raw_result_view("p1", "t1", &json!({"a": 1}), &json!({"content": []}));
        let second = mcp_raw_result_view("p2", "t2", &json!({"b": 2}), &json!({"content": []}));
        append_mcp_raw_results(&mut items, Some(&first));
        append_mcp_raw_results(&mut items, Some(&second));
        let view = mcp_raw_results_view(items).expect("两项必须聚合");
        let arr = view["items"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["plugin"], "p1");
        assert_eq!(arr[1]["plugin"], "p2");
        assert!(mcp_raw_results_view(Vec::new()).is_none());
    }

    #[test]
    fn mcp_raw_evidence_ignores_malformed_or_foreign_view() {
        let mut items = Vec::new();
        append_mcp_raw_results(&mut items, None);
        append_mcp_raw_results(&mut items, Some(&json!({"kind": "other", "items": []})));
        append_mcp_raw_results(&mut items, Some(&json!({"kind": "mcp_raw_results", "items": "x"})));
        append_mcp_raw_results(&mut items, Some(&json!({"no-kind": true})));
        assert!(items.is_empty());
    }

    #[test]
    fn mcp_raw_evidence_take_consumes_exact_sidecar_leaves_foreign() {
        let foreign = json!({"kind": "ui_card", "data": 1});
        let mut view = Some(foreign.clone());
        assert!(take_mcp_durable_output(&mut view).is_none());
        assert_eq!(view.as_ref().unwrap(), &foreign);

        let mut none_view: Option<serde_json::Value> = None;
        assert!(take_mcp_durable_output(&mut none_view).is_none());

        let malformed = json!({"kind": "mcp_raw_results", "items": 7});
        let mut view = Some(malformed.clone());
        assert!(take_mcp_durable_output(&mut view).is_none());
        assert_eq!(view.as_ref().unwrap(), &malformed);
    }

    #[test]
    fn mcp_durable_projection_renders_all_items_without_raw_leak() {
        let secret_b64 = "U0VDUkVULUJMT0ItU0VOVElORUw=";
        let sidecar = serde_json::to_string_pretty(&json!({
            "kind": "mcp_raw_results",
            "items": [
                {
                    "plugin": "p1", "tool": "t1", "arguments": {"a": 1},
                    "result": {"content": [{"type": "text", "text": "第一块正文"}], "_meta": {"k": "META-SECRET"}}
                },
                {
                    "plugin": "p2", "tool": "t2", "arguments": {"b": 2},
                    "result": {"content": [{"type": "image", "data": secret_b64, "mimeType": "image/png"}]}
                }
            ]
        }))
        .unwrap();
        let projected = project_mcp_durable_output(&sidecar).expect("sidecar 必须可投影");
        assert!(projected.contains("2 次调用"));
        assert!(projected.contains("p1/t1"));
        assert!(projected.contains("p2/t2"));
        assert!(projected.contains("第一块正文"));
        assert!(projected.contains("image/png"));
        assert!(!projected.contains(secret_b64));
        assert!(!projected.contains("META-SECRET"));
        assert!(projected.contains("raw=true") || projected.contains("raw: true"));

        assert!(project_mcp_durable_output("普通文本输出").is_none());
        assert!(project_mcp_durable_output("{\"kind\":\"other\"}").is_none());
        assert!(project_mcp_durable_output("{broken json").is_none());
    }

    #[test]
    fn project_resources_list_supports_offset_limit_pagination() {
        // 提示词承诺"按需分页取回"，工具必须真实支持 offset/limit。
        let base = std::env::temp_dir().join(format!("free-api-res-page-{}", uuid::Uuid::new_v4()));
        let root = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..5 {
            std::fs::write(root.join(format!("r{i}.md")), "x").unwrap();
        }
        let conn = init_db(&base.join("test.db")).expect("init db");
        let project = crate::commands::projects::create_project_row(
            &conn,
            "分页项目",
            root.to_string_lossy().as_ref(),
        )
        .unwrap();
        let session = crate::commands::projects::create_project_session_row(
            &conn,
            &project.id,
            "分页会话",
            "model-a",
        )
        .unwrap();
        for i in 0..5 {
            crate::commands::projects::add_project_resource_row(
                &conn,
                &project.id,
                root.join(format!("r{i}.md")).to_string_lossy().as_ref(),
            )
            .unwrap();
        }
        let db = DbState(Arc::new(Mutex::new(conn)));

        // 无参数：全量返回 + total/has_more 元数据
        let all = crate::agent::dispatcher::project_dispatch::dispatch_project_resources(
            &db,
            &session.id,
            "list_resources",
            &json!({}),
        );
        assert!(all.success);
        let payload: serde_json::Value = serde_json::from_str(&all.stdout).unwrap();
        assert_eq!(payload["total"], 5);
        assert_eq!(payload["has_more"], false);
        assert_eq!(payload["resources"].as_array().unwrap().len(), 5);

        // offset+limit 分页生效
        let page = crate::agent::dispatcher::project_dispatch::dispatch_project_resources(
            &db,
            &session.id,
            "list_resources",
            &json!({"offset": 2, "limit": 2}),
        );
        let payload: serde_json::Value = serde_json::from_str(&page.stdout).unwrap();
        assert_eq!(payload["resources"].as_array().unwrap().len(), 2);
        assert_eq!(payload["total"], 5);
        assert_eq!(payload["offset"], 2);
        assert_eq!(payload["has_more"], true);

        // 分页必须有界：缺省按默认页（50），超限 limit 收敛到硬上限（200），
        // 否则一次 list_resources() 全量倾倒就绕过了尾部清单的展示预算。
        let default_page = crate::agent::dispatcher::project_dispatch::dispatch_project_resources(
            &db,
            &session.id,
            "list_resources",
            &json!({}),
        );
        let payload: serde_json::Value = serde_json::from_str(&default_page.stdout).unwrap();
        assert_eq!(payload["limit"], 50, "缺省 limit 应为默认页大小");

        let capped = crate::agent::dispatcher::project_dispatch::dispatch_project_resources(
            &db,
            &session.id,
            "list_resources",
            &json!({"limit": 99999}),
        );
        let payload: serde_json::Value = serde_json::from_str(&capped.stdout).unwrap();
        assert_eq!(payload["limit"], 200, "超限 limit 必须收敛到硬上限");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mcp_outbound_tmp_and_relative_paths_become_absolute() {
        let tmp = std::path::Path::new("/data/tmp/sess-1");
        let ws = std::path::Path::new("/repo/proj");
        let mut params = json!({
            "file_path": "tmp/report.pptx",
            "output_dir": "./tmp/out",
            "out_path": "session-tmp://chart.png",
            "target_dir": "workspace://docs/spec.md",
            "source": "src/main.rs",
            "save_to": "plain_name.txt",
            "presentation_id": "presentation_1",
            "dest_url": "https://x.dev/tmp/a",
            "absolute": "/etc/passwd",
            "nested": {"in": [{"target_file": "tmp/deep.csv"}]}
        });
        let n = normalize_mcp_path_params(&mut params, tmp, ws);
        assert_eq!(n, 7, "tmp/×2 + URI×2 + 相对×2 + 嵌套×1");
        assert_eq!(params["file_path"], "/data/tmp/sess-1/report.pptx");
        assert_eq!(params["output_dir"], "/data/tmp/sess-1/out");
        assert_eq!(params["out_path"], "/data/tmp/sess-1/chart.png");
        assert_eq!(params["target_dir"], "/repo/proj/docs/spec.md");
        assert_eq!(params["source"], "/repo/proj/src/main.rs");
        assert_eq!(params["save_to"], "/repo/proj/plain_name.txt");
        assert_eq!(params["nested"]["in"][0]["target_file"], "/data/tmp/sess-1/deep.csv");
        // 不该碰的：纯标识符、URL、绝对路径
        assert_eq!(params["presentation_id"], "presentation_1");
        assert_eq!(params["dest_url"], "https://x.dev/tmp/a");
        assert_eq!(params["absolute"], "/etc/passwd");
    }

    #[test]
    fn mcp_outbound_path_normalization_rejects_traversal_and_odd_values() {
        let tmp = std::path::Path::new("/data/tmp/sess-1");
        let ws = std::path::Path::new("/repo/proj");
        let mut params = json!({
            "file_path": "tmp/../escape.txt",
            "other_path": "../up.txt",
            "win_abs": "C:\\Windows\\x.dll",
            "empty_path": "  ",
            "title": "tmp/not-a-path-key"
        });
        let n = normalize_mcp_path_params(&mut params, tmp, ws);
        assert_eq!(n, 0, "逃逸/绝对/空值/非path键一律不碰");
        assert_eq!(params["file_path"], "tmp/../escape.txt");
        assert_eq!(params["title"], "tmp/not-a-path-key", "非 path 键的字符串值不受路由约定约束");
    }

    #[test]
    fn mcp_tool_case_preservation_and_cached_tools_resolution() {
        let registered_tools = vec!["App", "Click", "Notification"];
        let resolve_name = |input_name: &str| -> String {
            registered_tools
                .iter()
                .find(|&&name| name.eq_ignore_ascii_case(input_name))
                .map(|&name| name.to_string())
                .unwrap_or_else(|| input_name.to_string())
        };

        assert_eq!(resolve_name("App"), "App");
        assert_eq!(resolve_name("app"), "App");
        assert_eq!(resolve_name("APP"), "App");
        assert_eq!(resolve_name("notification"), "Notification");
        assert_eq!(resolve_name("Notification"), "Notification");
        assert_eq!(resolve_name("other_tool"), "other_tool");
    }

#[test]
fn native_input_errors_precede_permissions_workspace_and_database() {
    let db = DbState(Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())));
    let registry = PluginRegistry::new();
    for (tool, action, params, field) in [
        ("local_shell", "exec", json!({"path":".","depth":1}), "command"),
        ("local_file", "write", json!({"path":"a.txt"}), "content"),
        ("local_file", "edit", json!({"path":"a.txt","target_content":"a"}), "replacement_content"),
        ("mcp_manager", "add_http", json!({}), "url"),
        ("mcp_manager", "toggle", json!({"name":"x","enabled":"false"}), "enabled"),
        ("artifact_manager", "create_document", json!({}), "content"),
    ] {
        let r = execute_command_inner(&db, "input-test", tool, action, &params, &registry, None);
        assert!(!r.success);
        assert_eq!(r.route, crate::agent::tools::input_validation::INVALID_INPUT_ROUTE);
        assert!(r.stderr.contains(field), "{}", r.stderr);
        assert!(!r.stderr.contains("权限安全拦截"));
    }
}

#[test]
fn preflight_does_not_reclassify_valid_write_or_destructive_shell() {
    let db = DbState(Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())));
    for v in [json!({"tool":"local_file","action":"write","params":{"path":"a.txt","content":"ok"}}),
        json!({"tool":"local_shell","command":{"command":"git reset --hard"}})] {
        let inst: AgentInstruction = serde_json::from_value(v).unwrap();
        assert!(crate::agent::tools::input_validation::instruction_rejection(&db, &inst).is_none());
        assert!(is_write_or_side_effect_tool(&inst), "合法写入仍须经过原门禁");
    }
}


#[test]
fn empty_parallel_placeholder_executes_the_original_file_read() {
    let root = std::env::temp_dir().join(format!("free-api-empty-parallel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("probe.txt"), "empty-parallel-read-ok").unwrap();
    let db = DbState(Arc::new(Mutex::new(init_db(&PathBuf::from(":memory:")).unwrap())));
    let registry = PluginRegistry::new();
    let instruction: AgentInstruction = serde_json::from_value(json!({
        "tool":"local_file", "command":{"action":"read","path":"probe.txt"},
        "parallel":[]
    })).unwrap();
    let result = run_agent_step(&db, "empty-parallel-test", &instruction, &registry, Some(&root));
    std::fs::remove_dir_all(&root).unwrap();
    assert!(result.success, "{} / {}", result.route, result.stderr);
    assert_ne!(result.route, "parallel_contract_rejected");
    assert!(result.stdout.contains("empty-parallel-read-ok"), "{}", result.stdout);
}


#[test]
fn reversed_read_range_is_rejected_before_workspace_or_business_checks() {
    let db = DbState(Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())));
    let instruction: AgentInstruction = serde_json::from_value(json!({
        "tool":"local_file", "command":{"path":"missing.txt","start_line":1960,"end_line":298}
    })).unwrap();
    let result = run_agent_step(&db, "range-preflight", &instruction, &PluginRegistry::new(), None);
    assert_eq!(result.route, crate::agent::tools::input_validation::INVALID_INPUT_ROUTE);
    assert!(result.stderr.contains("end_line (298) 小于 start_line (1960)"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_reversed_read_range_does_not_crash_other_branch() {
    let root = std::env::temp_dir().join(format!("free-api-range-parallel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("probe.txt"), "valid-branch-still-executed\n").unwrap();
    let db = DbState(Arc::new(Mutex::new(init_db(&PathBuf::from(":memory:")).unwrap())));
    let instruction: AgentInstruction = serde_json::from_value(json!({"parallel":[
        {"tool":"local_file","command":{"path":"probe.txt","start_line":1960,"end_line":298}},
        {"tool":"local_file","command":{"path":"probe.txt"}}
    ]})).unwrap();
    let result = run_agent_step(&db, "range-parallel", &instruction, &PluginRegistry::new(), Some(&root));
    assert!(!result.success);
    assert!(result.stderr.contains("end_line (298) 小于 start_line (1960)"));
    // Failure reports intentionally dehydrate successful payloads; verify the branch status instead.
    assert!(result.stdout.contains("成功: 1, 失败: 1, 超时: 0"), "{}", result.stdout);
    assert!(result.stdout.contains("\"ok\": true"));
    assert!(!result.stderr.contains("panicked") && !result.stderr.contains("工具执行任务崩溃"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_params_aliases_execute_identically_in_all_four_modes() {
    let (root, db, registry) = shell_params_fixture();
    std::fs::write(root.join("probe.txt"), "container-parity-ok\n").unwrap();
    for container in ["params", "args", "command"] {
        let mut leaf = json!({"tool":"local_file"});
        leaf[container] = json!({"action":"read","path":"probe.txt"});
        for raw in [leaf.clone(), json!({"commands":[leaf.clone()]}),
            json!({"parallel":[leaf.clone()]}), json!({"plan_tree":[leaf.clone()]})] {
            let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
            let result = run_agent_step(&db, "tool-params-parity", &inst, &registry, Some(&root));
            assert!(result.success, "{container}: {}", result.stderr);
            assert!(result.stdout.contains("container-parity-ok"), "{container}: {}", result.stdout);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_params_null_outer_containers_agree_in_all_four_modes() {
    let (root, db, registry) = shell_params_fixture();
    std::fs::write(root.join("probe.txt"), "null-container-ok\n").unwrap();
    for tool in ["local_file", "local_shell"] {
        for container in ["params", "args", "command"] {
            let mut leaf = json!({"tool":tool,"args":null,"params":null,"command":null});
            leaf[container] = if tool == "local_file" {
                json!({"action":"read","path":"probe.txt"})
            } else {
                json!({"command":"printf null-container-ok"})
            };
            for raw in [leaf.clone(), json!({"commands":[leaf.clone()]}),
                json!({"parallel":[leaf.clone()]}), json!({"plan_tree":[leaf.clone()]})] {
                let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
                let result = tokio::task::block_in_place(|| {
                    run_agent_step(&db, "null-container-parity", &inst, &registry, Some(&root))
                });
                assert!(result.success, "{tool}/{container}: {}", result.stderr);
                assert!(result.stdout.contains("null-container-ok"), "{}", result.stdout);
            }
        }
    }
    // 字符串命令不能把已忽略的外层 args:null 作为进程参数重新带回。
    let inst: AgentInstruction = serde_json::from_value(json!({
        "tool":"local_shell","command":"printf null-container-ok","args":null,"params":null
    })).unwrap();
    assert_eq!(inst.get_params(), json!({"command":"printf null-container-ok"}));
    let leaf = serde_json::to_value(&inst).unwrap();
    for raw in [leaf.clone(), json!({"commands":[leaf.clone()]}),
        json!({"parallel":[leaf.clone()]}), json!({"plan_tree":[leaf.clone()]})] {
        let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
        let result = tokio::task::block_in_place(|| {
            run_agent_step(&db, "null-shell-shorthand", &inst, &registry, Some(&root))
        });
        assert!(result.success && result.stdout.contains("null-container-ok"), "{}", result.stderr);
    }
    // 没有容器时沿用既有平铺参数兼容；null 占位不能遮住 path。
    let leaf = json!({"tool":"local_file","action":"read","path":"probe.txt",
        "args":null,"params":null,"command":null});
    for raw in [leaf.clone(), json!({"commands":[leaf]})] {
        let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
        let result = run_agent_step(&db, "null-flat-compatibility", &inst, &registry, Some(&root));
        assert!(result.success && result.stdout.contains("null-container-ok"), "{}", result.stderr);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_params_inner_null_is_rejected_and_empty_replacement_deletes_in_all_four_modes() {
    let (root, db, registry) = shell_params_fixture();
    for mode in 0..4 {
        let session = format!("inner-null-{mode}");
        std::fs::write(root.join("probe.txt"), "remove-me\n").unwrap();
        let read: AgentInstruction = serde_json::from_value(json!({
            "tool":"local_file","params":{"action":"read","path":"probe.txt"}
        })).unwrap();
        assert!(run_agent_step(&db, &session, &read, &registry, Some(&root)).success);
        for replacement in [serde_json::Value::Null, json!("")] {
            let leaf = json!({"tool":"local_file","args":null,"params":{
                "action":"edit","path":"probe.txt","target_content":"remove-me",
                "replacement_content":replacement
            }});
            let raw = match mode {
                0 => leaf,
                1 => json!({"commands":[leaf]}),
                2 => json!({"parallel":[leaf]}),
                _ => json!({"plan_tree":[leaf]}),
            };
            let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
            let result = run_agent_step(&db, &session, &inst, &registry, Some(&root));
            if replacement.is_null() {
                assert!(!result.success, "inner null must not become deletion");
                assert!(result.stderr.contains("replacement_content"), "{}", result.stderr);
                assert_eq!(std::fs::read_to_string(root.join("probe.txt")).unwrap(), "remove-me\n");
            } else {
                assert!(result.success, "mode {mode}: {}", result.stderr);
                assert_eq!(std::fs::read_to_string(root.join("probe.txt")).unwrap(), "\n");
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_params_shell_aliases_and_process_args_agree_in_all_four_modes() {
    let (root, db, registry) = shell_params_fixture();
    let mut leaves = Vec::new();
    for container in ["params", "args", "command"] {
        let mut leaf = json!({"tool":"local_shell"});
        leaf[container] = json!({"command":"printf shell-container-ok"});
        leaves.push(leaf);
    }
    leaves.push(json!({"tool":"local_shell","command":"sh","args":["-c","printf shell-container-ok"]}));
    for leaf in leaves {
        for raw in [leaf.clone(), json!({"commands":[leaf.clone()]}),
            json!({"parallel":[leaf.clone()]}), json!({"plan_tree":[leaf.clone()]})] {
            let inst: AgentInstruction = serde_json::from_value(raw).unwrap();
            let result = tokio::task::block_in_place(|| {
                run_agent_step(&db, "tool-params-shell", &inst, &registry, Some(&root))
            });
            assert!(result.success, "{}", result.stderr);
            assert!(result.stdout.contains("shell-container-ok"), "{}", result.stdout);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tool_params_video_shape_does_not_guess_edit_and_pipeline_keeps_fail_fast() {
    let (root, db, registry) = shell_params_fixture();
    std::fs::write(root.join("probe.txt"), "original\n").unwrap();
    // 真实视频失败形状：有替换字段却漏 action。解包后由既有预检报动作错误，不猜 edit。
    let inst: AgentInstruction = serde_json::from_value(json!({"commands":[
        {"tool":"local_file","params":{"path":"probe.txt","target_content":"original","replacement_content":"changed"}}
    ]})).unwrap();
    let result = run_agent_step(&db, "tool-params-default", &inst, &registry, Some(&root));
    assert!(!result.success, "missing edit action must not execute");
    assert!(result.stderr.contains("action") && result.stderr.contains("文件未修改"), "{}", result.stderr);
    assert!(!result.stderr.contains("缺少必填参数 'path'"));
    assert_eq!(std::fs::read_to_string(root.join("probe.txt")).unwrap(), "original\n");
    let inst: AgentInstruction = serde_json::from_value(json!({"commands":[
        {"tool":"local_file","params":{"action":"read","path":"probe.txt"}},
        {"tool":"local_file","args":{"action":"read"}},
        {"tool":"local_shell","params":{"command":"touch must-not-run"}}
    ]})).unwrap();
    let result = run_agent_step(&db, "tool-params-fail-fast", &inst, &registry, Some(&root));
    assert!(!result.success && result.route == "pipeline_fail_at_2", "{}", result.stderr);
    assert!(result.stdout.contains("original") && result.stderr.contains("path"));
    assert!(!root.join("must-not-run").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tool_params_explicit_pipeline_edit_keeps_baseline_and_path_boundary() {
    let (root, db, registry) = shell_params_fixture();
    std::fs::write(root.join("probe.txt"), "original\n").unwrap();
    let inst: AgentInstruction = serde_json::from_value(json!({"commands":[
        {"tool":"local_file","params":{"action":"read","path":"probe.txt"}},
        {"tool":"local_file","params":{"action":"edit","path":"probe.txt","target_content":"original","replacement_content":"changed"}}
    ]})).unwrap();
    let result = run_agent_step(&db, "tool-params-edit", &inst, &registry, Some(&root));
    assert!(result.success, "{}", result.stderr);
    assert_eq!(std::fs::read_to_string(root.join("probe.txt")).unwrap(), "changed\n");
    for container in ["params", "args", "command"] {
        let mut leaf = json!({"tool":"local_file"});
        leaf[container] = json!({"action":"read","path":"../outside.txt"});
        let inst: AgentInstruction = serde_json::from_value(json!({"commands":[leaf]})).unwrap();
        let result = run_agent_step(&db, "tool-params-boundary", &inst, &registry, Some(&root));
        assert!(!result.success, "path escape via {container}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tool_params_precedence_and_parallel_classification_use_selected_payload() {
    let raw = json!({"tool":"local_file","args":{"action":"edit","path":"a","target_content":"x","replacement_content":"y"},
        "params":{"action":"read","path":"b"},"command":{"action":"list","path":"c"}});
    let inst: AgentInstruction = serde_json::from_value(raw.clone()).unwrap();
    let node: crate::agent::plan_tree::PlanNodeObj = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(inst.get_params(), node.get_args().unwrap());
    assert_eq!(inst.get_action(), "edit");
    assert!(is_write_or_side_effect_tool(&inst));
    let legacy = json!({"tool":"local_file","command":{"action":"read","path":"formal"},"params":{"action":"edit","path":"auxiliary"}});
    let instruction: AgentInstruction = serde_json::from_value(legacy.clone()).unwrap();
    let legacy_node: crate::agent::plan_tree::PlanNodeObj = serde_json::from_value(legacy).unwrap();
    assert_eq!(instruction.get_params(), json!({"action":"read","path":"formal"}));
    assert_eq!(instruction.get_params(), legacy_node.get_args().unwrap());
    let shell: AgentInstruction = serde_json::from_value(json!({"tool":"local_shell","args":{"command":"git reset --hard"}})).unwrap();
    assert!(is_write_or_side_effect_tool(&shell));
    assert_eq!(extract_shell_command_string(&shell).unwrap(), "git reset --hard");
}

#[test]
fn tool_params_mcp_payload_and_nested_business_fields_are_preserved() {
    let payload = json!({"path":"a.txt","params":{"command":"business"},"args":["x"],"command":{"future":true},"secret":"not-logged"});
    for container in ["params", "args", "command"] {
        let mut raw = json!({"tool":"remote/example"}); raw[container] = payload.clone();
        let inst: AgentInstruction = serde_json::from_value(raw.clone()).unwrap();
        let node: crate::agent::plan_tree::PlanNodeObj = serde_json::from_value(raw).unwrap();
        assert_eq!(inst.get_params(), payload);
        assert_eq!(node.get_args().unwrap(), payload);
        assert_eq!(crate::agent::tool_params::resolve_tool_call("remote/example",
            node.args.as_ref(), node.params.as_ref(), node.command.as_ref(), std::iter::empty()).unwrap(), payload);
    }
}

fn shell_params_fixture() -> (PathBuf,DbState,PluginRegistry) {
    let root=std::env::temp_dir().join(format!("free-api-shell-params-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let db=DbState(Arc::new(Mutex::new(init_db(&PathBuf::from(":memory:")).unwrap())));
    (root,db,PluginRegistry::new())
}

#[test]
fn tool_key_order_is_compatible_through_parsing_and_real_dispatch() {
    let (root, db, registry) = shell_params_fixture();
    for raw in [
        r#"{"tool":"local_shell","params":{"command":"printf key-order-ok"},"description":"check"}"#,
        r#"{"description":"check","params":{"command":"printf key-order-ok"},"tool":"local_shell"}"#,
    ] {
        let extracted = crate::agent::utils::extract_json_from_text(raw).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&extracted).unwrap();
        crate::agent::utils::normalize_raw_instruction_value(&mut value);
        let inst: AgentInstruction = serde_json::from_value(value).unwrap();
        let result = run_agent_step(&db, "key-order-compatibility", &inst, &registry, Some(&root));
        assert!(result.success, "{}", result.stderr);
        assert!(result.stdout.contains("\n--- STDOUT ---\nkey-order-ok\n"), "{}", result.stdout);
        assert!(result.stdout.contains("exit_code: 0"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documented_plan_tree_resolves_the_actual_resource_receipt() {
    let (root, db, registry) = shell_params_fixture();
    let core = include_str!("../../../prompts/core_logic.md");
    let section = core.split("### 形态 C：").nth(1).unwrap();
    let example = section.split("```json\n").nth(1).unwrap().split("```").next().unwrap();
    let inst: AgentInstruction = serde_json::from_str(example).unwrap();
    let result = run_agent_step(&db, "documented-tree", &inst, &registry, Some(&root));
    assert!(result.success, "{}", result.stderr);
    let resources = crate::agent::task_store::list_resource_refs(&db, "documented-tree").unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].label, "调查记录");
    assert!(result.stdout.contains(&resources[0].resource_id));
    assert!(!result.stdout.contains("$step1"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_single_step_executes_preserved_cwd_args_and_env() {
    let (root,db,registry)=shell_params_fixture();
    let inst: AgentInstruction=serde_json::from_value(json!({"tool":"local_shell","command":"sh",
        "params":{"cwd":"sub","args":["-c","printf '%s:%s' \"$CHECK\" \"${PWD##*/}\""],"env":{"CHECK":"canon"}},
        "timeout_secs":30,"background":false})).unwrap();
    assert!(crate::agent::tools::input_validation::instruction_rejection(&db,&inst).is_none());
    let result=run_agent_step(&db,"shell-params-single",&inst,&registry,Some(&root));
    assert!(result.success,"{}",result.stderr);
    assert!(result.stdout.contains("canon:sub"),"{}",result.stdout);
    assert_eq!(inst.get_params()["cwd"],json!("sub"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_direct_dispatch_shorthand_uses_existing_default_directory() {
    let (root,db,registry)=shell_params_fixture();
    let result=execute_command_inner(&db,"shell-params-direct","local_shell","exec",&json!("pwd"),&registry,Some(&root));
    assert!(result.success,"{}",result.stderr);
    assert!(result.stdout.contains(root.to_str().unwrap()));
    let result=crate::agent::tools::local_shell::dispatch(&db,"shell-params-handler","exec",&json!("printf handler-ok"),Some(&root));
    assert!(result.success && result.stdout.contains("handler-ok"),"{}",result.stderr);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_invalid_or_conflicting_parameters_do_not_execute() {
    let (root,db,registry)=shell_params_fixture();
    for raw in [json!({"tool":"local_shell","command":23}),json!({"tool":"local_shell","params":[]}),
        json!({"tool":"local_shell","command":" \n "}),
        json!({"tool":"local_shell","command":"touch marker-a","params":{"command":"touch marker-b"}}),
        json!({"tool":"local_shell","command":"pwd","cwd":"one","params":{"cwd":"two"}})] {
        let inst: AgentInstruction=serde_json::from_value(raw).unwrap();
        let pre=crate::agent::tools::input_validation::instruction_rejection(&db,&inst).unwrap();
        assert!(pre.stderr.contains("尚未执行"),"{}",pre.stderr);
        let result=run_agent_step(&db,"shell-params-invalid",&inst,&registry,Some(&root));
        assert_eq!(result.route,crate::agent::tools::input_validation::INVALID_INPUT_ROUTE);
    }
    assert!(!root.join("marker-a").exists() && !root.join("marker-b").exists());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_shorthand_still_obeys_cwd_boundary() {
    let (root,db,registry)=shell_params_fixture();
    let outside=std::env::temp_dir().join(format!("free-api-outside-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).unwrap();
    let inst: AgentInstruction=serde_json::from_value(json!({"tool":"local_shell","command":"pwd","cwd":outside})).unwrap();
    let result=run_agent_step(&db,"shell-params-boundary",&inst,&registry,Some(&root));
    assert!(!result.success);
    assert_eq!(result.route,"local_shell:cwd_denied","{}",result.stderr);
    std::fs::remove_dir_all(outside).unwrap();std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_safety_reads_the_same_merged_args_as_execution() {
    for raw in [json!({"tool":"local_shell","command":"git reset --hard"}),
        json!({"tool":"local_shell","command":"sh","params":{"args":["-c","git reset --hard"]}})] {
        let inst: AgentInstruction=serde_json::from_value(raw).unwrap();
        assert!(is_write_or_side_effect_tool(&inst));
        assert_eq!(extract_shell_command_string(&inst).unwrap(),"git reset --hard");
        assert_ne!(git_destructive_review(&extract_shell_command_string(&inst).unwrap()),GitDestructiveVerdict::NotDestructive);
    }
}
#[test]
fn shell_params_pipeline_accepts_params_and_string_without_dropping_options() {
    let (root,db,registry)=shell_params_fixture();
    let inst: AgentInstruction=serde_json::from_value(json!({"commands":[
        {"tool":"local_shell","command":"pwd","cwd":"sub"},
        {"tool":"local_shell","params":{"command":"printf pipeline-ok"}}
    ]})).unwrap();
    let result=run_agent_step(&db,"shell-params-pipeline",&inst,&registry,Some(&root));
    assert!(result.success,"{}",result.stderr);
    assert!(result.stdout.contains("pipeline-ok") && result.stdout.contains(root.join("sub").to_str().unwrap()));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn shell_params_parallel_and_plan_tree_share_the_canonical_contract() {
    let (root,db,registry)=shell_params_fixture();
    for raw in [json!({"parallel":[
        {"tool":"local_shell","command":"pwd","cwd":"sub"},
        {"tool":"local_shell","params":{"command":"printf parallel-ok"}}
    ]}),json!({"plan_tree":[
        {"tool":"local_shell","command":"pwd","cwd":"sub"},
        {"tool":"local_shell","args":"printf tree-ok"}
    ]})] {
        let inst: AgentInstruction=serde_json::from_value(raw).unwrap();
        let result=run_agent_step(&db,"shell-params-tree",&inst,&registry,Some(&root));
        assert!(result.success,"{}",result.stderr);
        assert!(result.stdout.contains(root.join("sub").to_str().unwrap()),"{}",result.stdout);
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn shell_params_parallel_conflict_reports_error_without_running_bad_branch() {
    let (root,db,registry)=shell_params_fixture();
    let inst: AgentInstruction=serde_json::from_value(json!({"parallel":[
        {"tool":"local_shell","command":"touch marker-a","params":{"command":"touch marker-b"}},
        {"tool":"local_shell","params":{"command":"printf good-branch"}}
    ]})).unwrap();
    let result=run_agent_step(&db,"shell-params-conflict",&inst,&registry,Some(&root));
    assert!(!result.success && result.stderr.contains("参数冲突"),"{}",result.stderr);
    assert!(result.stdout.contains("成功: 1, 失败: 1"),"{}",result.stdout);
    assert!(!root.join("marker-a").exists() && !root.join("marker-b").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shell_params_pipeline_conflict_retains_completed_output_and_stops_later_steps() {
    let (root,db,registry)=shell_params_fixture();
    let inst:AgentInstruction=serde_json::from_value(json!({"commands":[
        {"tool":"local_shell","command":"printf first-completed"},
        {"tool":"local_shell","command":"touch marker-a","params":{"command":"touch marker-b"}},
        {"tool":"local_shell","command":"touch marker-later"}
    ]})).unwrap();
    let result=run_agent_step(&db,"shell-params-pipeline-fail",&inst,&registry,Some(&root));
    assert_eq!(result.route,"pipeline_fail_at_2");
    assert!(result.stdout.contains("first-completed"));
    assert!(result.stderr.contains("此前成功步骤已执行") && result.stderr.contains("参数冲突"));
    for name in ["marker-a","marker-b","marker-later"] { assert!(!root.join(name).exists()); }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shell_params_parallel_capacity_is_independent_of_script_classification() {
    use crate::agent::plan_tree_dispatch::validate_controlled_parallel_batch;
    let write =
        json!({"tool":"local_shell","command":"sh","params":{"args":["-c","git reset --hard"]}});
    assert!(validate_controlled_parallel_batch(&vec![write.clone(); 5]).is_ok());
    assert!(validate_controlled_parallel_batch(&vec![write; 6]).is_err());
    let read = json!({"tool":"local_shell","command":"sh","params":{"args":["-c","pwd"]}});
    assert!(validate_controlled_parallel_batch(&vec![read; 5]).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_five_writes_execute_and_six_are_rejected_before_any_write() {
    let (root, db, registry) = shell_params_fixture();
    for (count, prefix) in [(5, "allowed"), (6, "rejected")] {
        let branches: Vec<_> = (0..count).map(|i| json!({
            "tool":"local_file",
            "params":{"action":"write","path":format!("{prefix}-{i}.txt"),"content":format!("branch {i}")}
        })).collect();
        let instruction: AgentInstruction =
            serde_json::from_value(json!({"parallel":branches})).unwrap();
        let result = run_agent_step(
            &db,
            "parallel-five-writes",
            &instruction,
            &registry,
            Some(&root),
        );
        if count == 5 {
            assert!(result.success, "{}", result.stderr);
            for i in 0..count {
                assert_eq!(
                    std::fs::read_to_string(root.join(format!("{prefix}-{i}.txt"))).unwrap(),
                    format!("branch {i}")
                );
            }
        } else {
            assert!(!result.success);
            assert!(
                result.stderr.contains("最多允许 5") && result.stderr.contains("尚未执行"),
                "{}",
                result.stderr
            );
            for i in 0..count {
                assert!(!root.join(format!("{prefix}-{i}.txt")).exists());
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
