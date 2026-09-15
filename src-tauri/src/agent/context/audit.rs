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

        // 3. MCP 命名空间工具（plugin/tool 形态）：写动词词表不可枚举——
        //    format_range/set_formula/rename_sheet/merge_cells/insert_rows 这类
        //    不含经典写动词的变更操作不能因词表漏收而对台账隐形。
        //    只读动词前缀放行，其余一律记副作用（fail-closed）。
        //    例外：叶子名带强壳标记（shell/bash/terminal/powershell/cmd/command）
        //    时参数语言就是 shell——走命令级分析，与 dispatcher 判定保持一致
        //    （x/run_command 跑 `ls` 不该一侧放行一侧入账）。
        if lower_tool.contains('/') {
            let orig_name = tool.split('/').last().unwrap_or(tool);
            let leaf_lower = orig_name.to_lowercase();
            let strong_shell_marker = leaf_lower.contains("shell")
                || leaf_lower.contains("bash")
                || leaf_lower.contains("terminal")
                || leaf_lower.contains("powershell")
                || leaf_lower.contains("cmd")
                || leaf_lower.contains("command");
            if strong_shell_marker {
                return !Self::is_readonly_shell_command(params);
            }
            return !Self::mcp_tool_is_readonly_verb(orig_name);
        }

        // 4. 常见写操作与变更类动作
        action_name.contains("write")
            || action_name.contains("edit")
            || action_name.contains("move")
            || action_name.contains("rename")
            || action_name.contains("delete")
            || action_name.contains("create")
            || action_name.contains("upsert")
            || action_name.contains("patch")
            || action_name.contains("apply")
            || action_name.contains("remove")
            || action_name.contains("insert")
            || action_name.contains("update")
            || action_name.contains("format")
            || action_name.contains("merge")
            || action_name.contains("append")
            || action_name.contains("install")
            || action_name.contains("set_")
            || action_name.contains("add_")
            || action_name.contains("drop")
            || action_name.contains("modify")
            || action_name.contains("fill")
            || action_name.contains("clear")
            || action_name.contains("resize")
            || action_name.contains("upload")
            || action_name.contains("download")
            || action_name.contains("import")
            || action_name.contains("export")
            || action_name.contains("exec")
            || action_name.contains("deploy")
            || act.starts_with("write")
            || act.starts_with("edit")
            || act.starts_with("delete")
            || act.starts_with("save")
            || act.starts_with("rename")
            || act.starts_with("update")
            || act.starts_with("insert")
            || act.starts_with("move")
    }

    /// MCP 工具名的只读动词判定：snake_case 前缀、连字符前缀或 camelCase
    /// 词首大写边界（readFile/getSheetData）都算；`getaway_x` 这类以读动词
    /// 起头但非独立词的形态不误判（小写续写不算词边界）。
    /// 入参传原始工具名（未小写化）——camelCase 边界依赖原始大小写。
    pub(crate) fn mcp_tool_is_readonly_verb(action_name: &str) -> bool {
        let lower = action_name.to_lowercase();
        const READ_VERBS: &[&str] = &[
            "read", "list", "ls", "get", "describe", "show", "inspect", "query",
            "search", "find", "check", "status", "info", "count", "exists",
            "validate", "ping", "health", "version", "help", "view", "peek",
            "fetch", "probe", "analyze", "summarize", "preview", "dump", "cat",
            "diff", "watch", "monitor", "trace", "resolve", "lookup", "detect",
            "estimate", "explain", "report",
        ];
        READ_VERBS.iter().any(|v| {
            lower == *v
                || lower.starts_with(&format!("{v}_"))
                || lower.starts_with(&format!("{v}-"))
                || action_name
                    .strip_prefix(v)
                    .map_or(false, |rest| {
                        rest.chars().next().map_or(false, |c| c.is_uppercase())
                    })
        })
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
        let mut explicit_args: Vec<&str> = params
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let owned_args_cmd;
        let raw_cmd = {
            let c = params
                .get("command")
                .or_else(|| params.get("cmd"))
                .or_else(|| params.get("commandline"))
                .or_else(|| params.get("command_line"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim();
            if c.is_empty() && !explicit_args.is_empty() {
                // args-only 形态（{"args":["ls","-la"]}）：args 本体即命令行——
                // dispatcher 同款提取，两侧分类保持一致
                owned_args_cmd = explicit_args.join(" ");
                explicit_args.clear();
                owned_args_cmd.as_str()
            } else {
                c
            }
        };

        // 命令替换/进程替换守卫：$(cmd)、`cmd`、<(cmd)、>(cmd) 内层不可静态审计，
        // 必须扫原文（双引号内的 $() 同样会执行，不能用抹除引号后的文本判断）。
        if raw_cmd.contains("$(")
            || raw_cmd.contains('`')
            || raw_cmd.contains("<(")
            || raw_cmd.contains(">(")
        {
            return false;
        }

        // 复合命令拆解：&&/||/;/|/&/换行 的每个子命令段都必须各自只读，
        // 否则 `which x && rm y`（首段只读）会把整串误判为只读、漏记副作用。
        // 直接在原文上扫描并跟踪引号状态——不能用 blank_quoted_segments 定位，
        // 多字节字符被抹成单空格会导致字节偏移错位。
        let bytes = raw_cmd.as_bytes();
        let mut quote: Option<u8> = None;
        let mut segments: Vec<&str> = Vec::new();
        let mut last = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            if b == b'\'' || b == b'"' {
                quote = Some(b);
                i += 1;
                continue;
            }
            let mut sep_len = 0usize;
            match b {
                b';' | b'\n' => sep_len = 1,
                b'|' => {
                    // `||` 与 `|&`（stderr 管道）各占两字符
                    if matches!(bytes.get(i + 1), Some(b'|') | Some(b'&')) {
                        sep_len = 2;
                    } else {
                        sep_len = 1;
                    }
                }
                b'&' => {
                    // `>&` 与 `|&` 中的 & 属重定向/管道符号；`&>`/`&>>` 的 &
                    // 与重定向同属一段（后续 > 检测会处理），均不作命令分隔
                    let prev = i > 0 && matches!(bytes[i - 1], b'>' | b'|');
                    let next_gt = bytes.get(i + 1) == Some(&b'>');
                    if !prev && !next_gt {
                        sep_len = if bytes.get(i + 1) == Some(&b'&') { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            if sep_len > 0 {
                segments.push(raw_cmd[last..i].trim());
                i += sep_len;
                last = i;
            } else {
                i += 1;
            }
        }
        segments.push(raw_cmd[last..].trim());
        segments.retain(|s| !s.is_empty());

        // `[[ a || b ]]` 内的 ||/&& 是测试表达式而非命令分隔：把未闭合 [[ 的
        // 片段与后续片段重新合并，否则该常用只读探针会被误判为写入、清空验证态。
        let mut merged: Vec<String> = Vec::new();
        for seg in segments {
            if let Some(prev) = merged.last_mut() {
                let opens = prev.matches("[[").count();
                let closes = prev.matches("]]").count();
                if opens > closes {
                    prev.push_str(" || ");
                    prev.push_str(seg);
                    continue;
                }
            }
            merged.push(seg.to_string());
        }
        let segments: Vec<String> = merged;

        if segments.is_empty() {
            return false;
        }
        // 命令串含分隔符时 args 归属不明（属于哪一段不确定），保守按非只读
        if segments.len() > 1 && !explicit_args.is_empty() {
            return false;
        }

        segments.iter().all(|seg| {
            let mut tokens: Vec<&str> = seg.split_whitespace().collect();
            if explicit_args.is_empty() {
                Self::is_readonly_command_segment(&tokens)
            } else {
                // 单段命令 + 显式 args：args 接在命令后
                tokens.extend(explicit_args.iter().copied());
                Self::is_readonly_command_segment(&tokens)
            }
        })
    }

    /// 单段命令（无 &&/||/;/| 分隔）的只读判定：包装器剥壳 → 重定向 → 命令名分派。
    fn is_readonly_command_segment(tokens: &[&str]) -> bool {
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
                // command/builtin 只剥直跟形态：`command -v x` 的 -v 是查询
                // 模式（x 是查询对象而非被执行命令），剥掉会把对象当命令误评
                "command" | "builtin" => {
                    if tokens
                        .get(tok_idx + 1)
                        .map_or(false, |t| !t.starts_with('-'))
                    {
                        tok_idx += 1;
                    } else {
                        break;
                    }
                }
                // sudo/doas/pkexec 剥壳：跳过旗标参数，吃值旗标（-u/-g/-C/-T 等）
                // 连值一起跳过——`sudo -n cat f`、`sudo -u root ls /` 剥到真实命令
                "sudo" | "doas" | "pkexec" => {
                    tok_idx += 1;
                    while let Some(&t) = tokens.get(tok_idx) {
                        if !t.starts_with('-') {
                            break;
                        }
                        tok_idx += 1;
                        if matches!(
                            t,
                            "-u" | "-g" | "-h" | "-p" | "-C" | "-T" | "-r" | "-t"
                                | "-D" | "-U" | "--user" | "--group" | "--host"
                                | "--prompt" | "--chdir" | "--timeout" | "--role"
                                | "--type" | "--command-timeout"
                        ) {
                            tok_idx += 1;
                        }
                    }
                }
                // ionice/chrt/taskset 的旗标值多为数字/数字区间，数值跳过循环可吞
                "ionice" | "chrt" | "taskset" | "timeout" | "nice" | "nohup" | "stdbuf"
                | "time" => {
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

        // 重定向即写入：`>`/`>>`/`&>` 输出重定向不再算只读（`echo x >> file`）。
        // `2>&1`/`>&-` 是 fd 复制不写文件，不触发。引号内 `>` 字面量不计——
        // 先抹掉引号内内容再做结构检测。
        // 特例：`[[ a > b ]]` 内的 < > 是字符串比较而非重定向（`[`/`test` 无此
        // 语义——shell 在调用内建前就已解析重定向，故仅对 `[[ ]]` 豁免），
        // 检测范围收窄到 `]]` 之后，不丢 `[[ x ]] > f` 这类真写入。
        let mut redirect_subject = Self::blank_quoted_segments(&tokens.join(" "));
        if base_cmd == "[[" {
            if let Some(pos) = redirect_subject.rfind("]]") {
                redirect_subject = redirect_subject[pos..].to_string();
            }
        }
        if Self::has_write_redirect(&redirect_subject) {
            return false;
        }

        // 通用版本/帮助探针：`x --version`/`x -V`/`x help`/`x version` 纯输出
        // 不改状态——置于重定向检测之后（`x --version > f` 仍是写入）与命令分派
        // 之前（覆盖未单列的工具）。
        if !tokens[1..].is_empty()
            && tokens[1..].iter().all(|a| {
                matches!(
                    *a,
                    "--version" | "-V" | "--help" | "-h" | "-?" | "version" | "help"
                )
            })
        {
            return true;
        }

        if base_cmd == "git" {
            return Self::git_is_readonly(&tokens[1..]);
        }

        if base_cmd == "sed" {
            return Self::is_sed_readonly(tokens);
        }

        // 多态命令按子命令/旗标细分：查询态只读（可作状态断言证据），变更态记副作用。
        // 判别失败一律落到"非只读"（宁可误记副作用，不可漏记真实写入）。
        match base_cmd {
            "pacman" => return Self::pacman_is_readonly(tokens),
            "dpkg" | "dpkg-query" => return Self::dpkg_is_readonly(tokens),
            "rpm" => {
                return tokens[1..].iter().any(|a| {
                    a.strip_prefix("--").map_or(false, |r| {
                        r.starts_with("query") || r.starts_with("verify") || r.starts_with("checksig")
                    }) || a.strip_prefix('-').map_or(false, |r| {
                        r.starts_with('q') || r.starts_with('V') || r.starts_with('K')
                    })
                });
            }
            // find 裸名绝不能算只读：-exec/-execdir/-ok/-okdir 执行任意命令，
            // -delete 删除，-fprint/-fprintf/-fls/-cpio 写文件。
            "find" => {
                return !tokens[1..].iter().any(|a| {
                    matches!(
                        *a,
                        "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint"
                            | "-fprintf" | "-fls" | "-cpio"
                    )
                });
            }
            // fd/fdfind 同 find：--exec/--exec-batch/-x/-X 执行任意命令
            "fd" | "fdfind" => {
                return !tokens[1..].iter().any(|a| {
                    matches!(*a, "--exec" | "--exec-batch" | "-x" | "-X")
                });
            }
            // sort -o/--output 把结果落盘
            "sort" => {
                return !tokens[1..].iter().any(|a| {
                    a.starts_with("--output")
                        || (a.starts_with('-')
                            && !a.starts_with("--")
                            && a[1..].contains('o'))
                });
            }
            // tree -o/--output 把树图写入文件
            "tree" => {
                return !tokens[1..]
                    .iter()
                    .any(|a| *a == "-o" || a.starts_with("--output"));
            }
            // xargs 本质是命令执行器：仅接受 xargs 自身无旗标的形态——
            // 即 tokens[1] 必须直接是内层命令（xargs 旗标一律先于命令出现，
            // 如 `-I ls rm` 会把白名单命令名当旗标值，必须整体拒掉）。
            // 内层命令递归过同一白名单；其自身旗标（wc -l）不受影响。
            "xargs" => {
                let inner = &tokens[1..];
                if inner.is_empty() || inner[0].starts_with('-') {
                    return false;
                }
                return Self::is_readonly_command_segment(inner);
            }
            // date -s/--set 或纯数字操作数（MMDDhhmm 格式）设置系统时间
            "date" => {
                return !tokens[1..].iter().any(|a| {
                    *a == "-s"
                        || a.starts_with("--set")
                        || (!a.starts_with('-')
                            && !a.is_empty()
                            && a.chars().all(|c| c.is_ascii_digit()))
                });
            }
            // hostname/网络配置类：带非旗标操作数即变更（`hostname x`、`arp -d h`、
            // `route add`、`ifconfig eth0 up`）
            "hostname" | "ifconfig" | "iwconfig" | "arp" | "route" => {
                return !tokens[1..].iter().any(|a| !a.starts_with('-'));
            }
            // mount：裸跑列出挂载（只读），-a/-A/--all 挂载 fstab 全部条目（变更），
            // 任何非旗标操作数（设备/挂载点）也是挂载动作
            "mount" => {
                return !tokens[1..].iter().any(|a| {
                    !a.starts_with('-') || matches!(*a, "-a" | "-A" | "--all")
                });
            }
            // sysctl -w/--write 或 k=v 操作数写内核参数；-p/--load/--system 应用配置
            "sysctl" => {
                return !tokens[1..].iter().any(|a| {
                    matches!(*a, "-w" | "--write" | "-p" | "--load" | "--system")
                        || (!a.starts_with('-') && a.contains('='))
                });
            }
            // dmesg：-c/-C/--clear/--read-clear 清缓冲、-n/--console-level、-D/-E
            // 开关打印是变更；其余（-T/-w/-l 等）只读
            "dmesg" => {
                return !tokens[1..].iter().any(|a| {
                    if let Some(rest) = a.strip_prefix("--") {
                        return matches!(
                            rest.split('=').next().unwrap_or(""),
                            "clear" | "read-clear" | "console-level" | "console-on"
                                | "console-off"
                        );
                    }
                    a.strip_prefix('-').map_or(false, |rest| {
                        !rest.is_empty()
                            && rest.chars().any(|c| matches!(c, 'c' | 'C' | 'n' | 'D' | 'E'))
                    })
                });
            }
            // fuser -k/--kill 杀进程
            "fuser" => {
                return !tokens[1..]
                    .iter()
                    .any(|a| *a == "-k" || a.starts_with("--kill"));
            }
            // bridge 与 ip 同构：变更操作数在，即非只读
            "bridge" => {
                return !tokens[1..]
                    .iter()
                    .any(|a| !a.starts_with('-') && Self::is_netlink_mutating_operand(a));
            }
            // Python 包管理只读子集（install/uninstall/download 等落为变更）
            "pip" | "pip3" | "pipx" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "show", "check", "freeze", "inspect", "hash", "debug"],
                );
            }
            "npm" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "ls", "list", "ll", "la", "view", "info", "search", "outdated",
                        "audit", "ping", "root", "bin", "prefix", "explain", "why", "doctor",
                    ],
                );
            }
            "yarn" | "pnpm" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "ls", "why", "outdated", "audit", "info", "view", "licenses"],
                );
            }
            "gem" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "list", "info", "search", "dependency", "environment", "env",
                        "outdated", "query", "contents", "specification", "which", "help",
                    ],
                );
            }
            // go env -w 写配置；version/doc 纯读；list/build 可能触缓存故不算
            "go" => {
                return match tokens[1..].iter().find(|a| !a.starts_with('-')) {
                    Some(&"env") => !tokens[1..].iter().any(|a| *a == "-w"),
                    Some(&"version") | Some(&"doc") => true,
                    _ => false,
                };
            }
            "rustup" => {
                return Self::first_operand_in(
                    tokens,
                    &["show", "which", "check", "version", "docs"],
                );
            }
            "yum" | "dnf" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "list", "info", "search", "check-update", "repolist", "history",
                        "deplist", "provides", "whatprovides", "grouplist", "groupinfo",
                        "check", "summary",
                    ],
                );
            }
            "repoquery" => return true,
            "zypper" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "search", "se", "info", "if", "list-updates", "lu", "patch-check",
                        "packages", "pa", "products", "patterns", "services", "repos", "lr",
                        "ps", "verify", "what-provides", "wp", "licenses", "locks", "ll",
                    ],
                );
            }
            "apk" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "info", "list", "search", "policy", "version", "stats", "dot",
                        "verify", "manifest",
                    ],
                );
            }
            // helm：repo/template 等双态子命令不在列——保守按变更
            "helm" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "ls", "status", "show", "get", "history", "search", "env"],
                );
            }
            "minikube" => {
                return Self::first_operand_in(
                    tokens,
                    &["status", "ip", "version", "logs"],
                );
            }
            // virsh 查询子命令只读；start/destroy/define 等变更
            "virsh" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "list", "dominfo", "domstate", "dumpxml", "nodeinfo", "version",
                        "pool-list", "net-list", "vol-list", "snapshot-list", "capabilities",
                        "hostname", "uri", "domid", "domuuid", "domname", "domdisplay",
                        "sysinfo", "freecell", "freepages",
                    ],
                );
            }
            // nmcli：双态子命令靠操作数区分（con up/add/delete 变更）
            "nmcli" => {
                return Self::nmcli_is_readonly(tokens);
            }
            // busctl call/set-property/emit 会触发任意 D-Bus 副作用
            "busctl" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "status", "monitor", "tree", "introspect", "user-status",
                        "get-property"],
                );
            }
            // apt-config 只有读配置的能力
            "apt-config" => return true,
            "apt-file" => {
                return Self::first_operand_in(tokens, &["search", "list", "show", "find"]);
            }
            // ldconfig 裸跑重建链接器缓存（变更）；-p/--print-cache 只读
            "ldconfig" => {
                return tokens[1..]
                    .iter()
                    .any(|a| matches!(*a, "-p" | "--print-cache"));
            }
            // crontab：-l 列举只读；-e/-r/-i 或文件操作数（安装新表）即变更；
            // -u/--user 的取值是旗标值不是文件操作数
            "crontab" => {
                let mut i = 1usize;
                let mut has_list = false;
                let mut mutating = false;
                while i < tokens.len() {
                    let a = tokens[i];
                    if matches!(a, "-u" | "--user") {
                        i += 2;
                        continue;
                    }
                    match a {
                        "-l" | "--list" => has_list = true,
                        "-e" | "-r" | "-i" | "--edit" | "--remove" => mutating = true,
                        _ if !a.starts_with('-') => mutating = true,
                        _ => {}
                    }
                    i += 1;
                }
                return has_list && !mutating;
            }
            // macOS 管理工具：查询子命令只读，变更子命令入账
            "defaults" => {
                return Self::first_operand_in(
                    tokens,
                    &["read", "read-type", "find", "domains", "help"],
                );
            }
            "launchctl" => {
                return Self::first_operand_in(
                    tokens,
                    &[
                        "list", "print", "print-cache", "print-disabled", "blame",
                        "procinfo", "managername", "managerpid", "manageruid", "examine",
                    ],
                );
            }
            "mas" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "search", "info", "outdated", "version", "account", "home"],
                );
            }
            "log" => {
                return Self::first_operand_in(
                    tokens,
                    &["show", "stream", "stats", "predicate", "test", "help"],
                );
            }
            "diskutil" => {
                return Self::first_operand_in(
                    tokens,
                    &["list", "info", "verifyvolume", "verifydisk", "activity"],
                );
            }
            "hdiutil" => {
                return Self::first_operand_in(
                    tokens,
                    &["info", "imageinfo", "pmap", "isencrypted", "verify", "help"],
                );
            }
            // codesign：-s/-f/-r/-o 等签署/改写旗标即变更；-d/-v/--verify 只读
            "codesign" => {
                return tokens[1..].iter().all(|a| {
                    if let Some(rest) = a.strip_prefix("--") {
                        return matches!(
                            rest.split('=').next().unwrap_or(""),
                            "verify" | "display" | "check-verify" | "verbose" | "strict"
                                | "deep" | "all-architectures" | "xml"
                        );
                    }
                    if !a.starts_with('-') {
                        return true; // 目标文件操作数
                    }
                    a[1..].chars().all(|c| matches!(c, 'd' | 'v' | 'D' | 'V'))
                });
            }
            // spctl --master-disable/--add/--remove/--reset 等改评估策略
            "spctl" => {
                return !tokens[1..].iter().any(|a| {
                    a.strip_prefix("--").map_or(false, |r| {
                        r.starts_with("master")
                            || r.starts_with("add")
                            || r.starts_with("remove")
                            || r.starts_with("enable")
                            || r.starts_with("disable")
                            || r.starts_with("reset")
                    })
                });
            }
            // xattr -w 写、-d 删、-c 清扩展属性；-l/-p/-v/-x 读
            "xattr" => {
                return !tokens[1..].iter().any(|a| {
                    a.strip_prefix('-').map_or(false, |rest| {
                        !rest.is_empty()
                            && rest.chars().any(|c| matches!(c, 'w' | 'd' | 'c'))
                    })
                });
            }
            // `local_shell` 显式 args 值可能是旗标的取值（crontab -u user 等）
            "apt" | "apt-get" => return Self::apt_is_readonly(tokens),
            "apt-cache" => return true,
            "brew" => {
                return Self::first_operand_in(
                    &tokens,
                    &["list", "info", "search", "cat", "home", "leaves", "deps", "uses", "outdated"],
                );
            }
            "snap" => {
                return Self::first_operand_in(tokens, &["list", "info", "find", "changes", "tasks"]);
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
            // service x status 只读；--status-all 全旗标列举也只读
            "service" => {
                return tokens.get(2).map_or(false, |a| *a == "status")
                    || tokens[1..].iter().all(|a| a.starts_with('-'));
            }
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
            | "networkctl" | "bootctl" | "coredumpctl" | "machinectl" | "userdbctl"
            | "homectl" | "oomctl" => {
                return Self::ctl_is_readonly(tokens);
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
            "ip" => return Self::ip_is_readonly(tokens),
            "nc" | "ncat" | "netcat" => {
                return tokens[1..].iter().any(|a| *a == "-z" || *a == "--zero");
            }
            "curl" => return Self::curl_is_readonly(tokens),
            "wget" => return Self::wget_is_readonly(tokens),
            "command" => return matches!(tokens.get(1), Some(&"-v") | Some(&"-V")),
            "env" => {
                return tokens[1..]
                    .iter()
                    .all(|a| a.starts_with('-') || a.contains('='));
            }
            "tar" => return Self::tar_is_readonly(tokens),
            // unzip：-l/-t/-Z 列举测试、-p/-c 提取到 stdout（只读）；
            // -o/-f/-n 等落地解包旗标不在列 → 变更
            "unzip" => {
                return tokens[1..]
                    .iter()
                    .any(|a| matches!(*a, "-l" | "-t" | "-Z" | "-p" | "-c"));
            }
            _ => {}
        }

        matches!(
            base_cmd,
            "ls" | "cat"
                | "grep"
                | "rg"
                | "ripgrep"
                | "head"
                | "tail"
                | "wc"
                | "echo"
                | "printf"
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
                | "uptime"
                | "id"
                | "whoami"
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
                | "whois"
                // 硬件/内核/会话只读观测（双态工具在上方分派中带守卫）
                | "blkid"
                | "findmnt"
                | "lsmod"
                | "lsns"
                | "lsipc"
                | "lslocks"
                | "lslogins"
                | "lsmem"
                | "lsscsi"
                | "lsattr"
                | "getfacl"
                | "getent"
                | "lsb_release"
                | "dmidecode"
                | "lshw"
                | "sensors"
                | "acpi"
                | "upower"
                | "pstree"
                | "vmstat"
                | "iostat"
                | "mpstat"
                | "pidstat"
                | "top"
                | "who"
                | "w"
                | "users"
                | "last"
                | "lastlog"
                | "groups"
                | "locale"
                | "man"
                | "whatis"
                | "apropos"
                | "info"
                | "cal"
                // macOS 纯读观测（launchctl/diskutil 等双态工具在上方分派中带守卫）
                | "ioreg"
                | "kextstat"
                | "system_profiler"
                | "mdls"
                | "mdfind"
                // 终端态观测/重置：不写文件不改系统持久态
                | "tty"
                | "stty"
                | "tput"
                | "clear"
                | "reset"
                | "atq"
                | "mountpoint"
                | "fc-list"
                | "fc-match"
                // 文本过滤器与杂项只读（重定向 > 已在上方排除）
                | "jq"
                | "uniq"
                | "tr"
                | "cut"
                | "paste"
                | "join"
                | "column"
                | "fold"
                | "fmt"
                | "pr"
                | "rev"
                | "tac"
                | "nl"
                | "tsort"
                | "iconv"
                | "expand"
                | "unexpand"
                | "numfmt"
                | "seq"
                | "factor"
                | "bc"
                | "dc"
                | "expr"
                | "base64"
                | "basenc"
                | "od"
                | "hexdump"
                | "nm"
                | "objdump"
                | "readelf"
                | "strings"
                | "size"
                | "addr2line"
                | "c++filt"
                | "bat"
                | "batcat"
                | "less"
                | "more"
                | "most"
                | "exa"
                | "eza"
                | "lsd"
                | "dust"
                | "duf"
                | "zipinfo"
                | "zcat"
                | "zless"
                | "zgrep"
                | "bzcat"
                | "xzcat"
                | "sleep"
                | "true"
                | "false"
                // shell 内建：只改当前进程状态（cwd/环境/选项），不产生文件系统副作用
                | "cd"
                | "pushd"
                | "popd"
                | "dirs"
                | "export"
                | "readonly"
                | "declare"
                | "typeset"
                | "local"
                | "umask"
                | "ulimit"
                | "set"
                | "shopt"
                | "jobs"
                | "hash"
                | "history"
                | "alias"
                | "unalias"
        )
    }

    /// 第一个非旗标操作数是否落在只读子命令集合内（子命令位判定）。
    fn first_operand_in(tokens: &[&str], readonly_ops: &[&str]) -> bool {
        tokens[1..]
            .iter()
            .find(|a| !a.starts_with('-'))
            .map_or(false, |op| readonly_ops.contains(op))
    }

    /// git 只读判定：按子命令细分——纯读子命令白名单 + 双态子命令按操作数区分
    /// "列举"与"增删改"（`git branch -d` 删分支、`git config k v` 写配置、
    /// `git stash` 裸跑即 push 都是真实变更，不能因子命令名含列举形态就放走）。
    fn git_is_readonly(args: &[&str]) -> bool {
        // --output=<f> 让 diff/log/show 等把内容落盘——直接判变更
        if args.iter().any(|a| a.starts_with("--output")) {
            return false;
        }
        // 定位子命令：跳过全局选项；吃值的旗标消耗其后一个 token
        const VALUE_FLAGS: &[&str] = &[
            "-C", "-c", "--git-dir", "--work-tree", "--exec-path", "--namespace",
        ];
        let mut i = 0usize;
        while i < args.len() {
            let a = args[i];
            if VALUE_FLAGS.contains(&a) {
                i += 2;
                continue;
            }
            if a.starts_with('-') {
                i += 1;
                continue;
            }
            break;
        }
        let Some(&sub) = args.get(i) else {
            return true; // 裸 `git` 仅打印帮助
        };
        let rest = &args[(i + 1).min(args.len())..];
        let operands: Vec<&&str> = rest.iter().filter(|a| !a.starts_with('-')).collect();
        let has_flag = |names: &[&str]| rest.iter().any(|a| names.contains(a));
        match sub {
            // 纯读子命令
            "log" | "show" | "status" | "diff" | "rev-parse" | "describe" | "check-ignore"
            | "blame" | "annotate" | "ls-files" | "ls-tree" | "ls-remote" | "rev-list"
            | "shortlog" | "reflog" | "name-rev" | "show-branch" | "whatchanged"
            | "count-objects" | "var" | "verify-commit" | "verify-tag" | "cat-file"
            | "show-ref" | "for-each-ref" | "merge-base" | "grep" | "range-diff"
            | "cherry" | "fsck" | "help" | "version" | "stripspace" | "check-attr"
            | "check-mailmap" | "check-ref-format" | "interpret-trailers" | "mktag"
            | "patch-id" | "show-index" | "verify-pack" | "diff-files" | "diff-index"
            | "diff-tree" => true,
            // branch/tag：列举形态只读；创建/删除/改名/设上游（含写旗标）即变更
            "branch" => {
                if has_flag(&[
                    "-d", "-D", "-m", "-M", "-c", "-C", "-u", "--set-upstream-to",
                    "--unset-upstream", "-f", "--force", "--edit-description", "--track",
                ]) {
                    return false;
                }
                // 带 -l/--list/--contains 等列举旗标时，操作数是过滤模式
                if has_flag(&[
                    "-l", "--list", "--show-current", "--contains", "--no-contains",
                    "--merged", "--no-merged", "--points-at",
                ]) {
                    return true;
                }
                operands.is_empty()
            }
            "tag" => {
                if has_flag(&[
                    "-a", "-s", "-u", "-m", "-f", "-d", "-F", "-e", "--delete",
                    "--annotate", "--sign", "--local-user", "--file", "--edit",
                ]) {
                    return false;
                }
                if has_flag(&[
                    "-l", "--list", "--verify", "-v", "--contains", "--no-contains",
                    "--merged", "--no-merged", "--points-at", "--sort", "--format",
                ]) || rest.iter().any(|a| {
                    a.starts_with('-') && !a.starts_with("--") && a[1..].starts_with('n')
                }) {
                    return true;
                }
                operands.is_empty()
            }
            // remote：裸跑/-v/列举型子命令只读；add/remove/set-url/prune/update 变更
            "remote" => {
                if operands.is_empty() {
                    return true;
                }
                matches!(*operands[0], "get-url" | "show" | "ls-remote")
            }
            // config：单操作数 `git config key` 是取值（只读）；≥2 操作数或
            // --add/--unset/-e 等写旗标即变更。--file/--blob/--default/--type
            // 消耗其后的值，不算普通操作数
            "config" => {
                if has_flag(&[
                    "--add", "--set", "--unset", "--unset-all", "--replace-all",
                    "--rename-section", "--remove-section", "-e", "--edit",
                ]) {
                    return false;
                }
                if has_flag(&[
                    "--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list",
                    "-l", "--null", "-z", "--name-only", "--show-origin", "--show-scope",
                    "--includes",
                ]) {
                    return true;
                }
                let mut operands = 0usize;
                let mut j = 0usize;
                while j < rest.len() {
                    let a = rest[j];
                    if matches!(a, "--file" | "--blob" | "--default" | "--type" | "-f") {
                        j += 2;
                        continue;
                    }
                    if !a.starts_with('-') {
                        operands += 1;
                    }
                    j += 1;
                }
                operands <= 1
            }
            // stash：list/show 只读；裸 stash 即 push = 变更；pop/apply/drop 变更
            "stash" => operands.first().map_or(false, |op| matches!(**op, "list" | "show")),
            // submodule：status/summary 或裸跑只读；init/update/sync/foreach 变更
            "submodule" => {
                operands.first().map_or(true, |op| {
                    matches!(**op, "status" | "summary")
                })
            }
            // worktree：list 只读；add/remove/move/lock/prune 变更
            "worktree" => operands.first().map_or(false, |op| **op == "list"),
            // notes：list/show/get-ref 只读
            "notes" => operands.first().map_or(false, |op| {
                matches!(**op, "list" | "show" | "get-ref")
            }),
            // symbolic-ref：单操作数读引用；-d/--delete 或 ≥2 操作数写引用
            "symbolic-ref" => {
                !has_flag(&["-d", "--delete"]) && operands.len() <= 1
            }
            // hash-object：-w/--write 写对象库
            "hash-object" => !has_flag(&["-w", "--write"]),
            _ => false,
        }
    }

    /// *ctl 工具只读判定：裸命令（hostnamectl/timedatectl 无参即打印状态）与
    /// status/show/list-* /show-*/*status 族只读；set-*/lock-*/enable/disable 等
    /// 变更子命令不在列 → 非只读。
    fn ctl_is_readonly(tokens: &[&str]) -> bool {
        let Some(op) = tokens[1..].iter().find(|a| !a.starts_with('-')) else {
            return true;
        };
        matches!(
            *op,
            "status" | "show" | "list" | "query" | "statistics" | "dump" | "help"
                | "hostname" | "info" | "describe"
        ) || op.starts_with("list-")
            || op.starts_with("show-")
            || op.ends_with("-status")
    }

    /// ip/bridge 变更操作数集合判定（route/addr 对象名不在内）
    fn is_netlink_mutating_operand(op: &str) -> bool {
        matches!(
            op,
            "set" | "add" | "del" | "delete" | "replace" | "flush" | "change" | "save"
                | "restore" | "enable" | "disable" | "up" | "down"
        )
    }

    /// nmcli 只读判定：出现变更操作数即非只读；否则首操作数须为只读对象域
    fn nmcli_is_readonly(tokens: &[&str]) -> bool {
        let operands: Vec<&&str> = tokens[1..].iter().filter(|a| !a.starts_with('-')).collect();
        if operands.iter().any(|a| {
            matches!(
                **a,
                "up" | "down" | "add" | "delete" | "del" | "modify" | "edit" | "set"
                    | "connect" | "disconnect" | "load" | "clone" | "import" | "on" | "off"
                    | "enable" | "disable" | "reapply" | "reload" | "create" | "apply"
            )
        }) {
            return false;
        }
        operands.first().map_or(true, |op| {
            matches!(
                **op,
                "g" | "general" | "device" | "dev" | "d" | "connection" | "con" | "c"
                    | "radio" | "r" | "networking" | "n" | "monitor" | "m" | "agent"
                    | "show" | "status" | "help"
            )
        })
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
                        | "verify" | "get-selections" | "yet-to-unpack" | "show" | "search"
                );
            }
            if let Some(rest) = arg.strip_prefix('-') {
                if !rest.is_empty() {
                    return matches!(
                        rest.chars().next().unwrap(),
                        'l' | 's' | 'S' | 'L' | 'p' | 'V' | 'C' | 'W'
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

    /// 写入重定向检测：任一 `>` 且其后不是 `&<数字>`/`&-` 的 fd 复制形式。
    /// `&>`/`&>>` 中的 `>` 同样命中（写盘）；`2>&1`、`>&-` 不命中（fd 操作）；
    /// `\>` 是转义字面量（`test a \> b` 的比较语法）不命中——奇数连续反斜杠
    /// 才转义，偶数（`\\>`）末尾的 `>` 仍是重定向。
    /// 重定向写盘检测（入参须先经 blank_quoted_segments 抹除引号内容）。
    /// dispatcher 与台账共用此判定，保证"只读探针"两侧口径一致。
    pub(crate) fn has_write_redirect(blanked: &str) -> bool {
        let b = blanked.as_bytes();
        for i in 0..b.len() {
            if b[i] == b'>' {
                let mut backslashes = 0usize;
                let mut j = i;
                while j > 0 && b[j - 1] == b'\\' {
                    backslashes += 1;
                    j -= 1;
                }
                if backslashes % 2 == 1 {
                    continue; // 转义的 \> 是字面量
                }
                let fd_dup = b.get(i + 1) == Some(&b'&')
                    && b.get(i + 2)
                        .map_or(false, |c| c.is_ascii_digit() || *c == b'-');
                if !fd_dup {
                    // 目标是哑设备则非真实写入：>/dev/null、2>/dev/null、
                    // >/dev/stdout|stderr|tty|pts/*|fd/* 都不产生持久文件。
                    // 注意 /dev/sda 这类真设备不在豁免列——仍是写入。
                    let mut j = i + 1;
                    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                        j += 1;
                    }
                    const NOOP_TARGETS: &[&str] = &[
                        "/dev/null",
                        "/dev/stdout",
                        "/dev/stderr",
                        "/dev/tty",
                        "/dev/pts/",
                        "/dev/fd/",
                    ];
                    let rest = &blanked[j..];
                    let is_noop = NOOP_TARGETS.iter().any(|t| {
                        if !rest.starts_with(t) {
                            return false;
                        }
                        // 以 / 结尾的是目录前缀（/dev/fd/1、/dev/pts/0），
                        // 后续字符就是名字本体，无需边界检查
                        if t.ends_with('/') {
                            return true;
                        }
                        rest[t.len()..]
                            .chars()
                            .next()
                            .map_or(true, |c| {
                                !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
                            })
                    }) || {
                        // Windows 哑设备：`> nul`/`>NUL` 同 /dev/null
                        rest.len() >= 3
                            && rest[..3].eq_ignore_ascii_case("nul")
                            && rest[3..].chars().next().map_or(true, |c| {
                                !(c.is_ascii_alphanumeric()
                                    || c == '_'
                                    || c == '-'
                                    || c == '.')
                            })
                    };
                    if !is_noop {
                        return true;
                    }
                }
            }
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
}
