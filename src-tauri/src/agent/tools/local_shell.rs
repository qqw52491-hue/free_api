//! local_shell: 原生本地命令行执行工具（受工作区物理边界与超时熔断保护）。
//!
//! 设计目标与安全/自愈兜底机制（参考并汲取 mcp-remote-setup 生产实践）：
//! 1. 彻底解耦外部 MCP 依赖，为主 Agent 提供原生的终端命令执行与测试运行能力；
//! 2. 工具链 PATH 自动补齐：主动探测并补全 ~/.cargo/bin, ~/.local/bin, ~/.local/share/pnpm, /usr/local/bin 等路径，杜绝 GUI/非交互式会话中找不到编译器或包管理器的 ENOENT 错误；
//! 3. 进程组级别隔离与双阶段优雅杀灭：开启 process_group(0)，超时先发 SIGTERM 软杀整个进程组（-pid），1.5秒未退硬发 SIGKILL，彻底防止孤儿进程/子进程树常驻泄漏；
//! 4. 命令与参数智能归一化：无缝兼容单字符串命令 (command: "cargo test") 与结构化命令+参数数组 (command: "cargo", args: ["test", "--release"])，带标准单引号转义；
//! 5. 自定义环境变量支持：支持传入 env: { "KEY": "VAL" } 灵活注入环境变量；
//! 6. 工作区物理边界牢笼：命令默认在当前项目工作区根目录执行，自定义 cwd 必须通过 path_guard 双白名单核验；
//! 7. 详细友好的 ENOENT 诊断：明确区分工作目录不存在与命令未在 PATH 中找到，并输出排查指引；
//! 8. 结构化结果输出与诊断建议：输出 exit_code、duration_ms、kind、retryable、safe_to_retry、recommended_action，辅助大模型自愈纠错。

use crate::agent::context::AuditLedger;
use crate::agent::path_guard::ensure_allowed_path;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50KB 输出上限

/// 常用开发工具链路径自动补齐（避免非交互式桌面进程丢失 pnpm/cargo/node/go 等二进制）
pub fn get_enriched_path_env() -> String {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let mut paths_to_prepend: Vec<PathBuf> = Vec::new();

    if let Some(home) = dirs_next::home_dir() {
        let candidates = [
            home.join(".cargo").join("bin"),
            home.join(".local").join("bin"),
            home.join(".local").join("share").join("pnpm"),
            home.join(".yarn").join("bin"),
        ];
        for c in &candidates {
            if c.exists() {
                paths_to_prepend.push(c.clone());
            }
        }
    }

    #[cfg(unix)]
    {
        for p in &["/usr/local/bin", "/opt/homebrew/bin", "/usr/local/go/bin"] {
            let pb = PathBuf::from(p);
            if pb.exists() {
                paths_to_prepend.push(pb);
            }
        }
    }

    if paths_to_prepend.is_empty() {
        return current_path;
    }

    #[cfg(windows)]
    let separator = ";";
    #[cfg(not(windows))]
    let separator = ":";

    let existing_set: std::collections::HashSet<String> = current_path
        .split(separator)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let mut new_parts = Vec::new();
    for p in paths_to_prepend {
        let s = p.to_string_lossy().to_string();
        if !existing_set.contains(&s) {
            new_parts.push(s);
        }
    }

    if new_parts.is_empty() {
        current_path
    } else {
        format!("{}{}{}", new_parts.join(separator), separator, current_path)
    }
}

/// 标准单引号安全转义（shell 里转义单引号的标准写法 '\''）
pub fn quote_shell_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// 兼容 command + args 数组，拼装成可安全执行的完整命令行字符串
pub fn build_full_command(command: &str, args: &[String]) -> String {
    let trimmed = command.trim();
    if args.is_empty() {
        return trimmed.to_string();
    }
    let quoted_args: Vec<String> = args.iter().map(|a| quote_shell_arg(a)).collect();
    format!("{} {}", trimmed, quoted_args.join(" "))
}

/// 杀灭子进程树 / 进程组（防止孤儿后台进程残留）
fn kill_process_group(pid: u32, signal: &str) {
    #[cfg(unix)]
    {
        // 负 PID (-pid) 表示向以 pid 为组长的整个子进程组发送信号，防止子进程树孤儿泄漏
        let status = std::process::Command::new("kill")
            .arg(format!("-{}", signal))
            .arg(format!("-{}", pid))
            .status();
        // 若负 PID 失败，退化为针对单进程发送信号
        if status.map(|s| !s.success()).unwrap_or(true) {
            let _ = std::process::Command::new("kill")
                .arg(format!("-{}", signal))
                .arg(pid.to_string())
                .status();
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(&["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

pub fn dispatch(
    db: &DbState,
    session_id: &str,
    _action: &str,
    params: &serde_json::Value,
    workspace_root: Option<&Path>,
) -> DispatchResult {
    let ws_root = match workspace_root {
        Some(p) => p,
        None => {
            let payload = json!({
                "kind": "missing_workspace",
                "retryable": false,
                "safe_to_retry": true,
                "recommended_action": "provide_workspace_root",
            });
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ [系统错误] 未提供当前活跃的工作区路径，拒绝执行 shell 命令。\n<!-- 结构化元数据: {} -->", payload),
                success: false,
                route: "local_shell:missing_workspace".to_string(),
            };
        }
    };

    let raw_cmd = match params
        .get("command")
        .or_else(|| params.get("cmd"))
        .or_else(|| params.get("commandline"))
        .and_then(|v| v.as_str())
    {
        Some(c) if !c.trim().is_empty() => c.trim(),
        _ => {
            let payload = json!({
                "kind": "missing_command",
                "retryable": false,
                "safe_to_retry": true,
                "recommended_action": "provide_command_field",
            });
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ local_shell 缺少必填参数 'command'（待执行的命令行）。\n<!-- 结构化元数据: {} -->", payload),
                success: false,
                route: "local_shell:missing_command".to_string(),
            };
        }
    };

    // 支持可选的 args 参数列表
    let args_list: Vec<String> = params
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let final_cmd = build_full_command(raw_cmd, &args_list);

    // 工具误用引导（非安全边界）：纯文件读取命令应使用类型化工具 local_file(read)
    // 建立 SHA256 乐观并发基线，而非 shell（shell 读文件会让后续 edit/write 既有文件
    // 因缺基线被硬拦，且绕过结构化审计）。仅拦"单条、无管道、无重定向的纯读取"；
    // 任何歧义（管道组合/重定向/设备路径）一律放行，交给审计台账与 finish 三分法门禁兜底。
    if pure_file_read_command(&final_cmd) {
        crate::agent::telemetry::record_gate_fire(
            db,
            &crate::agent::telemetry::GateFireRecord {
                run_id: session_id,
                session_id: &session_id,
                step_id: 0,
                gate: "shell_tool_misuse",
                route: "shell_tool_misuse:file_read_via_shell",
            },
        );
        return DispatchResult { view: None,
            stdout: String::new(),
            success: false,
            route: "local_shell:file_read_misuse".to_string(),
            stderr: "❌ [工具误用拦截] 检测到你正通过 shell 执行纯文件读取命令。\n\
                     💡 系统规范：查看/读取文件必须调用类型化工具 `local_file(action: \"read\", path: \"...\", start_line?: number, end_line?: number)`，代码检索调用 `search`。\n\
                     原因：① shell 读文件无法建立乐观并发哈希基线，后续 edit/write 该既有文件会被硬拦截；② 绕过结构化审计与行号定位。\n\
                     构建、测试、代码检查与环境探针类命令不受此约束，可正常执行。\n\
                     <!-- 结构化元数据: {\"kind\":\"file_read_via_shell\",\"retryable\":false,\"safe_to_retry\":false,\"recommended_action\":\"use_local_file_read\"} -->"
                .to_string(),
        };
    }

    // 确定执行工作目录 (cwd)，受 path_guard 严格约束
    let execution_cwd: PathBuf = match params.get("cwd").and_then(|v| v.as_str()) {
        Some(custom_cwd) if !custom_cwd.trim().is_empty() => {
            match ensure_allowed_path(custom_cwd.trim(), ws_root, Some(session_id), false) {
                Ok(resolved) => resolved.canonical_path,
                Err(e) => {
                    let payload = json!({
                        "kind": "cwd_denied",
                        "retryable": false,
                        "safe_to_retry": true,
                        "recommended_action": "use_path_within_workspace",
                    });
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("❌ [执行目录拒绝] 指定的 cwd 不在合法白名单内: {}\n<!-- 结构化元数据: {} -->", e, payload),
                        success: false,
                        route: "local_shell:cwd_denied".to_string(),
                    };
                }
            }
        }
        _ => ws_root.to_path_buf(),
    };

    let timeout_secs = params
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .min(MAX_TIMEOUT_SECS);

    // 提取可选自定义环境变量
    let custom_env: Option<HashMap<String, String>> = params
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|val| (k.clone(), val.to_string())))
                .collect()
        });

    execute_command_blocking(session_id, &final_cmd, &execution_cwd, timeout_secs, custom_env)
}

fn execute_command_blocking(
    session_id: &str,
    cmd_str: &str,
    cwd: &Path,
    timeout_secs: u64,
    custom_env: Option<HashMap<String, String>>,
) -> DispatchResult {
    let start_time = Instant::now();
    let enriched_path = get_enriched_path_env();
    let session_tmp_dir = crate::agent::path_guard::get_session_tmp_dir(session_id);
    let _ = std::fs::create_dir_all(&session_tmp_dir);
    let session_tmp_str = session_tmp_dir.to_string_lossy().to_string();

    // 使用 tokio 异步派发并挂载独立 Reactor 与超时控制
    let fut = async move {
        #[cfg(target_os = "windows")]
        let mut child = {
            let mut c = tokio::process::Command::new("cmd.exe");
            c.arg("/C").arg(cmd_str);
            c.current_dir(cwd);
            c.env("PATH", &enriched_path);
            c.env("SESSION_TMP_DIR", &session_tmp_str);
            c.env("TMPDIR", &session_tmp_str);
            if let Some(ref env_map) = custom_env {
                for (k, v) in env_map {
                    c.env(k, v);
                }
            }
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
            c.spawn().map_err(|e| format_spawn_error(e, cmd_str, cwd, &enriched_path))?
        };

        #[cfg(not(target_os = "windows"))]
        let mut child = {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(cmd_str);
            c.current_dir(cwd);
            c.env("PATH", &enriched_path);
            c.env("SESSION_TMP_DIR", &session_tmp_str);
            c.env("TMPDIR", &session_tmp_str);
            c.process_group(0); // 独立进程组：以子进程为主导组长，方便超时按组整棵树杀死
            if let Some(ref env_map) = custom_env {
                for (k, v) in env_map {
                    c.env(k, v);
                }
            }
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
            c.spawn().map_err(|e| format_spawn_error(e, cmd_str, cwd, &enriched_path))?
        };

        let child_pid = child.id();
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();

        let timeout_dur = Duration::from_secs(timeout_secs);

        let wait_fut = async {
            use tokio::io::AsyncReadExt;
            let read_stdout = async {
                if let Some(mut r) = stdout_handle {
                    let _ = r.read_to_end(&mut stdout_bytes).await;
                }
            };
            let read_stderr = async {
                if let Some(mut r) = stderr_handle {
                    let _ = r.read_to_end(&mut stderr_bytes).await;
                }
            };
            let ((), (), status_res) = tokio::join!(read_stdout, read_stderr, child.wait());
            status_res
        };

        match tokio::time::timeout(timeout_dur, wait_fut).await {
            Ok(Ok(status)) => Ok((status, stdout_bytes, stderr_bytes)),
            Ok(Err(e)) => Err((
                "spawn_error".to_string(),
                false,
                true,
                "check_command_path_and_permissions".to_string(),
                format!("命令等待执行结果失败: {e}"),
            )),
            Err(_) => {
                // 1. 发送 SIGTERM 软杀整个进程组（向负 PID 发信号）
                if let Some(pid) = child_pid {
                    kill_process_group(pid, "TERM");
                }
                // 2. 给予最多 1.5 秒优雅清理缓冲期，若超时未退则硬杀 (SIGKILL)
                let exited = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
                if exited.is_err() {
                    if let Some(pid) = child_pid {
                        kill_process_group(pid, "KILL");
                    }
                    let _ = child.kill().await;
                }

                Err((
                    "process_timeout".to_string(),
                    true,
                    false,
                    "check_state_before_retry_or_increase_timeout".to_string(),
                    format!(
                        "⚠️ [执行超时] 命令运行超过 {} 秒上限，已被系统强制终止杀死（已向进程树发送清理信号）。",
                        timeout_secs
                    ),
                ))
            }
        }
    };

    let run_in_rt = || -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), (String, bool, bool, String, String)> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| (
                "runtime_error".to_string(),
                false,
                true,
                "retry".to_string(),
                format!("创建 Tokio 运行时失败: {e}"),
            ))?;
        rt.block_on(fut)
    };

    let result = if tokio::runtime::Handle::try_current().is_ok() {
        // 若当前已在 Tokio 运行时内（如主循环/异步任务线程），派生独立 OS 线程运行独立的单线程 Tokio Reactor，
        // 既保障有完整的进程 Reactor 信号监听，又彻底消除"在运行时内重入启动运行时"或死锁隐患。
        std::thread::scope(|s| {
            s.spawn(|| run_in_rt())
                .join()
                .unwrap_or_else(|_| Err((
                    "thread_crashed".to_string(),
                    false,
                    false,
                    "check_host_system".to_string(),
                    "命令执行线程意外退出".to_string(),
                )))
        })
    } else {
        run_in_rt()
    };

    let duration_ms = start_time.elapsed().as_millis();

    match result {
        Ok((status, stdout_bytes, stderr_bytes)) => {
            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            let raw_stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let raw_stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

            let stdout_trimmed = truncate_output(&raw_stdout, MAX_OUTPUT_BYTES);
            let stderr_trimmed = truncate_output(&raw_stderr, MAX_OUTPUT_BYTES);
            let is_truncated = raw_stdout.len() > MAX_OUTPUT_BYTES || raw_stderr.len() > MAX_OUTPUT_BYTES;

            let (kind, retryable, safe_to_retry, recommended_action) = if success {
                ("success", false, true, "done")
            } else {
                ("non_zero_exit", true, false, "inspect_stderr_and_fix_arguments")
            };

            let status_badge = if success {
                format!("✅ [成功] (exit_code: 0, 耗时: {}ms)", duration_ms)
            } else {
                format!(
                    "⚠️ [退出码非零] (exit_code: {}, 建议: {}, 耗时: {}ms)",
                    exit_code, recommended_action, duration_ms
                )
            };

            let mut formatted_stdout = format!("{}\n$ {}\n", status_badge, cmd_str);

            if !stdout_trimmed.is_empty() {
                formatted_stdout.push_str(&format!("\n--- STDOUT ---\n{}", stdout_trimmed));
            }
            if !stderr_trimmed.is_empty() {
                formatted_stdout.push_str(&format!("\n--- STDERR ---\n{}", stderr_trimmed));
            }

            let payload = json!({
                "command": cmd_str,
                "cwd": cwd.display().to_string(),
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "success": success,
                "kind": kind,
                "retryable": retryable,
                "safe_to_retry": safe_to_retry,
                "recommended_action": recommended_action,
                "truncated": is_truncated,
            });

            DispatchResult { view: None,
                stdout: format!("{}\n\n<!-- 结构化元数据: {} -->", formatted_stdout.trim_end(), payload),
                stderr: if success { String::new() } else { format!("命令退出码: {}", exit_code) },
                success,
                route: format!("local_shell:{}", kind),
            }
        }
        Err((kind, retryable, safe_to_retry, recommended_action, err_msg)) => {
            let payload = json!({
                "command": cmd_str,
                "cwd": cwd.display().to_string(),
                "exit_code": -1,
                "duration_ms": duration_ms,
                "success": false,
                "kind": kind,
                "retryable": retryable,
                "safe_to_retry": safe_to_retry,
                "recommended_action": recommended_action,
            });

            DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ {}\n<!-- 结构化元数据: {} -->", err_msg, payload),
                success: false,
                route: format!("local_shell:{}", kind),
            }
        }
    }
}

/// 友好详细的进程启动异常诊断
fn format_spawn_error(e: std::io::Error, cmd: &str, cwd: &Path, path_env: &str) -> (String, bool, bool, String, String) {
    if e.kind() == std::io::ErrorKind::NotFound {
        if !cwd.exists() {
            (
                "cwd_not_found".to_string(),
                false,
                true,
                "check_working_directory".to_string(),
                format!("执行工作目录不存在: '{}' (请核对路径有效性)", cwd.display()),
            )
        } else {
            (
                "command_not_found".to_string(),
                false,
                true,
                "check_command_path_and_permissions".to_string(),
                format!(
                    "未在系统 PATH 中找到待执行命令 (spawn ENOENT)。\n待执行: {}\n补充后的 PATH: {}\n建议：检查该工具是否已安装或提供完整可执行文件绝对路径。",
                    cmd, path_env
                ),
            )
        }
    } else {
        (
            "spawn_error".to_string(),
            false,
            true,
            "check_permissions_and_args".to_string(),
            format!("命令进程启动失败: {e}"),
        )
    }
}

/// 智能截断超长输出（优先保留头尾关键上下文）
fn truncate_output(s: &str, max_bytes: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }

    let head_bytes = max_bytes / 3;
    let tail_bytes = max_bytes - head_bytes;

    let head: String = trimmed.chars().take(head_bytes).collect();
    let tail: String = trimmed.chars().rev().take(tail_bytes).collect::<String>().chars().rev().collect();

    format!(
        "{}\n\n... [⚠️ 篇幅控制：中间输出已省略，总大小 {} 字节 > 50KB 上限] ...\n\n{}",
        head.trim_end(),
        trimmed.len(),
        tail.trim_start()
    )
}

/// 判定命令是否为"纯文件读取"（工具误用引导，非安全边界）。
/// 仅当整条命令为单条、无管道/重定向/命令分隔的文件内容读取时才命中；
/// 任何歧义（管道组合如 `cargo test | tail`、重定向落盘、`/dev` `/proc` 设备路径、
/// 组合命令）一律放行——误拦的代价（模型对抗绕行）高于漏拦（已有审计台账与
/// finish 三分法门禁兜底）。rg/grep 不拦：检索是 shell 的合法用途。
fn pure_file_read_command(cmd: &str) -> bool {
    let structural = AuditLedger::blank_quoted_segments(cmd);
    if structural.contains(['|', '>', '<', ';', '&']) {
        return false;
    }
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return false;
    };
    let unquoted = first.trim_matches(|c| c == '\'' || c == '"');
    let base = unquoted.rsplit('/').next().unwrap_or(unquoted);
    if !matches!(base, "cat" | "sed" | "head" | "tail" | "less" | "more") {
        return false;
    }
    // 设备与虚拟路径有合法用途（如 head -c 32 /dev/urandom 取随机数），放行
    if tokens.iter().any(|t| t.contains("/dev/") || t.contains("/proc/")) {
        return false;
    }
    if base == "sed" {
        return AuditLedger::is_sed_readonly(&tokens);
    }
    // tail -f 属持续读取（会挂起到超时），同样按文件读取引导到 local_file
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> DbState {
        let base =
            std::env::temp_dir().join(format!("free-api-shell-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )))
    }

    #[test]
    fn pure_file_read_classifier_hits_only_unambiguous_reads() {
        // 命中：单条、无管道、无重定向的文件内容读取
        assert!(pure_file_read_command("sed -n '265,340p' src-tauri/src/agent/main_loop.rs"));
        assert!(pure_file_read_command("cat foo.txt"));
        assert!(pure_file_read_command("tail -20 build.log"));
        assert!(pure_file_read_command("head -n 50 src/lib.rs"));
        assert!(pure_file_read_command("sed -n '1p;2p' file.txt"));
        // 放行：管道组合（输出过滤/统计是 shell 的合法用途）
        assert!(!pure_file_read_command("cargo test 2>&1 | tail -20"));
        assert!(!pure_file_read_command("cat config.yaml | grep port | wc -l"));
        // 放行：重定向落盘（写入类，交审计台账记账）
        assert!(!pure_file_read_command("cat a.txt > b.txt"));
        // 放行：设备/虚拟路径（如取随机数）
        assert!(!pure_file_read_command("head -c 32 /dev/urandom"));
        // 放行：非读取命令
        assert!(!pure_file_read_command("cargo build --release"));
        assert!(!pure_file_read_command("grep -n foo src/**/*.rs"));
        assert!(!pure_file_read_command("rg pattern src/"));
        // 放行：组合命令（歧义，交给台账与 finish 三分法兜底）
        assert!(!pure_file_read_command("cat a; cat b"));
        // 拦截：sed 带原地写入/脚本写文件能力
        assert!(!pure_file_read_command("sed -i 's/a/b/' file.txt"));
        assert!(!pure_file_read_command("sed -n '2w /tmp/out.txt' file.txt"));
        // 引号内 `>`/`|` 字面量不改变结构判定
        assert!(pure_file_read_command("sed -n 's/a|b/c/p' file.txt"));
    }

    struct TempDirGuard(PathBuf);
    impl TempDirGuard {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("test-local-shell-{}", uuid::Uuid::new_v4()));
            let _ = std::fs::create_dir_all(&p);
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_local_shell_simple_echo() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "echo 'antigravity_test_ok'"
        });

        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(res.success);
        assert!(res.stdout.contains("antigravity_test_ok"));
        assert!(res.stdout.contains("exit_code: 0"));
        assert!(res.stdout.contains("\"kind\":\"success\""));
    }

    #[test]
    fn test_local_shell_with_args_array() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "echo",
            "args": ["antigravity_arg1", "antigravity_arg2 with space"]
        });

        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(res.success);
        assert!(res.stdout.contains("antigravity_arg1 antigravity_arg2 with space"));
        assert!(res.stdout.contains("\"recommended_action\":\"done\""));
    }

    #[test]
    fn test_local_shell_custom_env() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "echo $MY_CUSTOM_TEST_VAR",
            "env": {
                "MY_CUSTOM_TEST_VAR": "magic_env_val_999"
            }
        });

        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(res.success);
        assert!(res.stdout.contains("magic_env_val_999"));
    }

    #[test]
    fn test_local_shell_session_tmp_dir_injected() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "echo $SESSION_TMP_DIR"
        });

        let res = dispatch(&test_db(), "test_sess_tmp_check", "exec", &params, Some(ws));
        assert!(res.success);
        assert!(res.stdout.contains("test_sess_tmp_check"));
    }

    #[test]
    fn test_local_shell_non_zero_exit() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "exit 42"
        });

        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stdout.contains("exit_code: 42"));
        assert!(res.stdout.contains("\"kind\":\"non_zero_exit\""));
        assert!(res.stdout.contains("\"retryable\":true"));
    }

    #[test]
    fn test_local_shell_cwd_containment() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        // 试图逃逸到父级目录
        let params = json!({
            "command": "pwd",
            "cwd": "../../"
        });

        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stderr.contains("指定的 cwd 不在合法白名单内"));
        assert!(res.stderr.contains("\"kind\":\"cwd_denied\""));
    }

    #[test]
    fn test_local_shell_missing_command() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({});
        let res = dispatch(&test_db(), "test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stderr.contains("缺少必填参数 'command'"));
    }

    #[test]
    fn test_enriched_path_has_entries() {
        let enriched = get_enriched_path_env();
        assert!(!enriched.is_empty());
    }
}
