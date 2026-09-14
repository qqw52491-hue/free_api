//! 门禁任务矩阵：把真实任务的工具调用序列穿过审计门（副作用分类）与验证门
//! （自动认账决策），逐场景断言两层分类正确。
//! 覆盖：卸载/安装运维、编码、办公文档（PPT/Excel 提取）、混合任务、
//! 对抗偷写（漏记=验证层静默绕过）、双态边界（误记=幻影写入卡 finish）。

#[cfg(test)]
mod gate_matrix_tests {
    use crate::agent::change_verification::{
        decide_auto_verify, AutoVerifyDecision,
    };
    use crate::agent::context::audit::AuditLedger;
    use serde_json::json;
    use AutoVerifyDecision::{AbsenceProbeGuidance, AutoVerify, Skip};

    /// 审计门：true=只读（不入账副作用），false=变更（入账，要求后续验证）
    fn ro(cmd: &str) -> bool {
        AuditLedger::is_readonly_shell_command(&json!({ "command": cmd }))
    }

    /// 待绑定引用集：模拟宿主把副作用描述+受影响路径拼成 pending_refs
    fn refs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn decide(cmd: &str, ok: bool, pending: &[String]) -> AutoVerifyDecision {
        decide_auto_verify(cmd, ok, pending, &[], None)
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 A：卸载任务（pacman -Rns → 探针验证）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_uninstall_task() {
        // 卸载动作必须入账为副作用
        assert!(!ro("sudo pacman -Rns workbuddy"));
        let pending = refs(&["local_shell: sudo pacman -Rns workbuddy --noconfirm"]);

        // 绑定的查询探针成功 → 自动认账（安装方向验证）；探针本身只读
        assert!(ro("pacman -Qs workbuddy"));
        assert_eq!(decide("pacman -Qs workbuddy", true, &pending), AutoVerify);
        assert_eq!(decide("which workbuddy", true, &pending), AutoVerify);
        assert_eq!(
            decide("systemctl status workbuddy.service", true, &pending),
            AutoVerify
        );

        // 绑定的存在性探针失败 → 方向不明，发举证指引，不自动闭合
        assert_eq!(
            decide("pacman -Qs workbuddy", false, &pending),
            AbsenceProbeGuidance
        );
        assert_eq!(
            decide("which workbuddy", false, &pending),
            AbsenceProbeGuidance
        );
        assert_eq!(
            decide("test -e /opt/WorkBuddy", false, &pending),
            AbsenceProbeGuidance
        );

        // 无关探针：不认账、不发指引——"探测了别的对象"不算这次变更的验证
        assert_eq!(decide("which python", true, &pending), Skip);
        assert_eq!(decide("which python", false, &pending), Skip);
        assert_eq!(decide("pacman -Q nginx", true, &pending), Skip);
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 B：安装任务 + 契约 verify_command
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_install_task_with_contract() {
        assert!(!ro("sudo pacman -S --noconfirm workbuddy"));
        let pending = refs(&["local_shell: sudo pacman -S --noconfirm workbuddy"]);

        // 绑定探针成功 → 认账
        assert_eq!(decide("pacman -Q workbuddy", true, &pending), AutoVerify);
        assert_eq!(decide("test -e /opt/workbuddy/bin", true, &pending), AutoVerify);
        assert!(ro("test -e /opt/workbuddy/bin"));

        // 契约 verify_command 精确匹配 → 无条件认账（即使不是探针/测试命令）
        let contract_vcs = vec!["systemctl is-enabled workbuddy"];
        assert_eq!(
            decide_auto_verify(
                "systemctl is-enabled workbuddy",
                true,
                &pending,
                &contract_vcs,
                None
            ),
            AutoVerify
        );
        // 契约命令加 sudo 前缀 ≠ 精确匹配 → 但绑定探针路径仍认账
        assert_eq!(
            decide_auto_verify(
                "sudo systemctl is-enabled workbuddy",
                true,
                &pending,
                &contract_vcs,
                None
            ),
            AutoVerify
        );
        // 宿主推断的验证命令（双向子串匹配）
        assert_eq!(
            decide_auto_verify("cargo test --lib", true, &pending, &[], Some("cargo test")),
            AutoVerify
        );
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 C：编码任务
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_code_task() {
        assert!(!ro("write_file")); // 非 shell 命令名不白名单——保守变更侧
        let pending = refs(&["write_file: /repo/src/main.rs"]);

        // 生态测试命令无条件认账
        assert_eq!(decide("cargo test", true, &pending), AutoVerify);
        assert_eq!(decide("cargo test --workspace", true, &pending), AutoVerify);
        assert_eq!(decide("npx tsc --noemit", true, &pending), AutoVerify);
        // cargo test 本身产生 target/ 写入 → 审计侧入账，验证决策层认账
        assert!(!ro("cargo test"));

        // 只读观察/无关探针不结账
        assert_eq!(decide("git status", true, &pending), Skip);
        assert_eq!(decide("which python", true, &pending), Skip);
        assert!(ro("git status"));
        assert!(ro("git diff"));
        assert!(ro("cat src/main.rs"));
        assert!(ro("cargo test > /tmp/log") == false); // 输出落盘 = 写入
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 D：办公文档任务（PPT/Excel）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_office_doc_task() {
        let pending = refs(&["write_file: /tmp/work/report.pptx"]);

        // 归档内窥是只读提取（pptx/xlsx 本质是 zip）
        assert!(ro("unzip -l report.pptx"));
        assert!(ro("zipinfo report.pptx"));
        assert!(ro("ls -la /tmp/work/"));
        // 提取类命令不是状态断言探针 → 不自动闭合（模型走 step_N 举证）
        assert_eq!(decide("unzip -l report.pptx", true, &pending), Skip);

        // 解释器提取脚本是变更类（可写）→ 入账、不认账
        assert!(!ro("python3 -c \"import pptx; print('slides')\""));
        // unzip -p/-l 到 stdout 经管道喂 grep：两段均只读 → 整串只读
        assert!(ro("unzip -p report.pptx ppt/slides/slide3.xml | grep 标题"));
        assert!(ro("unzip -l report.pptx | grep slide3"));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 E：对抗偷写——全部必须入账（漏记 = 验证层静默绕过）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_adversarial_smuggled_writes() {
        // 复合命令藏写段
        assert!(!ro("which workbuddy && rm -rf /tmp/x"));
        assert!(!ro("ls /tmp ; rm /tmp/x"));
        assert!(!ro("cat f | tee /tmp/out"));
        // find/fd 的执行类旗标
        assert!(!ro("find . -name '*.o' -delete"));
        assert!(!ro("find . -type f -exec rm {} +"));
        assert!(!ro("find . -execdir rm {} +"));
        assert!(!ro("find . -ok rm {} +"));
        assert!(!ro("find . -fprint /tmp/out"));
        assert!(!ro("fd tmp --exec rm"));
        assert!(!ro("fd tmp -x rm"));
        // git 双态子命令的变更形态
        assert!(!ro("git config core.hooksPath /tmp/evil"));
        assert!(!ro("git config user.email a@b.c"));
        assert!(!ro("git config --global init.defaultBranch x"));
        assert!(!ro("git branch -d feat"));
        assert!(!ro("git branch -D feat"));
        assert!(!ro("git branch newbranch"));
        assert!(!ro("git branch -m old new"));
        assert!(!ro("git tag v1.0"));
        assert!(!ro("git tag -d v1.0"));
        assert!(!ro("git tag -a v1 -m msg"));
        assert!(!ro("git remote add origin url"));
        assert!(!ro("git remote set-url origin url"));
        assert!(!ro("git remote prune origin"));
        assert!(!ro("git stash"));
        assert!(!ro("git stash pop"));
        assert!(!ro("git stash drop"));
        assert!(!ro("git submodule update --init"));
        assert!(!ro("git worktree add /tmp/w"));
        assert!(!ro("git symbolic-ref HEAD refs/heads/x"));
        assert!(!ro("git symbolic-ref -d HEAD"));
        assert!(!ro("git hash-object -w f.txt"));
        assert!(!ro("git diff --output=/tmp/p.patch"));
        assert!(!ro("git config --unset core.hooksPath"));
        // 输出落盘型旗标
        assert!(!ro("sort -o out.txt in.txt"));
        assert!(!ro("sort -oout.txt in.txt"));
        assert!(!ro("tree -o tree.txt"));
        assert!(!ro("curl -o file.tar.gz http://x/y"));
        // 系统状态变更
        assert!(!ro("date -s '2030-01-01'"));
        assert!(!ro("date --set='2030-01-01'"));
        assert!(!ro("date 12312359"));
        assert!(!ro("hostname pwned"));
        assert!(!ro("mount -a"));
        assert!(!ro("mount --all"));
        assert!(!ro("mount /dev/sda1 /mnt"));
        assert!(!ro("mount -o remount,rw /"));
        assert!(!ro("sysctl -w net.ipv4.ip_forward=1"));
        assert!(!ro("sysctl net.ipv4.ip_forward=1"));
        assert!(!ro("sysctl -p"));
        assert!(!ro("dmesg -C"));
        assert!(!ro("dmesg -c"));
        assert!(!ro("dmesg -n8"));
        assert!(!ro("fuser -k 8080/tcp"));
        assert!(!ro("nmcli con up myvpn"));
        assert!(!ro("nmcli radio wifi off"));
        assert!(!ro("bridge link set dev x up"));
        assert!(!ro("virsh start vm1"));
        assert!(!ro("helm install app chart"));
        assert!(!ro("pip install requests"));
        assert!(!ro("npm install lodash"));
        assert!(!ro("pacman -Rns nginx"));
        assert!(!ro("timedatectl set-time '2030-01-01'"));
        assert!(!ro("loginctl lock-session"));
        assert!(!ro("service sshd restart"));
        assert!(!ro("ifconfig eth0 up"));
        assert!(!ro("arp -d host"));
        assert!(!ro("route add -net 10.0.0.0"));
        assert!(!ro("go env -w GOPROXY=x"));
        // 重定向类
        assert!(!ro("[[ a ]] > /tmp/out"));
        assert!(!ro("[[ x > y ]] 2>/dev/null > /tmp/out"));
        assert!(!ro("ls > /tmp/ls.txt"));
        assert!(!ro("echo x >> log.txt"));
        // 命令替换/进程替换：内层不可审计
        assert!(!ro("echo $(rm -rf /tmp/x)"));
        assert!(!ro("cat `rm -rf /tmp/x`"));
        assert!(!ro("diff <(ls a) <(ls b)"));
        assert!(!ro("bash -c 'ls'"));
        assert!(!ro("eval ls"));
        assert!(!ro("xargs rm"));
        // rpm/dpkg/yum/zypper 变更形态
        assert!(!ro("rpm -i pkg.rpm"));
        assert!(!ro("dpkg -i pkg.deb"));
        assert!(!ro("yum install vim"));
        assert!(!ro("zypper install vim"));
        assert!(!ro("apk add vim"));
        assert!(!ro("helm template x --output-dir /tmp/y"));
        // 双态 minikube
        assert!(!ro("minikube delete"));
        assert!(!ro("virsh destroy vm1"));
        // busctl 方法调用可触发任意副作用
        assert!(!ro("busctl call org.x /o org.x.I M"));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 F：双态边界——必须保持只读（误记 = 幻影写入卡 finish）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_boundary_readonly() {
        // [[ ]] 内比较不是重定向
        assert!(ro("[[ $a > $b ]]"));
        assert!(ro("[[ a > b && c < d ]]"));
        assert!(ro("[[ -e /opt/x || -e /opt/y ]]"));
        assert!(ro("test a \\> b"));
        // 测试内建
        assert!(ro("test -e /opt/x"));
        assert!(ro("[ -f /etc/hosts ]"));
        // git 列举形态
        for c in [
            "git branch",
            "git branch -a",
            "git branch -v",
            "git branch --show-current",
            "git branch --contains abc123",
            "git branch -l",
            "git tag",
            "git tag -l v1.*",
            "git tag -v v1.0",
            "git tag -n3 -l",
            "git remote",
            "git remote -v",
            "git remote show origin",
            "git remote get-url origin",
            "git config user.name",
            "git config --global user.email",
            "git config --get user.name",
            "git config --list",
            "git config --file /home/u/.gitconfig user.name",
            "git stash list",
            "git stash show -p",
            "git submodule status",
            "git submodule",
            "git worktree list",
            "git blame src/main.rs",
            "git log --oneline -5",
            "git diff HEAD~1",
            "git rev-parse HEAD",
            "git ls-files",
            "git for-each-ref",
            "git cat-file -p abc123",
            "git fsck",
            "git symbolic-ref HEAD",
            "git hash-object f.txt",
            "git notes list",
            "git --version",
            "git -C /repo status",
            "git -c core.pager=cat log -1",
        ] {
            assert!(ro(c), "应为只读: {c}");
        }
        // sort/tree 只读形态
        assert!(ro("sort -n data.txt"));
        assert!(ro("sort -u -t: -k1 /etc/passwd"));
        assert!(ro("tree -L 2"));
        // 系统观测
        assert!(ro("date"));
        assert!(ro("date -d '2024-01-01'"));
        assert!(ro("date +%Y-%m-%d"));
        assert!(ro("hostname"));
        assert!(ro("hostname -I"));
        assert!(ro("mount"));
        assert!(ro("mount -l"));
        assert!(ro("sysctl -a"));
        assert!(ro("sysctl -n kern.ostype"));
        assert!(ro("sysctl kern.ostype"));
        assert!(ro("dmesg"));
        assert!(ro("dmesg -T"));
        assert!(ro("dmesg -w"));
        assert!(ro("dmesg -l err"));
        assert!(ro("fuser 8080/tcp"));
        assert!(ro("journalctl -u sshd -n 50"));
        assert!(ro("nmcli device status"));
        assert!(ro("nmcli"));
        assert!(ro("nmcli con show"));
        assert!(ro("bridge link show"));
        assert!(ro("bridge fdb show"));
        assert!(ro("virsh list --all"));
        assert!(ro("helm list"));
        assert!(ro("helm status app"));
        assert!(ro("apt-config dump"));
        assert!(ro("service sshd status"));
        assert!(ro("service --status-all"));
        assert!(ro("ifconfig"));
        assert!(ro("arp -a"));
        assert!(ro("route -n"));
        assert!(ro("sysctl --help"));
        // ctl 裸跑/列举
        assert!(ro("hostnamectl"));
        assert!(ro("timedatectl"));
        assert!(ro("loginctl list-sessions"));
        assert!(ro("loginctl show-user u1"));
        assert!(ro("machinectl list"));
        assert!(ro("busctl list"));
        assert!(ro("busctl introspect org.x /o"));
        // 包管理只读子集
        assert!(ro("pacman -Q nginx"));
        assert!(ro("pacman -Qs workbuddy"));
        assert!(ro("dpkg -l nginx"));
        assert!(ro("dpkg-query -W x"));
        assert!(ro("rpm -qa"));
        assert!(ro("rpm -V nginx"));
        assert!(ro("rpm --checksig pkg.rpm"));
        assert!(ro("apt list --installed"));
        assert!(ro("apt-get -s upgrade"));
        assert!(ro("apt-cache policy x"));
        assert!(ro("apt-file search libx.so"));
        assert!(ro("pip list"));
        assert!(ro("pip show requests"));
        assert!(ro("pipx list"));
        assert!(ro("npm ls"));
        assert!(ro("npm view lodash"));
        assert!(ro("yarn list"));
        assert!(ro("pnpm outdated"));
        assert!(ro("gem list"));
        assert!(ro("yum list installed"));
        assert!(ro("dnf check-update"));
        assert!(ro("zypper se vim"));
        assert!(ro("apk info"));
        assert!(ro("repoquery -l pkg"));
        assert!(ro("go version"));
        assert!(ro("go env GOPATH"));
        assert!(ro("go doc fmt.Println"));
        assert!(ro("rustup show"));
        assert!(ro("cargo --version"));
        assert!(ro("node --version"));
        assert!(ro("python3 -V"));
        assert!(ro("systemctl --version"));
        // shell 内建/进程态
        assert!(ro("printf '%s' hello"));
        assert!(ro("cd /tmp"));
        assert!(ro("export FOO=bar"));
        assert!(ro("umask 022"));
        assert!(ro("ulimit -n"));
        assert!(ro("set -e"));
        assert!(ro("alias ll='ls -l'"));
        assert!(ro("jobs"));
        // 读取器
        assert!(ro("unzip -l x.zip"));
        assert!(ro("tar -tf a.tar"));
        assert!(ro("tar tf a.tar"));
        assert!(ro("sed -n '1,5p' f.txt"));
        assert!(ro("nm a.out"));
        assert!(ro("readelf -h a.out"));
        assert!(ro("strings a.out | grep main"));
        assert!(ro("base64 f.bin"));
        assert!(ro("od -x f.bin | head"));
        assert!(ro("lsof -i :8080"));
        assert!(ro("pgrep -af node"));
        assert!(ro("curl -fsSI http://x/ -o /dev/null"));
        assert!(ro("curl -X GET http://x"));
        assert!(ro("wget --spider http://x"));
        assert!(ro("nc -z host 80"));
        // kubectl/docker 查询
        assert!(ro("kubectl get pods"));
        assert!(ro("docker ps"));
        assert!(ro("docker inspect c1"));
        // 复合全只读
        assert!(ro("cat a.txt | sort | uniq -c"));
        assert!(ro("ls /opt && which uvx"));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 H：重定向哑目标 + 包装剥壳 + macOS 工具族（第二轮猎捕补网）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_noop_targets_and_wrappers() {
        // /dev 哑目标：丢弃输出不是写入
        assert!(ro("curl -fsS http://x/ -o /dev/null"));
        assert!(ro("ls / > /dev/null"));
        assert!(ro("cat f 2>/dev/null"));
        assert!(ro("ls / > /dev/stdout"));
        assert!(ro("ls / > /dev/fd/1"));
        // 真写入仍判变更
        assert!(!ro("ls / > /tmp/out"));
        assert!(!ro("ls / > /dev/sda"));
        assert!(!ro("cat f > /dev/nullx"));
        // 包装剥壳：command/builtin 剥到内层评估
        assert!(ro("command cat /etc/hosts"));
        assert!(ro("builtin cd /tmp"));
        assert!(ro("command ls /"));
        assert!(!ro("command rm -rf /tmp/x"));
        assert!(ro("builtin history -c")); // 清进程内历史——进程态不入账
        // ionice/taskset 吃值旗标剥壳
        assert!(ro("taskset -c 0 cat /proc/cpuinfo"));
        assert!(ro("ionice -c3 cat /etc/hosts"));
        assert!(!ro("taskset -c 0 rm /tmp/x"));
        // sudo 直跟剥壳
        assert!(ro("sudo cat /etc/shadow"));
        assert!(!ro("sudo rm /etc/passwd"));
        // crontab
        assert!(ro("crontab -l"));
        assert!(ro("crontab -u alice -l"));
        assert!(!ro("crontab -e"));
        assert!(!ro("crontab -r"));
        assert!(!ro("crontab /tmp/newcron"));
        // macOS 工具族
        assert!(ro("defaults read com.apple.finder"));
        assert!(ro("defaults domains"));
        assert!(!ro("defaults write com.x k v"));
        assert!(!ro("defaults delete com.x k"));
        assert!(ro("launchctl list"));
        assert!(ro("launchctl print gui/501"));
        assert!(!ro("launchctl load /x.plist"));
        assert!(!ro("launchctl kickstart -k gui/501/com.x"));
        assert!(ro("mas list"));
        assert!(!ro("mas install 12345"));
        assert!(ro("log show --last 1h"));
        assert!(!ro("log collect --output /tmp/x"));
        assert!(ro("diskutil list"));
        assert!(ro("diskutil info /dev/disk0"));
        assert!(!ro("diskutil eraseDisk APFS x /dev/disk1"));
        assert!(ro("hdiutil info"));
        assert!(!ro("hdiutil attach x.dmg"));
        assert!(ro("codesign -dv /Applications/Safari.app"));
        assert!(ro("codesign --verify --verbose /App.app"));
        assert!(!ro("codesign -s 'Dev ID' /App.app"));
        assert!(ro("spctl --status"));
        assert!(!ro("spctl --master-disable"));
        assert!(ro("xattr -l file"));
        assert!(!ro("xattr -w k v file"));
        assert!(!ro("xattr -d com.apple.quarantine file"));
        assert!(ro("ioreg -l | head"));
        assert!(ro("system_profiler SPHardwareDataType"));
        assert!(ro("mdfind -name report"));
        // 终端态
        assert!(ro("tty"));
        assert!(ro("stty -a"));
        assert!(ro("tput cols"));
        assert!(ro("clear"));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 J：绑定精度——路径骨架词/扩展名/单元后缀不构成物理绑定
    // （假绑定 = 无关探针把别人的验证关账）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_binding_noise_tokens() {
        // 挂账写入只提到 config，与 /opt 探针无关
        let pending = refs(&["write_file: /repo/src/config options.rs"]);
        // "opt" ⊆ "options" 属假绑定——探针对象是 x，未引用本次改动对象
        assert_eq!(decide("test -e /opt/x", true, &pending), Skip);
        // ".conf" 扩展名段 ⊆ "config" 假绑定——/etc/random.conf 与本次改动无关
        assert_eq!(decide("test -f /etc/random.conf", true, &pending), Skip);
        // ".service" 单元后缀假绑定——探测别的服务不关这次验证
        assert_eq!(decide("systemctl status nginx.service", true, &pending), Skip);
        // 真实对象 token 仍绑定：路径目录名、文件名主干
        let pending2 = refs(&["local_shell: sudo pacman -S workbuddy"]);
        assert_eq!(decide("test -e /opt/workbuddy/bin", true, &pending2), AutoVerify);
        assert_eq!(
            decide("systemctl status workbuddy.service", true, &pending2),
            AutoVerify
        );
        assert_eq!(decide("test -f /etc/workbuddy.conf", true, &pending2), AutoVerify);
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 K：command -v 查询模式与 sudo 旗标剥壳回归
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_wrapper_query_modes() {
        // command -v/-V 是查询模式：参数是查询对象而非被执行命令 → 恒只读
        assert!(ro("command -v workbuddy"));
        assert!(ro("command -v rm"));       // 查的是 rm 但并未执行 rm
        assert!(ro("command -V anything"));
        // command/builtin 直跟内层命令仍剥壳评估
        assert!(ro("command cat /etc/hosts"));
        assert!(!ro("command rm -rf /tmp/x"));
        // sudo 带旗标剥壳到真实命令
        assert!(ro("sudo -n cat /etc/hosts"));
        assert!(ro("sudo -u root ls /"));
        assert!(ro("sudo --user=nobody stat /etc/passwd"));
        assert!(!ro("sudo -n rm /etc/passwd"));
        assert!(!ro("sudo -u root rm /tmp/x"));
        assert!(!ro("sudo -u root bash -c 'id'"));
        // 真空断言探针不收录：404/搜不到也 exit 0 的形态不构成验证
        let pending = refs(&["local_shell: curl -o out http://x/api"]);
        assert_eq!(decide("curl -o /dev/null http://x/api", true, &pending), Skip);
        assert_eq!(decide("zypper se workbuddy", true, &pending), Skip);
        // 真断言仍认账
        assert_eq!(decide("curl -f http://x/api", true, &pending), AutoVerify);
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 L：非 shell 语言的执行型 MCP——弱 exec/run 标记不能吃 shell 检查放行
    // （"DELETE FROM users" 不是 shell 语法，破坏性检查对它隐形 = 漏记）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_non_shell_exec_tools() {
        use crate::agent::context::audit::AuditLedger;
        let mutates = |tool: &str, params: serde_json::Value| {
            AuditLedger::is_mutating_side_effect(tool, &params)
        };
        // SQL/JS 执行型：弱 exec 标记 + 非 shell 内容 → 动词层 fail-closed 入账
        assert!(mutates("db/exec_sql", json!({"command": "SELECT 1"})));
        assert!(mutates("db/exec_sql", json!({"command": "DELETE FROM users"})));
        assert!(mutates("browser/exec_js", json!({"command": "document.title='x'"})));
        assert!(mutates("server/eval_code", json!({"command": "1+1"})));
        // 强壳标记仍走命令级分析，两侧判定一致
        assert!(!mutates("hub/run_command", json!({"command": "ls -la /tmp"})));
        assert!(!mutates("hub/run_command", json!({"args": ["ls", "-la", "/tmp"]})));
        assert!(mutates("hub/run_command", json!({"command": "rm -rf /tmp/x"})));
        assert!(!mutates("remote-hub/shell_exec", json!({"command": "curl -s http://x"})));
        assert!(mutates("remote-hub/shell_exec", json!({"command": "curl -o f http://x"})));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 M：dispatcher↔audit 重定向口径一致——只读探针不能被前置拦截，
    // 真写盘不能被放行（两侧漂移 = 假拦截或漏记）
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_redirect_parity() {
        use crate::agent::dispatcher::has_file_write_redirection as w;
        // 比较/哑目标/fd 复制——dispatcher 不得判写盘（audit 同款语义）
        for c in [
            "[[ $a > $b ]]",
            "[[ a > b && c < d ]]",
            "test a \\> b",
            "ls / > /dev/null",
            "ls / > /dev/stdout",
            "ls / > /dev/fd/1",
            "ls / 2>/dev/null",
            "ls / >nul",
            "ls / >NUL",
            "ls / 2>&1",
            "ls / >&-",
            "grep 'a > b' f.txt",
        ] {
            assert!(!w(c), "dispatcher 误判写盘: {c}");
            assert!(ro(c), "audit 误判写入: {c}");
        }
        // 真写盘——两侧都必须识别
        for c in [
            "[[ x > y ]] > /tmp/out",
            "ls > /tmp/ls.txt",
            "echo x >> log.txt",
            "ls / > /dev/sda",
            "ls / &> /tmp/both.log",
        ] {
            assert!(w(c), "dispatcher 漏判写盘: {c}");
            assert!(!ro(c), "audit 漏记写入: {c}");
        }
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 I：MCP 命名空间工具——写动词词表覆盖不到的变化操作必须入账
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_mcp_tool_side_effects() {
        use crate::agent::context::audit::AuditLedger;
        let mutates = |tool: &str, params: serde_json::Value| {
            AuditLedger::is_mutating_side_effect(tool, &params)
        };

        // Excel/PPT 类 MCP：format/rename/set/insert/merge 全是写入
        for t in [
            "excel-mcp-server/format_range",
            "excel-mcp-server/set_formula",
            "excel-mcp-server/rename_sheet",
            "excel-mcp-server/merge_cells",
            "excel-mcp-server/insert_rows",
            "excel-mcp-server/update_cell",
            "ppt-mcp-server/add_slide",
            "ppt-mcp-server/set_title",
            "ssh/execute_command",
            "ssh/run_remote",
            "db-mcp/insert_record",
            "fs/write_file",
        ] {
            assert!(mutates(t, json!({})), "MCP 变更工具应入账: {t}");
        }
        // 读动词前缀的 MCP 工具不入账
        for t in [
            "excel-mcp-server/read_sheet",
            "excel-mcp-server/get_workbook_metadata",
            "excel-mcp-server/list_sheets",
            "ppt-mcp-server/describe_presentation",
            "fs/read_text_file",
            "fs/list_directory",
            "db-mcp/query_table",
            "web/search_docs",
            "api/fetch_user",
            "server/readFile",      // camelCase
            "server/getFileInfo",   // camelCase
        ] {
            assert!(!mutates(t, json!({})), "MCP 只读工具不应入账: {t}");
        }
        // 词表边界：读动词起头但非独立词 → 仍按变更（fail-closed）
        assert!(mutates("srv/getaway_execute", json!({})));
        assert!(mutates("srv/readonly_apply", json!({})));
        // 原生非命名空间工具不受新规则影响
        assert!(!mutates("local_file", json!({"action": "read_file", "path": "/x"})));
        assert!(mutates("local_file", json!({"action": "write_file", "path": "/x"})));
    }

    // ────────────────────────────────────────────────────────────────
    // 场景 G：验证决策的负路径——无绑定不认账
    // ────────────────────────────────────────────────────────────────
    #[test]
    fn scenario_verify_negative_paths() {
        let pending = refs(&["write_file: /repo/src/lib.rs"]);
        // 无关探针成功也不认账
        assert_eq!(decide("which docker", true, &pending), Skip);
        // 非探针非测试的观察命令不认账
        assert_eq!(decide("ls -la", true, &pending), Skip);
        assert_eq!(decide("git log -1", true, &pending), Skip);
        // 探针失败且未绑定 → 不发指引
        assert_eq!(decide("which docker", false, &pending), Skip);
        // 测试命令失败 → 不认账也不发指引（测试失败语义明确，无需举证）
        assert_eq!(decide("cargo test", false, &pending), Skip);
        // 空 pending（无待验证写入）：宿主层面根本不会进决策——
        // 这里验证决策函数自身对空 refs 的行为（探针无法绑定）
        assert_eq!(decide("which x", true, &[]), Skip);
        assert_eq!(decide("cargo test", true, &[]), AutoVerify);
        // inferred 为空串：contains("") 恒真——必须防"空推断认账一切"
        assert_eq!(
            decide_auto_verify("ls /tmp", true, &pending, &[], Some("")),
            Skip
        );
    }
}
