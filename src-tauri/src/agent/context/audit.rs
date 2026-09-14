use crate::agent::context::types::{SandwichContext, SideEffectRecord};

/// 不可逆操作永久台账（安全审计区，无论历史如何压缩，台账永不丢弃）
pub struct AuditLedger;

impl AuditLedger {
    /// 格式化不可逆操作台账（如超过 8 条则进行同类动作聚合折叠，保持体积紧凑）
    pub fn render_ledger(records: &[SideEffectRecord]) -> String {
        if records.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let total = records.len();

        if total <= 8 {
            for r in records {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        } else {
            // 早期记录聚合
            let omitted = total - 6;
            lines.push(format!(
                "- [... 早期已安全执行 {} 项不可逆动作 (完整记录在 SQLite) ...]",
                omitted
            ));
            for r in &records[omitted..] {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        }

        format!(
            "【已生效的不可逆外部操作 (Side-Effects 台账)】\n(⚠️系统提示：以下动作已被物理执行，请勿重复调用！)\n{}\n\n",
            lines.join("\n")
        )
    }

    /// 兼容性入口：根据 tool 与 params 判断是否产生外部物理副作用
    pub fn is_mutating_side_effect(tool: &str, params: &serde_json::Value) -> bool {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        Self::is_mutating_side_effect_with_action(tool, action, params)
    }

    /// 核心判断：结合工具名、动作名与参数细分判定是否属于可能产生外部物理副作用的操作
    pub fn is_mutating_side_effect_with_action(
        tool: &str,
        action: &str,
        params: &serde_json::Value,
    ) -> bool {
        let lower_tool = tool.to_lowercase();
        let action_name = lower_tool.split('/').last().unwrap_or(&lower_tool);
        let act = if !action.is_empty() {
            action.to_lowercase()
        } else {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase()
        };

        // 0. 原生 local_file 工具专用精准判定：
        //    - read / list 为纯只读，绝不记录进 Side-Effects 台账；
        //    - 写入到 session_tmp（如 path 为 tmp/ 或 ./tmp/，或显式 is_tmp）为草稿免审，不进台账；
        //    - write / edit 写入项目工作区代码时，必须严格入账！
        if action_name == "local_file" {
            if act.starts_with("read") || act.starts_with("list") {
                return false;
            }
            if act.starts_with("write") || act.starts_with("edit") {
                if Self::is_temporary_draft_path(params) {
                    return false;
                }
                return true;
            }
            return false;
        }

        // 1. 显式只读工具白名单（绝不记录进 Side-Effects 台账）
        let is_readonly_tool = matches!(
            action_name,
            "read_file"
                | "read_text_file"
                | "read_media_file"
                | "read_multiple_files"
                | "list_directory"
                | "list_directory_with_sizes"
                | "directory_tree"
                | "get_file_info"
                | "list_allowed_directories"
                | "vscode_getdiagnostics"
                | "vscode_getsymbollspinfo"
                | "vscode_getreferences"
                | "vscode_listworkspaces"
                | "vscode_health"
                | "ripgrep_search"
                | "ripgrep_list_files"
                | "ripgrep_search_files"
                | "search_graph"
                | "query_graph"
                | "trace_path"
                | "get_code_snippet"
                | "get_graph_schema"
                | "get_architecture"
                | "search_code"
                | "list_projects"
                | "index_status"
                | "check_index_coverage"
                | "detect_changes"
                | "list_workspaces"
                | "git_diff"
                | "health_check"
                | "web_search"
                | "web_fetch"
                | "get_step_output"
                | "search_step_output"
                | "get_output"
                | "list_resources"
                | "get_resource"
                | "list_records"
                | "get_record"
                | "model_manager"
                | "ask_human"
                | "finish"
        );
        if is_readonly_tool {
            return false;
        }

        // 2. shell 命令：细分检测是否为只读命令
        if action_name == "local_shell" || action_name == "shell_exec" || action_name == "shell_exec_async" {
            return !Self::is_readonly_shell_command(params);
        }

        // 3. 常见写操作与变更类动作
        action_name.contains("write")
            || action_name.contains("edit")
            || action_name.contains("move")
            || action_name.contains("delete")
            || action_name.contains("create")
            || action_name.contains("upsert")
            || action_name.contains("patch")
            || action_name.contains("apply")
            || action_name.contains("remove")
            || act.starts_with("write")
            || act.starts_with("edit")
            || act.starts_with("delete")
            || act.starts_with("save")
    }

    /// 判定参数中的目标文件路径是否属于会话临时草稿目录 (session_tmp / tmp)
    pub fn is_temporary_draft_path(params: &serde_json::Value) -> bool {
        if params.get("is_tmp").and_then(|v| v.as_bool()).unwrap_or(false) {
            return true;
        }
        let raw_path = params
            .get("path")
            .or_else(|| params.get("target_file"))
            .or_else(|| params.get("filepath"))
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            return false;
        }
        crate::agent::path_guard::is_path_in_session_tmp(raw_path, None)
    }

    /// 检测 shell 命令是否为只读查询（如 git log/show/diff, ls, cat, grep 等）
    pub fn is_readonly_shell_command(params: &serde_json::Value) -> bool {
        let raw_cmd = params
            .get("command")
            .or_else(|| params.get("cmd"))
            .or_else(|| params.get("commandline"))
            .or_else(|| params.get("command_line"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();

        let explicit_args: Vec<&str> = params
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut tokens: Vec<&str> = raw_cmd.split_whitespace().collect();
        if !explicit_args.is_empty() {
            tokens.extend(explicit_args);
        }

        if tokens.is_empty() {
            return false;
        }

        // 前置包装器剥壳：sudo/timeout/nice 等包装命令的真实命令在内层——
        // 不剥壳会让 `sudo pacman -Q` 这类只读探针被误记为副作用
        // （副作用入账会清空 change_verification，形成"探测即重置验证态"的死锁）。
        // 保守策略：只剥能静态确定参数边界的形态，剥不动就按非只读处理。
        let mut tok_idx = 0usize;
        while tok_idx < tokens.len() {
            match tokens[tok_idx] {
                // 仅剥 "sudo cmd" 直跟形态；sudo -u foo cmd 等带参旗标不剥
                "sudo" | "doas" | "pkexec" => {
                    if tokens
                        .get(tok_idx + 1)
                        .map_or(false, |t| !t.starts_with('-'))
                    {
                        tok_idx += 1;
                    } else {
                        break;
                    }
                }
                "timeout" | "nice" | "nohup" | "stdbuf" | "time" => {
                    tok_idx += 1;
                    // 跳过包装器自身旗标（-k/-s/-n/-o 等）
                    while tokens.get(tok_idx).map_or(false, |t| t.starts_with('-')) {
                        tok_idx += 1;
                    }
                    // timeout/nice 的数值操作数（时长/优先级，如 "5"、"0.5"）
                    while tokens.get(tok_idx).map_or(false, |t| {
                        t.chars().next().map_or(false, |c| c.is_ascii_digit())
                    }) {
                        tok_idx += 1;
                    }
                }
                _ => break,
            }
        }
        let tokens = &tokens[tok_idx..];
        if tokens.is_empty() {
            return false;
        }

        let first = tokens[0];
        let base_cmd = first.split('/').last().unwrap_or(first).trim();

        // 重定向即写入：带 `>` 输出重定向的命令不再算只读（如 `echo x >> file`、`git diff > patch`）。
        // 引号内的 `>` 字面量不触发——先抹掉引号内内容再做结构检测。
        if Self::blank_quoted_segments(&tokens.join(" ")).contains('>') {
            return false;
        }

        if base_cmd == "git" {
            let mut skip_next = false;
            for arg in &tokens[1..] {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if *arg == "-C" || *arg == "--git-dir" || *arg == "--work-tree" {
                    skip_next = true;
                    continue;
                }
                if arg.starts_with('-') {
                    continue;
                }
                // 第一个非 flag 参数即为 git 子命令
                return matches!(
                    *arg,
                    "log"
                        | "show"
                        | "status"
                        | "diff"
                        | "branch"
                        | "tag"
                        | "remote"
                        | "config"
                        | "rev-parse"
                        | "describe"
                        | "check-ignore"
                );
            }
            return false;
        }

        if base_cmd == "sed" {
            return Self::is_sed_readonly(&tokens);
        }

        // 多态命令按子命令/旗标细分：查询态只读（可作状态断言证据），变更态记副作用。
        // 判别失败一律落到"非只读"（宁可误记副作用，不可漏记真实写入）。
        match base_cmd {
            "pacman" => return Self::pacman_is_readonly(&tokens),
            "dpkg" | "dpkg-query" => return Self::dpkg_is_readonly(&tokens),
            "rpm" => {
                return tokens[1..].iter().any(|a| {
                    a.strip_prefix("--").map_or(false, |r| r.starts_with("query"))
                        || a.strip_prefix('-').map_or(false, |r| r.starts_with('q'))
                });
            }
            "apt" | "apt-get" => return Self::apt_is_readonly(&tokens),
            "apt-cache" => return true,
            "brew" => {
                return Self::first_operand_in(
                    &tokens,
                    &["list", "info", "search", "cat", "home", "leaves", "deps", "uses", "outdated"],
                );
            }
            "snap" => {
                return Self::first_operand_in(&tokens, &["list", "info", "find", "changes", "tasks"]);
            }
            "flatpak" => {
                return Self::first_operand_in(
                    &tokens,
                    &["list", "info", "search", "remote-ls", "history"],
                );
            }
            "systemctl" => {
                return Self::first_operand_in(
                    &tokens,
                    &[
                        "status", "is-active", "is-enabled", "is-failed", "is-system-running",
                        "is-online", "show", "cat", "help", "list-units", "list-unit-files",
                        "list-timers", "list-sockets", "list-dependencies", "get-default",
                        "show-environment", "log-level", "log-target",
                    ],
                );
            }
            "service" => return tokens.get(2).map_or(false, |a| *a == "status"),
            "docker" | "podman" => {
                return Self::first_operand_in(
                    &tokens,
                    &[
                        "ps", "inspect", "images", "logs", "info", "version", "top", "port",
                        "diff", "stats", "search", "history",
                    ],
                );
            }
            "kubectl" => {
                return Self::first_operand_in(
                    &tokens,
                    &[
                        "get", "describe", "logs", "top", "version", "cluster-info",
                        "api-resources", "api-versions", "explain", "diff",
                    ],
                );
            }
            "hostnamectl" | "timedatectl" | "localectl" | "loginctl" | "resolvectl"
            | "networkctl" | "bootctl" | "coredumpctl" => {
                return tokens[1..]
                    .iter()
                    .find(|a| !a.starts_with('-'))
                    .map_or(true, |op| {
                        matches!(*op, "status" | "show" | "list" | "query" | "statistics" | "dump")
                    });
            }
            // journalctl 无子命令（参数全是旗标/过滤表达式），变更靠特定长旗标
            "journalctl" => {
                return !tokens[1..].iter().any(|a| {
                    a.starts_with("--vacuum")
                        || matches!(
                            *a,
                            "--rotate" | "--flush" | "--sync" | "--relinquish-var"
                                | "--smart-relinquish-var" | "--update-catalog"
                        )
                });
            }
            "ip" => return Self::ip_is_readonly(&tokens),
            "ifconfig" | "iwconfig" => {
                return !tokens[1..].iter().any(|a| !a.starts_with('-'));
            }
            "nc" | "ncat" | "netcat" => {
                return tokens[1..].iter().any(|a| *a == "-z" || *a == "--zero");
            }
            "curl" => return Self::curl_is_readonly(&tokens),
            "wget" => return Self::wget_is_readonly(&tokens),
            "mount" => return !tokens[1..].iter().any(|a| !a.starts_with('-')),
            "command" => return matches!(tokens.get(1), Some(&"-v") | Some(&"-V")),
            "env" => {
                return tokens[1..]
                    .iter()
                    .all(|a| a.starts_with('-') || a.contains('='));
            }
            "tar" => return Self::tar_is_readonly(&tokens),
            "unzip" => return tokens[1..].iter().any(|a| matches!(*a, "-l" | "-t" | "-Z")),
            _ => {}
        }

        matches!(
            base_cmd,
            "ls" | "cat"
                | "grep"
                | "rg"
                | "ripgrep"
                | "find"
                | "head"
                | "tail"
                | "wc"
                | "echo"
                | "pwd"
                | "which"
                | "whereis"
                // 存在性/状态断言探针：退出码即判据（卸载、禁用、删除任务的天然验证手段）
                | "test"
                | "["
                | "[["
                | "stat"
                | "pgrep"
                | "pidof"
                | "ps"
                | "type"
                | "ping"
                | "ping6"
                | "traceroute"
                | "tracepath"
                // 校验/比较/编码类纯读取
                | "file"
                | "diff"
                | "cmp"
                | "comm"
                | "md5sum"
                | "sha1sum"
                | "sha256sum"
                | "sha512sum"
                | "cksum"
                | "b2sum"
                | "readlink"
                | "realpath"
                | "basename"
                | "dirname"
                // 系统状态只读观测
                | "ss"
                | "netstat"
                | "lsof"
                | "df"
                | "du"
                | "free"
                | "hostname"
                | "uptime"
                | "id"
                | "whoami"
                | "date"
                | "uname"
                | "printenv"
                | "nproc"
                | "arch"
                | "lsblk"
                | "lspci"
                | "lsusb"
                | "lscpu"
                | "sw_vers"
                | "dig"
                | "host"
                | "nslookup"
                // 文本过滤器与杂项只读（重定向 > 已在上方排除）
                | "jq"
                | "sort"
                | "uniq"
                | "tr"
                | "cut"
                | "paste"
                | "join"
                | "column"
                | "fold"
                | "fmt"
                | "tree"
                | "zipinfo"
                | "zcat"
                | "zless"
                | "zgrep"
                | "bzcat"
                | "xzcat"
                | "sleep"
                | "true"
                | "false"
        )
    }

    /// 第一个非旗标操作数是否落在只读子命令集合内（子命令位判定）。
    fn first_operand_in(tokens: &[&str], readonly_ops: &[&str]) -> bool {
        tokens[1..]
            .iter()
            .find(|a| !a.starts_with('-'))
            .map_or(false, |op| readonly_ops.contains(op))
    }

    /// pacman 只读判定：操作位（首个旗标首字符或 --xxx 长旗标）为 -Q/--query/-T/--deptest 才算查询态。
    /// pacman 的操作是合并短旗标的首字符：-Qs 查询、-Rns 删除、-Syu 安装。
    fn pacman_is_readonly(tokens: &[&str]) -> bool {
        for arg in &tokens[1..] {
            if let Some(rest) = arg.strip_prefix("--") {
                return rest.starts_with("query") || rest.starts_with("deptest");
            }
            if let Some(rest) = arg.strip_prefix('-') {
                if !rest.is_empty() {
                    return rest.starts_with('Q') || rest.starts_with('T');
                }
                continue;
            }
            // 裸操作数出现在操作位之前属异常形态 → 保守按非只读
            return false;
        }
        false
    }

    /// dpkg 只读判定：-l/-s/-L/-p/-V 及对应长旗标为查询/审计态。
    fn dpkg_is_readonly(tokens: &[&str]) -> bool {
        for arg in &tokens[1..] {
            if let Some(rest) = arg.strip_prefix("--") {
                let name = rest.split('=').next().unwrap_or("");
                return matches!(
                    name,
                    "list" | "status" | "listfiles" | "print-avail" | "print-architecture"
                        | "print-foreign-architectures" | "query" | "audit" | "compare-versions"
                        | "verify" | "get-selections" | "yet-to-unpack"
                );
            }
            if let Some(rest) = arg.strip_prefix('-') {
                if !rest.is_empty() {
                    return matches!(
                        rest.chars().next().unwrap(),
                        'l' | 's' | 'L' | 'p' | 'V' | 'C'
                    );
                }
                continue;
            }
            return false;
        }
        false
    }

    /// apt/apt-get 只读判定：查询类子命令只读；-s/--simulate/--dry-run 模拟运行不算变更。
    fn apt_is_readonly(tokens: &[&str]) -> bool {
        if tokens[1..]
            .iter()
            .any(|a| matches!(*a, "-s" | "--simulate" | "--dry-run" | "--just-print"))
        {
            return true;
        }
        Self::first_operand_in(
            tokens,
            &[
                "list", "show", "search", "policy", "depends", "rdepends", "madison",
                "dumpavail", "showpkg", "showsrc", "stats", "check", "changelog",
            ],
        )
    }

    /// ip 只读判定：show/list/查询族只读；出现 set/add/del/change/replace/flush 即变更。
    fn ip_is_readonly(tokens: &[&str]) -> bool {
        let ops: Vec<&&str> = tokens[1..].iter().filter(|a| !a.starts_with('-')).collect();
        if ops.iter().any(|a| {
            matches!(
                **a,
                "set" | "add" | "del" | "delete" | "replace" | "flush" | "change" | "save"
                    | "restore"
            )
        }) {
            return false;
        }
        ops.first().map_or(false, |op| {
            matches!(
                **op,
                "a" | "addr" | "address" | "l" | "link" | "r" | "route" | "neigh"
                    | "neighbour" | "rule" | "maddr" | "mroute" | "show" | "list" | "help"
                    | "stats" | "monitor"
            )
        })
    }

    /// curl 只读判定：GET/HEAD/OPTIONS 及 /dev/null 输出断言只读；
    /// -d/--data/-F/--form/-T/--upload-file（上传）、-X 写动词、-o/-O 落盘 → 非只读。
    fn curl_is_readonly(tokens: &[&str]) -> bool {
        let mut i = 1;
        while i < tokens.len() {
            let a = tokens[i];
            match a {
                "-X" | "--request" => {
                    let verb = tokens
                        .get(i + 1)
                        .map(|s| s.to_uppercase())
                        .unwrap_or_default();
                    if !matches!(verb.as_str(), "GET" | "HEAD" | "OPTIONS") {
                        return false;
                    }
                    i += 1;
                }
                "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii"
                | "--data-urlencode" | "-T" | "--upload-file" | "-F" | "--form"
                | "--json" => return false,
                "-o" | "--output" => {
                    // 输出落盘即写文件；写到 /dev/null（HTTP 断言惯用法）仍只读
                    match tokens.get(i + 1) {
                        Some(&t) if t == "/dev/null" => i += 1,
                        _ => return false,
                    }
                }
                "-O" | "--remote-name" | "--remote-name-all" | "-J" | "--remote-header-name" => {
                    return false;
                }
                _ => {
                    let la = a.to_lowercase();
                    if la.starts_with("--data")
                        || la.starts_with("--form")
                        || la.starts_with("--upload")
                        || la.starts_with("--json")
                    {
                        return false;
                    }
                    if let Some(rest) = a.strip_prefix("--output=") {
                        if rest != "/dev/null" {
                            return false;
                        }
                    } else if a.starts_with("-o") && a.len() > 2 {
                        if &a[2..] != "/dev/null" {
                            return false;
                        }
                    } else if (a.starts_with("-d") || a.starts_with("-T") || a.starts_with("-F"))
                        && a.len() > 2
                    {
                        return false;
                    } else if let Some(rest) = a.strip_prefix("-X") {
                        if !rest.is_empty()
                            && !matches!(rest.to_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
                        {
                            return false;
                        }
                    }
                }
            }
            i += 1;
        }
        true
    }

    /// wget 只读判定：--spider/默认 GET 只读；--post-data/--method 写动词、-O 落盘 → 非只读。
    fn wget_is_readonly(tokens: &[&str]) -> bool {
        let mut i = 1;
        while i < tokens.len() {
            let a = tokens[i];
            match a {
                "--post-data" | "--post-file" | "--body-data" | "--body-file" => return false,
                "--method" => {
                    let verb = tokens
                        .get(i + 1)
                        .map(|s| s.to_uppercase())
                        .unwrap_or_default();
                    if !matches!(verb.as_str(), "GET" | "HEAD") {
                        return false;
                    }
                    i += 1;
                }
                "-O" | "--output-document" => {
                    match tokens.get(i + 1) {
                        Some(&t) if t == "/dev/null" => i += 1,
                        _ => return false,
                    }
                }
                _ => {
                    if a.starts_with("--post") || a.starts_with("--body") {
                        return false;
                    }
                    if let Some(rest) = a.strip_prefix("--output-document=") {
                        if rest != "/dev/null" {
                            return false;
                        }
                    }
                }
            }
            i += 1;
        }
        true
    }

    /// tar 只读判定：操作簇含 t（list）且不含 c/x/r/u/A（创建/解包/追加）即只读。
    /// 兼容无横线旧式形态（tar tf x.tar）与 -tf/--list。
    fn tar_is_readonly(tokens: &[&str]) -> bool {
        for arg in &tokens[1..] {
            if let Some(rest) = arg.strip_prefix("--") {
                let name = rest.split('=').next().unwrap_or("");
                return name.starts_with("list") || name.starts_with("test-label");
            }
            let letters = arg.strip_prefix('-').unwrap_or(arg);
            if letters.is_empty() {
                continue;
            }
            return letters.contains('t')
                && !letters
                    .chars()
                    .any(|c| matches!(c, 'c' | 'x' | 'r' | 'u' | 'A'));
        }
        false
    }

    /// 抹掉引号内的内容（保留字符位置结构），用于重定向/管道等结构性检测——
    /// 避免 `grep "a > b" file` 这类引号内字面量被误判为输出重定向。
    pub(crate) fn blank_quoted_segments(cmd: &str) -> String {
        let mut out = String::with_capacity(cmd.len());
        let mut quote: Option<char> = None;
        for c in cmd.chars() {
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                    out.push(' ');
                }
                None => {
                    if c == '\'' || c == '"' {
                        quote = Some(c);
                        out.push(' ');
                    } else {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// sed 只读判定：排查场景 `sed -n '1,5p' file` 属纯读取，但 sed 同时具备
    /// 原地改写能力，必须逐项排除后才可算只读（宁可误记副作用，不可漏记真实写入）：
    /// - 严禁 `-i` / `--in-place`（原地改写文件），含合并短旗标（如 `-in`、`-ni`）；
    /// - 严禁 `-f` / `--file`（脚本来自文件，内容不可静态审计）；
    /// - 严禁脚本文本中出现 `w` / `e` 文件写入或命令执行指令（如 `2w /tmp/x`）。
    pub(crate) fn is_sed_readonly(tokens: &[&str]) -> bool {
        for arg in &tokens[1..] {
            let a = *arg;
            if a == "-i" || a == "--in-place" || a == "-f" || a == "--file" {
                return false;
            }
            if a.starts_with("--") && (a.starts_with("--in-place") || a.starts_with("--file")) {
                return false;
            }
            // 合并短旗标（如 -in、-ni、-Ei）中的 'i' 同样代表 in-place
            if a.starts_with('-') && !a.starts_with("--") && a[1..].contains('i') {
                return false;
            }
            // 非旗标参数即脚本文本，检测 w/e 文件指令
            if !a.starts_with('-') && Self::sed_script_has_write_directive(a) {
                return false;
            }
        }
        true
    }

    /// 检测 sed 脚本文本中的写入/执行指令。保守子串启发：
    /// `w`（写入文件）或 `e`（执行外部命令）出现在行地址上下文（行首、数字/`$`/`;`/`}`/`,`/`/` 之后）
    /// 即判非只读。误判方向是安全的——最多把一条纯读取 sed 记为副作用。
    fn sed_script_has_write_directive(script: &str) -> bool {
        let lower = script.to_lowercase();
        let bytes = lower.as_bytes();
        for (idx, &b) in bytes.iter().enumerate() {
            if b != b'w' && b != b'e' {
                continue;
            }
            let prev_ok = idx == 0
                || matches!(
                    bytes[idx - 1],
                    b' ' | b'\t' | b';' | b'}' | b'$' | b',' | b'/' | b'\n' | b'0'..=b'9'
                );
            if !prev_ok {
                continue;
            }
            // 指令后应跟空白 + 文件名/命令（w file / e cmd）；行尾裸 w/e 也保守视为命中
            let rest = &lower[idx + 1..];
            let next_ok = rest.is_empty()
                || rest.starts_with(|c: char| c.is_ascii_whitespace())
                || rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '/');
            if next_ok {
                return true;
            }
        }
        false
    }

    /// 提取不可逆副作用的干净结构化参数元数据（保留合法完整的 JSON，绝不用 ... 截断破坏元数据）
    pub fn format_side_effect_params(params: &serde_json::Value) -> String {
        if let serde_json::Value::Object(map) = params {
            let mut cleaned = map.clone();
            // 如果包含庞大的正文内容（如 write_file 的整个文件 content），精简 content 字段避免打爆台账，但保留 path 等核心参数
            if let Some(content) = cleaned.get("content") {
                if content.as_str().map(|s| s.len() > 120).unwrap_or(false) {
                    cleaned.insert(
                        "content".to_string(),
                        serde_json::json!("[...已写入内容已持久化...]"),
                    );
                }
            }
            serde_json::to_string(&cleaned).unwrap_or_else(|_| params.to_string())
        } else {
            params.to_string()
        }
    }
}

impl SandwichContext {
    pub fn render_side_effects_ledger(&self) -> String {
        AuditLedger::render_ledger(&self.side_effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(formatted.contains("[...已写入内容已持久化...]"));
        // 必须能够被重新解析为合法 JSON
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();
        assert_eq!(parsed["path"], "/home/user/project/src/main.rs");
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
}
