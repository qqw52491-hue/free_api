    use super::*;

    #[test]
    fn xargs_recurses_into_inner_command() {
        let ro = |cmd: &str| {
            AuditLedger::is_readonly_shell_command(&serde_json::json!({ "command": cmd }))
        };
        // 无旗标 + 只读内层：放行（实测审查者 `find | xargs wc -l` 被拒的自救护栏位）
        assert!(ro("find src -name '*.rs' | xargs wc -l"));
        assert!(ro("xargs wc -l"));
        assert!(ro("xargs ls"));
        // 内层非只读：整条拒绝
        assert!(!ro("xargs rm"));
        assert!(!ro("find . | xargs rm -rf"));
        assert!(!ro("xargs sed -i s/a/b/"));
        // 带旗标：保守拒绝——旗标值可能与白名单命令同名（-I ls rm 若只看
        // "首个非旗标词元"会把 ls 当内层命令误判只读，实际执行的是 rm）
        assert!(!ro("xargs -0 wc"));
        assert!(!ro("xargs -I {} echo {}"));
        assert!(!ro("xargs -I ls rm"));
        assert!(!ro("xargs -n1 rm"));
        // 空 xargs / 单独 xargs：拒绝
        assert!(!ro("xargs"));
    }

    #[test]
    fn output_redirection_disqualifies_readonly() {
        let ro = |cmd: &str| {
            AuditLedger::is_readonly_shell_command(&serde_json::json!({ "command": cmd }))
        };
        // 引号内的 `>` 是字面量，不算重定向
        assert!(ro("grep 'a > b' file.txt"));
        assert!(ro("echo hello"));
        // 任何输出重定向即写入，不再算只读
        assert!(!ro("echo hacked >> ~/.zshrc"));
        assert!(!ro("git diff > patch.diff"));
        assert!(!ro("cat a > b"));
        assert!(!ro("sed -n '1p' file > out.txt"));
    }

    #[test]
    fn sed_readonly_variants_are_classified_correctly() {
        let ro = |cmd: &str| {
            AuditLedger::is_readonly_shell_command(&serde_json::json!({ "command": cmd }))
        };
        // 纯读取：事故现场的排查形态，必须判只读
        assert!(ro("sed -n '265,340p' src-tauri/src/agent/main_loop.rs"));
        assert!(ro("sed 's/a/b/' file.txt"));
        assert!(ro("sed -E -n '1,3p' file.txt"));
        // 原地写入：一律非只读
        assert!(!ro("sed -i 's/a/b/' file.txt"));
        assert!(!ro("sed --in-place 's/a/b/' file.txt"));
        assert!(!ro("sed -in 's/a/b/' file.txt"));
        assert!(!ro("sed -ni 's/a/b/' file.txt"));
        // 脚本来自文件：内容不可静态审计，非只读
        assert!(!ro("sed -f script.sed file.txt"));
        // 脚本内嵌 w 写文件 / e 执行命令指令：非只读
        assert!(!ro("sed -n '2w /tmp/out.txt' file.txt"));
        assert!(!ro("sed '$w /tmp/out' file.txt"));
        assert!(!ro("sed -n '/x/e cat /etc/passwd' file.txt"));
    }

    #[test]
    fn test_render_ledger_small() {
        let records = vec![SideEffectRecord {
            step_id: 1,
            tool: "code_editor".to_string(),
            description: "created src/main.rs".to_string(),
            timestamp: "2026-08-17T00:00:00Z".to_string(),
            affected_paths: Vec::new(),
        }];
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("created src/main.rs"));
        assert!(rendered.contains("Step 1"));
    }

    #[test]
    fn test_render_ledger_folding() {
        let mut records = Vec::new();
        for i in 0..15 {
            records.push(SideEffectRecord {
                step_id: i,
                tool: "code_editor".to_string(),
                description: format!("modified file_{}.rs", i),
                timestamp: "2026-08-17T00:00:00Z".to_string(),
                affected_paths: Vec::new(),
            });
        }
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("早期已安全执行"));
        assert!(rendered.contains("modified file_14.rs"));
    }

    #[test]
    fn test_is_mutating_side_effect() {
        use serde_json::json;

        // 纯只读命令应返回 false
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["-C", "/path", "show", "42b105a"]})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["log", "--oneline", "-10"]})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/ripgrep_search",
            &json!({"pattern": "fn main"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/read_text_file",
            &json!({"path": "src/main.rs"})
        ));

        // 产生副作用的命令应返回 true
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/write_file",
            &json!({"path": "src/main.rs", "content": "fn main() {}"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/edit_file",
            &json!({"path": "src/main.rs"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["commit", "-m", "fix"]})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "rm", "args": ["-rf", "dist"]})
        ));

        // local_file 原生工具针对项目代码写入/编辑入台账
        assert!(AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "write", "path": "src/main.rs", "content": "fn main() {}"})
        ));
        assert!(AuditLedger::is_mutating_side_effect_with_action(
            "local_file",
            "edit",
            &json!({"path": "src/lib.rs", "target_content": "old", "replacement_content": "new"})
        ));

        // local_file 针对 session_tmp 草稿豁免入台账
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "write", "path": "tmp/draft.md", "content": "draft"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "edit", "path": "./tmp/draft.md", "target_content": "a", "replacement_content": "b"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "write", "path": "src/main.rs", "content": "tmp", "is_tmp": true})
        ));

        // local_file 只读动作不入台账
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "read", "path": "src/main.rs"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_file",
            &json!({"action": "list", "path": "."})
        ));

        // local_shell 原生工具：只读命令免入台账，副作用命令严格入台账
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_shell",
            &json!({"command": "git status"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "local_shell",
            &json!({"command": "ls -la"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "local_shell",
            &json!({"command": "cargo test"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "local_shell",
            &json!({"command": "rm -rf target"})
        ));
    }

    #[test]
    fn test_format_side_effect_params_preserves_valid_json() {
        use serde_json::json;

        let big_content = "X".repeat(500);
        let params = json!({
            "path": "/home/user/project/src/main.rs",
            "content": big_content
        });

        let formatted = AuditLedger::format_side_effect_params(&params);
        assert!(formatted.contains("/home/user/project/src/main.rs"));
        assert!(formatted.contains("[...content 载荷已持久化，略...]"));
        // 必须能够被重新解析为合法 JSON
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();
        assert_eq!(parsed["path"], "/home/user/project/src/main.rs");

        // edit 载荷键（target_content/replacement_content）与超长命令同样瘦身
        let edit_params = json!({
            "action": "edit",
            "path": "src/foo.rs",
            "target_content": "Y".repeat(300),
            "replacement_content": "Z".repeat(300),
        });
        let formatted_edit = AuditLedger::format_side_effect_params(&edit_params);
        assert!(!formatted_edit.contains("YYYY"));
        assert!(formatted_edit.contains("target_content 载荷已持久化"));
        assert!(formatted_edit.contains("replacement_content 载荷已持久化"));
        // 短参数原样保留
        let small = json!({"command": "cargo check"});
        assert!(AuditLedger::format_side_effect_params(&small).contains("cargo check"));
    }

    #[test]
    fn state_probe_commands_are_readonly_not_side_effects() {
        // 运维探针必须判只读：若误记为副作用会清空 change_verification，
        // 形成"探测即重置验证态"的死锁（pacman 卸载事故现场）。
        let ro = |cmd: &str| {
            AuditLedger::is_readonly_shell_command(&serde_json::json!({ "command": cmd }))
        };
        // 存在性/包/服务探针
        assert!(ro("which workbuddy"), "which");
        assert!(ro("command -v workbuddy"));
        assert!(ro("test -e /opt/WorkBuddy"));
        assert!(ro("[ -e /usr/bin/workbuddy ]"));
        assert!(ro("[[ -d /opt/WorkBuddy ]]"));
        assert!(ro("stat /usr/bin/workbuddy"));
        assert!(ro("pacman -Q workbuddy"));
        assert!(ro("pacman -Qs workbuddy"));
        assert!(ro("pacman --query workbuddy"));
        assert!(ro("dpkg -s workbuddy"));
        assert!(ro("dpkg -l"));
        assert!(ro("rpm -q workbuddy"));
        assert!(ro("apt list --installed"));
        assert!(ro("apt-get -s install foo"), "模拟运行只读");
        assert!(ro("systemctl status workbuddy.service"));
        assert!(ro("systemctl --user is-active foo"));
        assert!(ro("systemctl is-enabled foo"));
        assert!(ro("service nginx status"));
        assert!(ro("pgrep -x workbuddy"));
        assert!(ro("ps -p 1234"));
        assert!(ro("journalctl -u foo.service -n 50"));
        assert!(ro("hostnamectl"));
        assert!(ro("timedatectl status"));
        // HTTP/网络断言
        assert!(ro("curl -f http://localhost:8790/health"));
        assert!(ro("curl -s -o /dev/null -w '%{http_code}' http://x"));
        assert!(ro("curl -fsSI http://x"));
        assert!(ro("nc -z 127.0.0.1 8045"));
        assert!(ro("ping -c 1 127.0.0.1"));
        assert!(ro("wget --spider http://x"));
        // 校验/过滤族
        assert!(ro("sha256sum app.tar.gz"));
        assert!(ro("diff -q a.txt b.txt"));
        assert!(ro("jq -e '.ok' resp.json"));
        assert!(ro("sleep 2"));
        // 包装器剥壳
        assert!(ro("sudo pacman -Q workbuddy"), "sudo 剥壳后仍是查询");
        assert!(ro("timeout 5 curl -f http://x"), "timeout 剥壳");
        // 变更形态保持非只读（判别失败一律保守记副作用）
        assert!(!ro("sudo pacman -Rns workbuddy"), "卸载是真实写入");
        assert!(!ro("pacman -S workbuddy"));
        assert!(!ro("systemctl stop workbuddy"));
        assert!(!ro("systemctl --user disable foo"));
        assert!(!ro("service nginx restart"));
        assert!(!ro("apt install foo"));
        assert!(!ro("apt remove foo"));
        assert!(!ro("dpkg -i foo.deb"));
        assert!(!ro("curl -X POST http://x/api"));
        assert!(!ro("curl -d 'a=b' http://x"));
        assert!(!ro("curl -o out.html http://x"), "落盘即写文件");
        assert!(!ro("curl -F 'f=@a.txt' http://x"));
        assert!(!ro("wget -O out.html http://x"));
        assert!(!ro("journalctl --vacuum-size=100M"));
        assert!(!ro("journalctl --rotate"));
        assert!(!ro("ip link set eth0 up"));
        assert!(!ro("ip addr add 10.0.0.1/24 dev eth0"));
        assert!(!ro("mount /dev/sda1 /mnt"));
        assert!(!ro("env FOO=1 rm x"), "env 携带命令即执行");
        assert!(!ro("command rm x"));
        assert!(!ro("tar czf out.tgz src/"));
        assert!(!ro("tar xf a.tar"));
        assert!(ro("tar tf a.tar"), "tar -t 列档只读");
        assert!(ro("tar --list a.tar"));
        assert!(ro("ip addr show"), "ip 查询只读");
        assert!(ro("ip a"));
        assert!(ro("docker inspect workbuddy"));
        assert!(ro("docker ps -a"));
        assert!(ro("kubectl get pods"));
        assert!(!ro("docker run --rm alpine true"));
        assert!(!ro("kubectl delete pod x"));
        assert!(ro("nc -z localhost 22"));
        assert!(!ro("nc -l 8080"), "监听态非只读");
        assert!(!ro("umount /mnt"));
    }

    #[test]
    fn compound_commands_require_every_segment_readonly() {
        let ro = |cmd: &str| {
            AuditLedger::is_readonly_shell_command(&serde_json::json!({ "command": cmd }))
        };
        // 全段只读 → 只读
        assert!(ro("which workbuddy && pacman -Qs workbuddy"));
        assert!(ro("test -e /opt/x || echo missing"));
        assert!(ro("ls /opt; stat /opt/WorkBuddy"));
        assert!(ro("cat a.txt | grep foo"));
        assert!(ro("[[ -e a || -e b ]]"), "[[ ]] 内 || 是表达式非命令分隔");
        assert!(ro("[[ -e a ]] && [[ -e b ]]"));
        // 首段只读 + 后段写入 → 非只读（堵漏：which x && rm y）
        assert!(!ro("which workbuddy && rm -rf /opt/WorkBuddy"), "首段只读不得掩护后段写入");
        assert!(!ro("ls /opt; rm x"));
        assert!(!ro("cat a | tee b.txt"), "管道末段 tee 写文件");
        assert!(!ro("which x || pacman -Rns x"));
        assert!(!ro("echo ok & rm -rf /tmp/x"), "后台段也是命令");
        // 重定向/命令替换/进程替换守卫
        assert!(!ro("echo done > /tmp/marker"));
        assert!(!ro("echo $(rm -rf /tmp/x)"), "命令替换内层不可审计");
        assert!(!ro("echo `id`"));
        assert!(!ro("diff <(cat a) b"), "进程替换会执行内层命令");
        assert!(!ro("ls >(cat > out.txt)"));
        // 非分隔符形态不得误拆
        assert!(ro("curl -f http://x 2>&1"), "2>&1 的 & 不是分隔");
        assert!(!ro("curl -o out.txt http://x 2>&1"), "仍检出 -o 写盘");
        assert!(!ro("cmd &> /tmp/log"), "&> 重定向整体检出");
        // 引号内分隔符不拆段
        assert!(ro("echo \"a && b\""), "引号内 && 不是命令分隔");
        assert!(ro("grep -q 'a|b' file.txt"), "引号内 | 不是管道");
    }
