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
//! 8. 结构化结果输出与诊断建议：输出 exit_code、duration_ms、kind、retryable、safe_to_retry、recommended_action，辅助大模型自愈纠错；
//! 9. IDE 调试器注入剥离与非交互 stdin：清除 js-debug autoAttach 注入的 NODE_OPTIONS/VSCODE_INSPECTOR_OPTIONS
//!    （否则每个 node 子进程被 IDE 挂上 inspector，退出时等 "debugger to disconnect" 挂到超时）；
//!    stdin 一律 /dev/null，杜绝交互式等待与后台进程组读 TTY 触发 SIGTTIN；
//! 10. 管道排空有界化：主进程退出后只给残余输出有界排空窗口——孙进程继承管道不退出时
//!    EOF 永不到达（守护进程/npm notifier），绝不能让收集输出反过来挂住主流程。

use crate::agent::context::AuditLedger;
use crate::agent::path_guard::ensure_allowed_path;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50KB 输出上限
const LIVE_TAIL_MAX_BYTES: usize = 8 * 1024;
const LIVE_TAIL_MAX_LINES: usize = 40;
const LIVE_EMIT_INTERVAL: Duration = Duration::from_millis(150);

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
/// pub(crate)：job_cancel 中止后台 shell 任务与同步超时的双阶段杀灭共用此口径。
pub(crate) fn kill_process_group(pid: u32, signal: &str) {
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

/// 向子进程统一应用 PATH/会话 tmp/stdin-stdio 与 IDE 调试器注入净化。
/// custom_env 在净化之后应用，显式传参始终优先（保留刻意调试的逃生门）。
fn apply_child_env(
    c: &mut tokio::process::Command,
    enriched_path: &str,
    session_tmp_str: &str,
    custom_env: Option<&HashMap<String, String>>,
) {
    c.env("PATH", enriched_path);
    c.env("SESSION_TMP_DIR", session_tmp_str);
    c.env("TMPDIR", session_tmp_str);
    sanitize_debugger_env(c);
    if let Some(env_map) = custom_env {
        for (k, v) in env_map {
            c.env(k, v);
        }
    }
    // stdin 一律 /dev/null：非交互工具继承宿主 stdin 只会引入挂死面——
    // 子进程读交互提示（npx 安装确认等）永远等不到输入；stdin 为 TTY 时
    // process_group(0) 的后台进程组读 TTY 还会触发 SIGTTIN 整组暂停。
    c.stdin(std::process::Stdio::null());
    c.stdout(std::process::Stdio::piped());
    c.stderr(std::process::Stdio::piped());
}

/// 剥离 IDE 调试器自动注入：VS Code 系 IDE 的 js-debug autoAttach 会向集成终端写入
/// NODE_OPTIONS="--require .../ms-vscode.js-debug/.../bootloader.js --inspect-publish-uid=http"
/// 与 VSCODE_INSPECTOR_OPTIONS/VSCODE_INJECTION。宿主应用经该终端启动后原样继承，再透传给
/// 每个 node 子进程；IDE 自动 attach 后进程退出时卡在 "Waiting for the debugger to
/// disconnect" 直到超时被杀（vue-tsc 5 秒的活挂 60 秒即此机制）。
/// 只剔除可识别的注入片段，保留用户合法的 NODE_OPTIONS（如 --max-old-space-size）。
/// pub(crate)：host_verify/义务验证等所有宿主 spawn 子进程的通道共用同一净化口径。
pub(crate) fn sanitize_debugger_env(c: &mut tokio::process::Command) {
    for k in ["VSCODE_INSPECTOR_OPTIONS", "VSCODE_INJECTION"] {
        c.env_remove(k);
    }
    if let Ok(raw) = std::env::var("NODE_OPTIONS") {
        match filter_injected_node_options(&raw) {
            Some(filtered) if filtered.trim().is_empty() => {
                c.env_remove("NODE_OPTIONS");
            }
            Some(filtered) => {
                c.env("NODE_OPTIONS", filtered);
            }
            None => {}
        }
    }
}

/// 过滤 NODE_OPTIONS 中的 IDE 调试器注入片段。命中注入特征时返回过滤后的串
/// （可能为空串）；未命中任何注入特征返回 None 表示原样保留。
fn filter_injected_node_options(raw: &str) -> Option<String> {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let injected_path = |s: &str| s.contains("js-debug") || s.contains("bootloader.js");
    let mut kept: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        // --require <path> 是双 token 选项：路径命中注入特征时连同选项一起剔除
        if (t == "--require" || t == "-r")
            && toks.get(i + 1).map_or(false, |p| injected_path(p))
        {
            i += 2;
            continue;
        }
        if (t.starts_with("--require=") || t.starts_with("-r=")) && injected_path(t) {
            i += 1;
            continue;
        }
        // js-debug 注入的 inspector 端口发布参数；无 bootloader 时本无意义
        if t.starts_with("--inspect-publish-uid") {
            i += 1;
            continue;
        }
        kept.push(t);
        i += 1;
    }
    (kept.len() != toks.len()).then(|| kept.join(" "))
}

fn trim_live_tail(tail: &mut String) {
    if tail.len() > LIVE_TAIL_MAX_BYTES {
        let mut cut = tail.len() - LIVE_TAIL_MAX_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
    let newline_count = tail.matches('\n').count();
    if newline_count > LIVE_TAIL_MAX_LINES {
        let skip = newline_count - LIVE_TAIL_MAX_LINES;
        if let Some((idx, _)) = tail.match_indices('\n').nth(skip - 1) {
            tail.drain(..=idx);
        }
    }
}

fn push_live_chunk(tail: &Mutex<String>, pending: &mut Vec<u8>, chunk: &[u8]) {
    pending.extend_from_slice(chunk);
    let text = match std::str::from_utf8(pending) {
        Ok(valid) => {
            let owned = valid.to_string();
            pending.clear();
            owned
        }
        Err(e) if e.error_len().is_none() => {
            let valid_up_to = e.valid_up_to();
            let owned = String::from_utf8_lossy(&pending[..valid_up_to]).to_string();
            pending.drain(..valid_up_to);
            owned
        }
        Err(_) => {
            let owned = String::from_utf8_lossy(pending).to_string();
            pending.clear();
            owned
        }
    };
    if text.is_empty() {
        return;
    }
    if let Ok(mut guard) = tail.lock() {
        guard.push_str(&text);
        trim_live_tail(&mut guard);
    }
}

fn emit_shell_stream(
    session_id: &str,
    cmd_str: &str,
    tail: &str,
    elapsed_ms: u64,
) {
    use tauri::Emitter;
    if let Some(app) = crate::agent::channel::app_handle() {
        let _ = app.emit(
            &format!("agent-progress-{}", session_id),
            json!({
                "type": "shell_output_stream",
                "command": cmd_str,
                "tail": tail,
                "elapsed_ms": elapsed_ms,
            }),
        );
    }
}

async fn drain_pipe<R: tokio::io::AsyncRead + Unpin>(
    reader: Option<R>,
    live_tail: Arc<Mutex<String>>,
) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    if let Some(mut r) = reader {
        let mut chunk = [0u8; 4096];
        let mut pending = Vec::new();
        loop {
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    push_live_chunk(&live_tail, &mut pending, &chunk[..n]);
                }
            }
        }
    }
    buf
}

/// 主进程退出/被杀后，有界收集 stdout/stderr 排空任务的残余输出。
/// 超过 grace 仍未等到 EOF（孙进程持管不退）则放弃——排空任务随运行时回收。
async fn join_drains(
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
    grace: Duration,
) -> (Vec<u8>, Vec<u8>) {
    tokio::time::timeout(grace, async move {
        let out = stdout_task.await.unwrap_or_default();
        let err = stderr_task.await.unwrap_or_default();
        (out, err)
    })
    .await
    .unwrap_or_default()
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

    // 评测密封边界（FREE_API_EVAL_SEAL_ROOT/_SUBPATH 由 eval runner 注入）：
    // 判分材料按 sparse-checkout 移出沙盒工作区，但对象库与宿仓路径对 shell
    // 仍可达——实测两个模型分别以 `cat /宿仓/判据` 与 `git sparse-checkout add`
    // 读到判据全文。密封期内三条机械规则：
    //   1) 命令引用判据子路径（文件路径或 HEAD:test/... 式 git pathspec）→ 拦；
    //   2) git sparse-checkout 变更子命令（add/set/disable/reapply）→ 拦；
    //   3) 宿仓内、沙盒工作区之外的路径（绝对或 cwd 相对解析）→ 拦。
    // 工作区别名绕行（任何会话期生效，不止评测密封）：
    // `ln -sfn . src-tauri` 造自指符号链接后，`src-tauri/src-tauri/x.rs`
    // 与 `src-tauri/x.rs` 是同 inode 不同路径串——path_guard 按路径串记
    // 读基线/副作用台账，别名即盲写护栏绕行敞口。实测 seq_ledger 场
    // 模型超时前一刻真用了这条（step 82）。机械规则：ln/ln -s 的目标
    // 解析为链接位置的祖先或其自身 → 构成路径环 → 拦。
    if let Some(reason) = workspace_alias_link_violation(&final_cmd, &execution_cwd, ws_root) {
        let payload = json!({
            "kind": "workspace_alias_link",
            "retryable": false,
            "safe_to_retry": false,
            "recommended_action": "use_real_paths_only",
        });
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ [路径别名拦截] {}：在工作区内创建指向其祖先/自身的链接会制造路径环——同一文件出现两个路径串，读写基线台账被绕行。请直接用真实相对路径操作。\n<!-- 结构化元数据: {} -->", reason, payload),
            success: false,
            route: "local_shell:workspace_alias".to_string(),
        };
    }

    if let Some(reason) = sealed_sandbox_violation(&final_cmd, &execution_cwd, ws_root) {
        let payload = json!({
            "kind": "sealed_eval_sandbox",
            "retryable": false,
            "safe_to_retry": false,
            "recommended_action": "stay_within_workspace",
        });
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ [评测密封拦截] {}：判分材料已移出沙盒工作区，禁止通过 shell 绕行访问。任务所需输入均在当前工作区内。\n<!-- 结构化元数据: {} -->", reason, payload),
            success: false,
            route: "local_shell:sealed_eval_sandbox".to_string(),
        };
    }

    // 后台模式分流：长任务（视频渲染/大构建/批处理）脱离步骤超时继续跑——
    // 输出实时落文件、进程独立于步骤存活，完成态进 job 引擎由 wait_jobs
    // 挂起回收或信箱自动送达。实测驱动：6 分钟渲染被 300s 上限连产物一起
    // 杀死（ffmpeg 中途 SIGKILL → moov 未写 → MP4 整段报废），同步模式
    // 的天花板是产品级缺陷而非保守策略。
    if params
        .get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return spawn_background_shell(db, session_id, &final_cmd, &execution_cwd, custom_env);
    }

    execute_command_blocking(session_id, &final_cmd, &execution_cwd, timeout_secs, custom_env)
}

/// 后台命令执行：spawn 后立即返回，进程在独立进程组中长跑。
/// stdout/stderr 追加写入 <session_tmp>/jobs/<job_id>.log——进程脱离
/// 步骤存活期间日志持续可见；看守线程收尸后把退出码+日志尾写回
/// job 行，wait_jobs 唤醒语义与子任务交付同一通道。
fn spawn_background_shell(
    db: &DbState,
    session_id: &str,
    cmd_str: &str,
    cwd: &Path,
    custom_env: Option<HashMap<String, String>>,
) -> DispatchResult {
    let session_tmp_dir = crate::agent::path_guard::get_session_tmp_dir(session_id);
    let jobs_dir = session_tmp_dir.join("jobs");
    if let Err(e) = std::fs::create_dir_all(&jobs_dir) {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ 后台任务目录创建失败: {}", e),
            success: false,
            route: "local_shell:bg_jobs_dir".to_string(),
        };
    }

    // run_id 取当前 checkpoint（job 行归属 run 便于信箱/wait_jobs 按 run 过滤）
    let run_id = crate::agent::checkpoint::load_checkpoint(db, session_id)
        .ok()
        .flatten()
        .map(|c| c.run_id)
        .unwrap_or_else(|| session_id.to_string());

    let preview: String = cmd_str.chars().take(120).collect();
    let params_json = json!({
        "command": cmd_str,
        "cwd": cwd.display().to_string(),
    })
    .to_string();

    let job_id = match crate::agent::job_engine::begin_job(
        db, &run_id, session_id, "shell", &preview, &params_json,
    ) {
        Ok(id) => id,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ 后台任务登记失败: {}", e),
                success: false,
                route: "local_shell:bg_register".to_string(),
            };
        }
    };

    let log_path = jobs_dir.join(format!("{}.log", job_id));
    let log_out = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            let _ = crate::agent::job_engine::finish_job(
                db, &job_id, "failed", 0, None, Some(&e.to_string()),
            );
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ 后台任务日志文件创建失败: {}", e),
                success: false,
                route: "local_shell:bg_log".to_string(),
            };
        }
    };
    let log_err = match log_out.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("❌ 日志句柄复制失败: {}", e),
                success: false,
                route: "local_shell:bg_log".to_string(),
            };
        }
    };

    let enriched_path = get_enriched_path_env();
    let session_tmp_str = session_tmp_dir.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    let mut c = {
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/C").arg(cmd_str);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut c = {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd_str);
        c.process_group(0); // 独立进程组：宿主退出/步骤结束不拖垮它
        c
    };
    c.current_dir(cwd);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_out))
        .stderr(std::process::Stdio::from(log_err));
    c.env("PATH", &enriched_path)
        .env("SESSION_TMP_DIR", &session_tmp_str)
        .env("TMPDIR", &session_tmp_str);
    // 与同步路径同一净化口径：IDE 调试器注入会让 node 子进程退出时挂死
    if let Ok(no) = std::env::var("NODE_OPTIONS") {
        if !no.contains("ms-vscode") && !no.contains("bootloader.js") {
            c.env("NODE_OPTIONS", no);
        }
    }
    for k in ["VSCODE_INSPECTOR_OPTIONS", "VSCODE_INJECTION"] {
        c.env_remove(k);
    }
    if let Some(env_map) = custom_env {
        for (k, v) in env_map {
            c.env(&k, v);
        }
    }

    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            let _ = crate::agent::job_engine::finish_job(
                db, &job_id, "failed", 0, None, Some(&e.to_string()),
            );
            let (_, _, _, _, msg) = format_spawn_error(e, cmd_str, cwd, &enriched_path);
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: msg,
                success: false,
                route: "local_shell:bg_spawn".to_string(),
            };
        }
    };

    // 看守线程：wait() 收尸 → 退出码+日志尾回填 job → 广播唤醒 wait_jobs
    let child_pid = child.id();
    // pid 持久化进 job 行：job_cancel 中止、会话恢复后审计都要能按 pid 找回进程
    if let Ok(conn) = db.0.lock() {
        let _ = conn.execute(
            "UPDATE agent_background_jobs SET params_json = json_set(params_json, '$.pid', ?1) WHERE id = ?2",
            rusqlite::params![child_pid, job_id],
        );
    }
    let db_state = DbState(db.0.clone());
    let job_id_w = job_id.clone();
    let log_path_w = log_path.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        let tail = tail_of_file(&log_path_w, 4096);
        let (status_str, error_kind) = match &status {
            Ok(s) if s.success() => ("succeeded", None),
            Ok(s) => ("failed", Some(format!("exit_code:{}", s.code().unwrap_or(-1)))),
            Err(e) => ("failed", Some(format!("wait_error:{}", e))),
        };
        let result = json!({
            "exit_code": status.ok().and_then(|s| s.code()),
            "pid": child_pid,
            "log_path": log_path_w.display().to_string(),
            "tail": tail,
        });
        let _ = crate::agent::job_engine::finish_job(
            &db_state,
            &job_id_w,
            status_str,
            0,
            Some(&result.to_string()),
            error_kind.as_deref(),
        );
    });

    DispatchResult { view: None,
        stdout: format!(
            "✅ 已转入后台执行（不受步骤超时上限约束）\njob_id: {}\npid: {}（需要中止时可 local_shell `kill {}`，或对进程组 `kill -- -{}`）\n日志（实时追加，可随时 read/tail 查进度）: {}\n回收方式: `wait_jobs` 传 job_ids:[\"{}\"] 挂起等待完成；也可继续推进其他操作，完成会进收件箱自动送达。\n注意：后台进程独立于本步骤存活，若任务本身是终态交付物请确保用 wait_jobs 等到结果再 finish。",
            job_id,
            child_pid,
            child_pid,
            child_pid,
            log_path.display(),
            job_id
        ),
        stderr: String::new(),
        success: true,
        route: "local_shell:background".to_string(),
    }
}

/// 读日志文件末尾 ≤max_bytes（UTF-8 边界对齐），完成回执携带进度证据。
fn tail_of_file(path: &Path, max_bytes: usize) -> String {
    let Ok(data) = std::fs::read(path) else {
        return String::new();
    };
    let start = data.len().saturating_sub(max_bytes);
    let slice = &data[start..];
    let s = String::from_utf8_lossy(slice);
    // UTF-8 边界截断可能切出半个字符的乱码头——lossy 已转，去掉首字符更干净
    s.to_string()
}

/// 评测密封检查：返回违规原因（None=未密封或命令合规）。
/// 纯词法分析——不触碰文件系统，不含语义判断。
fn sealed_sandbox_violation<'a>(
    final_cmd: &str,
    execution_cwd: &Path,
    ws_root: &Path,
) -> Option<&'a str> {
    let seal_root = std::env::var("FREE_API_EVAL_SEAL_ROOT").ok()?;
    let seal_root = seal_root.trim();
    if seal_root.is_empty() {
        return None;
    }
    let seal_root = Path::new(seal_root);

    // 规则 1：判据子路径引用（覆盖文件路径与 git pathspec 两种形态）
    let subpath = std::env::var("FREE_API_EVAL_SEAL_SUBPATH")
        .unwrap_or_else(|_| "test/eval_cases".to_string())
        .replace('\\', "/");
    if !subpath.is_empty() && final_cmd.replace('\\', "/").contains(subpath.as_str()) {
        return Some("命令引用密封判据路径");
    }

    let toks: Vec<&str> = final_cmd
        .split(|c: char| c.is_whitespace() || "\"'`".contains(c))
        .filter(|t| !t.is_empty())
        .collect();

    // 规则 2：git 密封材料通道——sparse-checkout 剔除集变更（set '/*'
    // 不带路径同样解开剔除），以及对象库管道命令（cat-file/ls-tree：
    // ls-tree 枚举树拿 blob sha → cat-file -p 读内容，全程不带路径字样，
    // 是 substring 检查唯一漏网形态；二者对 Agent 正当工作几无用例）
    if toks.iter().any(|t| t.contains("sparse-checkout"))
        && toks
            .iter()
            .any(|t| matches!(*t, "add" | "set" | "disable" | "reapply"))
    {
        return Some("变更 sparse-checkout 剔除集");
    }
    if toks
        .iter()
        .any(|t| matches!(*t, "cat-file" | "ls-tree" | "rev-list"))
    {
        return Some("使用 git 对象库管道命令");
    }

    // 规则 3：宿仓内沙盒外路径（绝对 / cwd 相对 / = 值形式）
    for tok in &toks {
        for frag in tok.split('=') {
            let f = frag.trim_start_matches('-');
            let cand = if f.starts_with('/') {
                lex_normalize(Path::new(f))
            } else if f.contains('/') {
                lex_normalize(&execution_cwd.join(f))
            } else {
                continue;
            };
            if cand.starts_with(seal_root) && !cand.starts_with(ws_root) {
                return Some("访问宿仓内沙盒外路径");
            }
        }
    }
    None
}

/// 工作区路径别名检查：`ln [opts] TARGET LINK` 中 TARGET 解析后为 LINK
/// 所在目录的祖先或其自身 → 建成后构成路径环（同一 inode 两个路径串，
/// 读写基线/副作用台账被绕行）。纯词法，不触盘；不含语义判断。
/// 返回违规原因；None = 非 ln 命令或不构成环。
fn workspace_alias_link_violation<'a>(
    final_cmd: &str,
    execution_cwd: &Path,
    ws_root: &Path,
) -> Option<&'a str> {
    let toks: Vec<&str> = final_cmd
        .split(|c: char| c.is_whitespace() || "\"'`".contains(c))
        .filter(|t| !t.is_empty())
        .collect();
    // 逐个 ln 调用单元检查（命令可能经 && / ; 串联，先按分隔符切段再逐段判）
    let segs: Vec<Vec<&str>> = toks
        .split(|t| matches!(*t, "&&" | "||" | ";" | "|"))
        .map(|s| s.to_vec())
        .collect();
    for seg in segs {
        let Some(ln_idx) = seg.iter().position(|t| *t == "ln" || t.ends_with("/ln")) else {
            continue;
        };
        let args: Vec<&str> = seg[ln_idx + 1..]
            .iter()
            .filter(|t| !t.starts_with('-'))
            .copied()
            .collect();
        if args.len() < 2 {
            continue;
        }
        // ln TARGET LINK（LINK 缺省=当前目录）；多 TARGET+DIR 形态取末位为落点
        let link_raw = args[args.len() - 1];
        let target_raw = args[args.len() - 2];
        let resolve = |raw: &str| -> PathBuf {
            let p = Path::new(raw);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                execution_cwd.join(p)
            };
            lex_normalize(&abs)
        };
        let target = resolve(target_raw);
        // LINK 是路径本身：ln -s T dir/x → 链接物为 dir/x；ln -s T dir →
        // 链接物为 dir/basename(T)。环判定只看前者显式形态与 DIR 形态两者。
        let link_loc = resolve(link_raw);
        let link_parent = link_loc.parent().map(|p| p.to_path_buf()).unwrap_or(link_loc.clone());
        // 目标为链接或其祖先：link 落在 target 之内 → 自指/祖先环
        let creates_loop = link_loc.starts_with(&target)
            || link_parent.starts_with(&target);
        // ws_root 需归一到 canonical 形态再比较（macOS /var→/private/var 符号链：
        // execution_cwd 已 canonicalize，裸 ws_root 直接 starts_with 恒错位）。
        let ws_canon = ws_root.canonicalize().unwrap_or_else(|_| ws_root.to_path_buf());
        let inside_ws = |p: &Path| p.starts_with(&ws_canon) || p.starts_with(ws_root);
        if inside_ws(&link_loc) && inside_ws(&target) && creates_loop {
            return Some("创建指向工作区祖先路径的符号链接/硬链接（自指路径环）");
        }
    }
    None
}

/// 词法级路径归一：消解 . 与 ..，不触盘。
fn lex_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
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

    // 落点纠偏快照：shell 里裸相对 tmp/ 会落在执行目录（不随会话清理），
    // 跑完后 diff cwd/tmp 新增文件并在输出尾部提醒改用 $SESSION_TMP_DIR。
    // 不拦不搬只提醒——cwd/tmp 可能是项目自有目录，硬干预会误伤。
    let cwd_tmp_dir = cwd.join("tmp");
    let cwd_tmp_before = snapshot_dir_entries(&cwd_tmp_dir);

    let session_id_owned = session_id.to_string();
    let cmd_owned = cmd_str.to_string();

    // 使用 tokio 异步派发并挂载独立 Reactor 与超时控制
    let fut = async move {
        #[cfg(target_os = "windows")]
        let mut child = {
            let mut c = tokio::process::Command::new("cmd.exe");
            c.arg("/C").arg(cmd_str);
            c.current_dir(cwd);
            apply_child_env(&mut c, &enriched_path, &session_tmp_str, custom_env.as_ref());
            c.spawn().map_err(|e| format_spawn_error(e, cmd_str, cwd, &enriched_path))?
        };

        #[cfg(not(target_os = "windows"))]
        let mut child = {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(cmd_str);
            c.current_dir(cwd);
            c.process_group(0); // 独立进程组：以子进程为主导组长，方便超时按组整棵树杀死
            apply_child_env(&mut c, &enriched_path, &session_tmp_str, custom_env.as_ref());
            c.spawn().map_err(|e| format_spawn_error(e, cmd_str, cwd, &enriched_path))?
        };

        let child_pid = child.id();
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // 读端独立为后台排空任务：必须与 wait() 并发运行（否则输出超 64KB 管道容量
        // 时子进程写满阻塞、永远退不出 → wait() 死等）。主进程退出后只给有界排空
        // 窗口收尾残余输出——孙进程继承管道不退出时 EOF 永不到达（守护进程、
        // npm notifier 等），不能为等 EOF 把主流程挂死。
        let live_tail = Arc::new(Mutex::new(String::new()));
        let drain_stdout = tokio::spawn(drain_pipe(stdout_handle, live_tail.clone()));
        let drain_stderr = tokio::spawn(drain_pipe(stderr_handle, live_tail.clone()));

        let stream_started = Instant::now();
        let ticker_tail = live_tail.clone();
        let ticker_session = session_id_owned.clone();
        let ticker_cmd = cmd_owned.clone();
        let ticker = tokio::spawn(async move {
            let mut last_emitted = String::new();
            loop {
                tokio::time::sleep(LIVE_EMIT_INTERVAL).await;
                let current = ticker_tail.lock().map(|g| g.clone()).unwrap_or_default();
                if current != last_emitted {
                    emit_shell_stream(
                        &ticker_session,
                        &ticker_cmd,
                        &current,
                        stream_started.elapsed().as_millis() as u64,
                    );
                    last_emitted = current;
                }
            }
        });

        let timeout_dur = Duration::from_secs(timeout_secs);

        let outcome = match tokio::time::timeout(timeout_dur, child.wait()).await {
            Ok(Ok(status)) => {
                let (stdout_bytes, stderr_bytes) =
                    join_drains(drain_stdout, drain_stderr, Duration::from_secs(3)).await;
                Ok((status, stdout_bytes, stderr_bytes))
            }
            Ok(Err(e)) => {
                let _ = join_drains(drain_stdout, drain_stderr, Duration::from_secs(2)).await;
                Err((
                    "spawn_error".to_string(),
                    false,
                    true,
                    "check_command_path_and_permissions".to_string(),
                    format!("命令等待执行结果失败: {e}"),
                ))
            }
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

                // 3. 收集被终止前已产生的部分输出附进错误——诊断线索不能再随超时整体丢弃
                //    （"Debugger attached." 这类挂死因证据就写在 stderr 里）
                let (stdout_bytes, stderr_bytes) =
                    join_drains(drain_stdout, drain_stderr, Duration::from_secs(2)).await;
                let mut err_msg = format!(
                    "⚠️ [执行超时] 命令运行超过 {} 秒上限，已被系统强制终止杀死（已向进程树发送清理信号）。若这是预估本就超长的任务（视频渲染/大构建/批处理），请改用 `background: true` 转后台执行——不受此上限约束、日志落文件、wait_jobs 回收结果。",
                    timeout_secs
                );
                let partial =
                    truncate_output(&format!("{}{}", String::from_utf8_lossy(&stdout_bytes), String::from_utf8_lossy(&stderr_bytes)), 4096);
                if !partial.is_empty() {
                    err_msg.push_str(&format!("\n--- 被终止前的部分输出 ---\n{}", partial));
                }

                Err((
                    "process_timeout".to_string(),
                    true,
                    false,
                    "check_state_before_retry_or_increase_timeout".to_string(),
                    err_msg,
                ))
            }
        };

        ticker.abort();
        let final_tail = live_tail.lock().map(|g| g.clone()).unwrap_or_default();
        if !final_tail.is_empty() {
            emit_shell_stream(
                &session_id_owned,
                &cmd_owned,
                &final_tail,
                stream_started.elapsed().as_millis() as u64,
            );
        }
        outcome
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

            // 检索类命令的退出码 1 语义是「无匹配」而非执行失败——这是反向证据
            // （目标可能不存在），若笼统标"失败+修参数"会诱导模型换姿势反复重试
            // 同一个不存在的符号（实测：幻觉函数名被 grep 追了 30+ 步）。
            let search_no_match = !success
                && exit_code == 1
                && stderr_trimmed.is_empty()
                && is_search_invocation(&cmd_str);
            // shell 解析层失败（引号不配平/EOF 等）：命令根本没跑起来，
            // 修复方向是"重写引号或改用 args 数组"，不是重试/改参数语义
            let shell_syntax_error = !success
                && !search_no_match
                && is_shell_parse_error(&stderr_trimmed);
            let (kind, retryable, safe_to_retry, recommended_action) = if success {
                ("success", false, true, "done")
            } else if search_no_match {
                ("no_matches", false, false, "no_match_means_absent")
            } else if shell_syntax_error {
                ("shell_syntax", true, false, "fix_quoting_or_use_args_array")
            } else {
                ("non_zero_exit", true, false, "inspect_stderr_and_fix_arguments")
            };

            let status_badge = if success {
                format!("✅ [成功] (exit_code: 0, 耗时: {}ms)", duration_ms)
            } else if search_no_match {
                format!(
                    "🔍 [无匹配] (exit_code: 1 — 检索命令的 1 是'没有命中'不是执行失败，\
                     目标很可能不存在，请改用 list/read 验证符号或文件名而不是重试, 耗时: {}ms)",
                    duration_ms
                )
            } else if shell_syntax_error {
                format!(
                    "❌ [Shell语法错误] (exit_code: {} — 命令未执行：引号/转义不配平。\
                     含复杂引号的命令请改用 args 数组传参以完全绕过 shell 解析, 耗时: {}ms)",
                    exit_code, duration_ms
                )
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

            // 落点纠偏提醒：cwd/tmp 冒出新文件且该目录并非会话产物区本身时提示
            if !cwd_tmp_is_session_tmp(cwd, &session_tmp_dir) {
                let new_entries: Vec<String> = snapshot_dir_entries(&cwd_tmp_dir)
                    .difference(&cwd_tmp_before)
                    .cloned()
                    .collect();
                if !new_entries.is_empty() {
                    let shown: Vec<&str> = new_entries.iter().take(8).map(|s| s.as_str()).collect();
                    formatted_stdout.push_str(&format!(
                        "\n\n💡 [落点提醒] 检测到 {}/tmp/ 下新增 {} 个条目（{}{}）。\
                         该目录是当前执行目录下的 tmp/，并非会话产物区，不随会话清理、也不会被产物登记/终审自动对账。\
                         若这些是交付产物，请改用 \"$SESSION_TMP_DIR/文件名\"（={}）重新落点，\
                         或在 local_file/register_file 等工具参数中使用 tmp/ 前缀路由到会话产物区。",
                        cwd.display(),
                        new_entries.len(),
                        shown.join(", "),
                        if new_entries.len() > 8 { " 等" } else { "" },
                        session_tmp_dir.display(),
                    ));
                }
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

            // UI 结构化视图：命令/退出码/耗时 + 输出尾巴（合并 stdout+stderr 末 48 行
            // 且限 8KB——要够溢出 190px 卡片视窗才滚得动，8 行时代内容从未超过容器、
            // 滚动条形同虚设；完整输出仍在 stdout 与 step 日志）。
            let mut combined = String::new();
            if !stdout_trimmed.is_empty() {
                combined.push_str(&stdout_trimmed);
            }
            if !stderr_trimmed.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr_trimmed);
            }
            let tail: Vec<&str> = combined.lines().collect();
            let mut output_tail: String = tail
                .iter()
                .skip(tail.len().saturating_sub(48))
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if output_tail.len() > 8192 {
                // 超长单行（如 minified js）按字符截尾部，保末尾上下文
                let cut = output_tail.len() - 8192;
                let boundary = output_tail
                    .char_indices()
                    .find(|(i, _)| *i >= cut)
                    .map(|(i, _)| i)
                    .unwrap_or(output_tail.len());
                output_tail = output_tail[boundary..].to_string();
            }
            let view = json!({
                "kind": "terminal",
                "command": cmd_str,
                "cwd": cwd.display().to_string(),
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "truncated": is_truncated,
                "output_tail": output_tail,
            });

            // 失败时 stderr 必须带真实诊断尾部——重试链路只回传 stderr 字段，
            // 只写"退出码: N"等于让模型盲改（实测：cargo 编译错模型连续重读
            // 文件找不存在的错因）。stderr 优先，空则退 stdout 尾部（合并诊断）。
            let stderr_field = if success {
                String::new()
            } else if search_no_match {
                format!(
                    "检索命令未命中（exit 1 = 无匹配，非命令错误）：目标 `{}` 很可能不存在于搜索范围。\
                     建议用 list/read 验证文件名或符号名是否存在，勿仅换参数重试同一目标。",
                    cmd_str
                )
            } else if shell_syntax_error {
                format!(
                    "Shell 语法错误（exit {}）：命令未被解析执行——`{}` 存在引号不配平或未闭合结构。\
                     含多层引号/换行的命令请改用 args 数组形式传参，可完全绕过 shell 引号解析。",
                    exit_code, cmd_str
                )
            } else {
                let diag = if !stderr_trimmed.is_empty() {
                    stderr_trimmed.clone()
                } else {
                    stdout_trimmed.clone()
                };
                let tail: Vec<&str> = diag.lines().collect();
                let tail_text = tail
                    .iter()
                    .skip(tail.len().saturating_sub(60))
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("命令退出码: {}\n{}", exit_code, tail_text)
            };
            DispatchResult {
                view: Some(view),
                stdout: format!("{}\n\n<!-- 结构化元数据: {} -->", formatted_stdout.trim_end(), payload),
                stderr: stderr_field,
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

            let view = json!({
                "kind": "terminal",
                "command": cmd_str,
                "cwd": cwd.display().to_string(),
                "exit_code": -1,
                "duration_ms": duration_ms,
                "spawn_error": err_msg,
            });

            DispatchResult {
                view: Some(view),
                stdout: String::new(),
                stderr: format!("❌ {}\n<!-- 结构化元数据: {} -->", err_msg, payload),
                success: false,
                route: format!("local_shell:{}", kind),
            }
        }
    }
}

/// 快照目录内条目相对路径集合（浅递归、封顶防爆），用于 shell 前后 diff。
/// 判定命令是否为「检索类」调用（其退出码 1 语义 = 无匹配，而非失败）。
/// 管线的退出码取最后一个分段——只看 | && || ; 分隔后的末段命令。
/// 集合：grep 家族/rg/ag/ack/ugrep/git grep/which/command -v（缺席语义相同）。
fn is_search_invocation(cmd: &str) -> bool {
    let last_seg = cmd
        .rsplit(['|', ';'])
        .next()
        .and_then(|s| s.rsplit("&&").next())
        .unwrap_or(cmd)
        .trim();
    // 剥掉 VAR=x 前缀赋值取首词
    let mut tokens = last_seg.split_whitespace();
    let mut first = "";
    for t in tokens.by_ref() {
        if !t.contains('=') || t.starts_with(|c: char| !c.is_alphanumeric() && c != '_') {
            first = t;
            break;
        }
    }
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let base = base.trim_end_matches(".exe");
    match base {
        "grep" | "egrep" | "fgrep" | "rgrep" | "rg" | "ag" | "ack" | "ugrep" | "which" => true,
        // `git grep` 的 1 同样是"无匹配"；`command -v foo` 不存在返回 1
        "git" => last_seg.split_whitespace().nth(1) == Some("grep"),
        "command" => last_seg.split_whitespace().nth(1) == Some("-v"),
        _ => false,
    }
}

/// 判定 stderr 是否为 shell 解析层语法错误（命令未进入执行阶段）。
/// sh/bash/dash 的共同指纹：unexpected EOF / looking for matching / syntax error。
fn is_shell_parse_error(stderr: &str) -> bool {
    let l = stderr.to_ascii_lowercase();
    (l.contains("syntax error")
        || l.contains("unexpected eof")
        || l.contains("looking for matching")
        || l.contains("unterminated quoted string")
        || l.contains("unexpected end of file"))
        && (l.contains("sh:") || l.contains("bash:") || l.contains("-c:") || l.contains("dash"))
}

fn snapshot_dir_entries(dir: &Path) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    fn walk(d: &Path, base: &Path, set: &mut BTreeSet<String>, depth: usize) {
        if depth > 4 || set.len() >= 2000 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(rel) = p.strip_prefix(base) {
                set.insert(rel.to_string_lossy().to_string());
            }
            if p.is_dir() {
                walk(&p, base, set, depth + 1);
            }
        }
    }
    let mut set = BTreeSet::new();
    walk(dir, dir, &mut set, 0);
    set
}

/// cwd/tmp 是否等价于会话产物区：cwd 本身在会话 tmp 内（自定义 cwd=SESSION_TMP_DIR）
/// 或 cwd/tmp 软链到会话 tmp 时，裸 tmp/ 落点即正确，无需提醒。
fn cwd_tmp_is_session_tmp(cwd: &Path, session_tmp: &Path) -> bool {
    let (Ok(c), Ok(s)) = (cwd.canonicalize(), session_tmp.canonicalize()) else {
        return false;
    };
    if c.starts_with(&s) {
        return true;
    }
    c.join("tmp").canonicalize().map_or(false, |t| t == s)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_invocation_detection() {
        // 检索族：exit 1 = 无匹配
        assert!(is_search_invocation("grep -rn foo src/"));
        assert!(is_search_invocation("rg --files"));
        assert!(is_search_invocation("cat x.txt | grep pattern"));
        assert!(is_search_invocation("cd dir && rg pattern"));
        assert!(is_search_invocation("git grep -n symbol"));
        assert!(is_search_invocation("command -v ffmpeg"));
        assert!(is_search_invocation("FOO=1 rg pattern src/"));
        // 非检索族：exit 1 是真错
        assert!(!is_search_invocation("cargo test"));
        assert!(!is_search_invocation("find . -name x")); // find 无匹配返回 0，exit1=真错
        assert!(!is_search_invocation("rg --files | head")); // 末段是 head，退出码不归 rg
        assert!(!is_search_invocation("git status"));
        assert!(!is_search_invocation("python script.py"));
    }

    #[test]
    fn shell_parse_error_detection() {
        // 实测指纹（sh -c 引号未配平）
        assert!(is_shell_parse_error(
            "sh: -c: line 0: unexpected EOF while looking for matching `\"'\nsh: -c: line 1: syntax error: unexpected end of file"
        ));
        assert!(is_shell_parse_error("bash: -c: line 3: syntax error near unexpected token `('"));
        assert!(is_shell_parse_error("sh: 1: Syntax error: Unterminated quoted string"));
        // 正常业务错误不误判
        assert!(!is_shell_parse_error("grep: pattern not found"));
        assert!(!is_shell_parse_error("python: syntax error in script.py")); // 无 shell 指纹
        assert!(!is_shell_parse_error(""));
    }

    fn test_db() -> DbState {
        let base =
            std::env::temp_dir().join(format!("free-api-shell-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )))
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

    struct SealEnvGuard;
    impl SealEnvGuard {
        fn set(root: &Path) -> Self {
            std::env::set_var("FREE_API_EVAL_SEAL_ROOT", root);
            std::env::set_var("FREE_API_EVAL_SEAL_SUBPATH", "test/eval_cases");
            Self
        }
    }
    impl Drop for SealEnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("FREE_API_EVAL_SEAL_ROOT");
            std::env::remove_var("FREE_API_EVAL_SEAL_SUBPATH");
        }
    }

    #[test]
    fn test_sealed_sandbox_three_escape_vectors() {
        // 模拟评测地形：ws 在宿仓 test-artifacts/worktrees 下
        let repo = TempDirGuard::new();
        let ws_dir = repo.path().join("test-artifacts/worktrees/case-x");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let _guard = SealEnvGuard::set(repo.path());

        // ① cat 宿仓判据绝对路径 → 拦
        let p1 = json!({"command": format!("cat {}/test/eval_cases/fixtures/judge.py", repo.path().display())});
        let r1 = dispatch(&test_db(), "s", "exec", &p1, Some(&ws_dir));
        assert!(!r1.success && r1.route == "local_shell:sealed_eval_sandbox");

        // ② git sparse-checkout 解开剔除 → 拦
        let p2 = json!({"command": "git sparse-checkout add test/eval_cases/fixtures"});
        let r2 = dispatch(&test_db(), "s", "exec", &p2, Some(&ws_dir));
        assert!(!r2.success && r2.route == "local_shell:sealed_eval_sandbox");

        // ②b sparse-checkout set '/*' 不带路径也拦
        let p2b = json!({"command": "git sparse-checkout set '/*'"});
        let r2b = dispatch(&test_db(), "s", "exec", &p2b, Some(&ws_dir));
        assert!(!r2b.success);

        // ③ git show 对象库读判据 → 拦（pathspec 含子路径）
        let p3 = json!({"command": "git show HEAD:test/eval_cases/fixtures/judge.py"});
        let r3 = dispatch(&test_db(), "s", "exec", &p3, Some(&ws_dir));
        assert!(!r3.success && r3.route == "local_shell:sealed_eval_sandbox");

        // ④ 相对路径 .. 逃逸 → 拦
        let p4 = json!({"command": "cat ../../test/eval_cases/judge.py"});
        let r4 = dispatch(&test_db(), "s", "exec", &p4, Some(&ws_dir));
        assert!(!r4.success && r4.route == "local_shell:sealed_eval_sandbox");

        // ④b 对象库管道（ls-tree 拿 sha → cat-file 读内容）→ 拦
        let p4b = json!({"command": "git cat-file -p abc1234"});
        let r4b = dispatch(&test_db(), "s", "exec", &p4b, Some(&ws_dir));
        assert!(!r4b.success && r4b.route == "local_shell:sealed_eval_sandbox");

        // ⑤ 工作区内正常命令放行
        let p5 = json!({"command": "echo hello && ls"});
        let r5 = dispatch(&test_db(), "s", "exec", &p5, Some(&ws_dir));
        assert!(r5.success, "工作区内命令应放行: {}", r5.stderr);

        // ⑥ 宿仓外路径（/etc）不受限
        let p6 = json!({"command": "cat /etc/hosts | head -1"});
        let r6 = dispatch(&test_db(), "s", "exec", &p6, Some(&ws_dir));
        assert!(r6.success, "宿仓外路径应放行: {}", r6.stderr);
    }

    /// 路径别名绕行（实测 seq_ledger 场 step82 现行）：`ln -sfn . src-tauri`
    /// 造自指环后 src-tauri/src-tauri/x.rs ≡ src-tauri/x.rs，path_guard
    /// 路径串台账被双名绕行。规则不依赖评测密封——任何会话期生效。
    #[test]
    fn test_workspace_alias_link_blocked() {
        let ws = TempDirGuard::new();
        let src = ws.path().join("src-tauri");
        std::fs::create_dir_all(&src).unwrap();

        // ① 实测攻击原样：cd src-tauri && ln -sfn . src-tauri → 拦
        let p1 = json!({"command": "cd src-tauri && ln -sfn . src-tauri", "cwd": "."});
        let r1 = dispatch(&test_db(), "s", "exec", &p1, Some(ws.path()));
        assert!(!r1.success && r1.route == "local_shell:workspace_alias", "自指环必须拦: {}", r1.stderr);

        // ② 直接形态（cwd 为 ws 根）：ln -s . sub/alias → 拦
        let p2 = json!({"command": "ln -s . sub/alias"});
        let r2 = dispatch(&test_db(), "s", "exec", &p2, Some(ws.path()));
        assert!(!r2.success);

        // ③ 合法前向链接放行：ln -s dir/file alias（目标非祖先）
        std::fs::write(src.join("real.rs"), "fn x(){}").unwrap();
        let p3 = json!({"command": "ln -s src-tauri/real.rs src-tauri/alias.rs"});
        let r3 = dispatch(&test_db(), "s", "exec", &p3, Some(ws.path()));
        assert!(r3.success, "前向别名应放行: {}", r3.stderr);

        // ④ 目标在工作区外放行：ln -s /etc/hosts ./h
        let p4 = json!({"command": "ln -s /etc/hosts ./h"});
        let r4 = dispatch(&test_db(), "s", "exec", &p4, Some(ws.path()));
        assert!(r4.success, "外部目标应放行: {}", r4.stderr);

        // ⑤ 普通命令不受影响
        let p5 = json!({"command": "echo ok"});
        let r5 = dispatch(&test_db(), "s", "exec", &p5, Some(ws.path()));
        assert!(r5.success);
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
    fn test_local_shell_cwd_tmp_write_triggers_landing_nudge() {
        // 裸相对 tmp/ 落到执行目录 → 输出尾部应附"落点提醒"引导到 $SESSION_TMP_DIR
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let res = dispatch(
            &test_db(),
            "test_sess_nudge",
            "exec",
            &json!({"command": "mkdir -p tmp && echo hi > tmp/stray.txt"}),
            Some(ws),
        );
        assert!(res.success);
        assert!(res.stdout.contains("落点提醒"), "cwd/tmp 新文件应触发落点提醒: {}", res.stdout);
        assert!(res.stdout.contains("SESSION_TMP_DIR"), "提醒应指向 SESSION_TMP_DIR: {}", res.stdout);
        assert!(ws.join("tmp/stray.txt").exists(), "文件应真实落在工作区 tmp/");

        // 对照：不写 tmp/ 的普通命令不触发提醒
        let res2 = dispatch(
            &test_db(),
            "test_sess_nudge",
            "exec",
            &json!({"command": "echo plain"}),
            Some(ws),
        );
        assert!(res2.success);
        assert!(!res2.stdout.contains("落点提醒"), "无 tmp 写入不应提醒: {}", res2.stdout);
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

    #[test]
    fn filter_injected_node_options_strips_ide_debugger_bits() {
        // IDE 注入原型（实测 NODE_OPTIONS 值）：整串过滤后应为空
        let injected = " --require /opt/antigravity-ide/resources/app/extensions/ms-vscode.js-debug/src/bootloader.js  --inspect-publish-uid=http";
        assert_eq!(
            filter_injected_node_options(injected).as_deref(),
            Some("")
        );
        // --require=<path> 单 token 形态同样剔除
        let injected_eq = "--require=/x/extensions/ms-vscode.js-debug/src/bootloader.js";
        assert_eq!(
            filter_injected_node_options(injected_eq).as_deref(),
            Some("")
        );
        // 混合：合法选项保留，注入片段剔除
        let mixed = "--max-old-space-size=4096 --require /x/js-debug/bootloader.js --inspect-publish-uid=http";
        assert_eq!(
            filter_injected_node_options(mixed).as_deref(),
            Some("--max-old-space-size=4096")
        );
        // 无注入特征 → None（原样保留，不触发 env 改写）
        assert_eq!(filter_injected_node_options("--max-old-space-size=4096"), None);
        // 普通 --require 不命中注入特征 → 保留
        assert_eq!(
            filter_injected_node_options("--require ts-node/register"),
            None
        );
        // 空串 → None
        assert_eq!(filter_injected_node_options("   "), None);
    }

    #[test]
    fn test_local_shell_stdin_is_dev_null() {
        // 子进程 stdin 必须是 /dev/null：read 应立即 EOF 失败而非永久等待输入
        let temp = TempDirGuard::new();
        let ws = temp.path();
        let res = dispatch(
            &test_db(),
            "test_sess_stdin",
            "exec",
            &json!({"command": "read -t 2 line && echo GOT || echo EOF_FAST", "timeout_secs": 30}),
            Some(ws),
        );
        assert!(res.success, "stdin=null 下 read 应立即 EOF: {}", res.stderr);
        assert!(res.stdout.contains("EOF_FAST"));
    }

    /// 后台模式：立即返回 job_id、不受 timeout_secs 约束、进程独立存活、
    /// 看守线程收尸后写回 job 行终态（exit_code + 日志尾）。
    /// 实证驱动：超长渲染任务被同步 300s 上限连产物杀死——必须有一条
    /// 不随步骤超时死的执行通道。
    #[test]
    fn background_mode_survives_and_reports_via_job_row() {
        let temp = TempDirGuard::new();
        let ws = temp.path();
        let db = test_db();
        let marker = ws.join("bg_marker.txt");
        let cmd = format!(
            "sleep 1; echo done-content > {}",
            marker.display()
        );
        let res = dispatch(
            &db,
            "test_sess_bg",
            "exec",
            &json!({"command": cmd, "timeout_secs": 60, "background": true}),
            Some(ws),
        );
        assert!(res.success, "background 派发应立即返回成功: {}", res.stderr);
        assert!(res.stdout.contains("job_id"), "应返回 job 句柄: {}", res.stdout);
        let job_id = res
            .stdout
            .split("job_id: ")
            .nth(1)
            .and_then(|s| s.lines().next())
            .unwrap_or("")
            .trim()
            .to_string();
        assert!(job_id.starts_with("job-"), "job_id: {}", job_id);

        // 等看守线程写终态（命令自身 1s，宽松窗口轮询）
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut status = String::new();
        let mut result_json = String::new();
        while std::time::Instant::now() < deadline {
            let conn = db.0.lock().unwrap();
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT status, COALESCE(result_json,'') FROM agent_background_jobs WHERE id=?1",
                    rusqlite::params![job_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            drop(conn);
            if let Some((s, rj)) = row {
                if s != "running" {
                    status = s;
                    result_json = rj;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(status, "succeeded", "后台任务应以 succeeded 收官: {}", result_json);
        assert!(result_json.contains("\"exit_code\":0"), "result_json: {}", result_json);
        assert!(marker.exists(), "产物文件应由后台进程写出");
    }

    /// 卡死中止链路：长睡后台任务 → job 行取 pid → kill_process_group →
    /// 看守线程 wait() 返回后写终态。覆盖"模型发现卡死可主动 kill"的诉求。
    #[test]
    fn background_job_is_killed_and_row_turns_terminal() {
        let temp = TempDirGuard::new();
        let ws = temp.path();
        let db = test_db();
        let res = dispatch(
            &db,
            "test_sess_bg_kill",
            "exec",
            &json!({"command": "sleep 300", "timeout_secs": 60, "background": true}),
            Some(ws),
        );
        assert!(res.success);
        let job_id = res.stdout.split("job_id: ").nth(1)
            .and_then(|s| s.lines().next()).unwrap_or("").trim().to_string();

        // pid 已持久化进 params_json
        let pid: u32 = {
            let conn = db.0.lock().unwrap();
            let pj: String = conn
                .query_row("SELECT params_json FROM agent_background_jobs WHERE id=?1",
                    rusqlite::params![job_id], |r| r.get(0))
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&pj).unwrap()
                .get("pid").unwrap().as_u64().unwrap() as u32
        };

        kill_process_group(pid, "TERM");

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut status = String::new();
        while std::time::Instant::now() < deadline {
            let conn = db.0.lock().unwrap();
            let s: Option<String> = conn
                .query_row("SELECT status FROM agent_background_jobs WHERE id=?1",
                    rusqlite::params![job_id], |r| r.get(0))
                .ok();
            drop(conn);
            if let Some(s) = s {
                if s != "running" { status = s; break; }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(status, "failed", "被 TERM 杀掉的后台任务应落 failed 终态");
    }
}
