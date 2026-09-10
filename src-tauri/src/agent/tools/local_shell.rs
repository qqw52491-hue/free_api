//! local_shell: 原生本地命令行执行工具（受工作区物理边界与超时熔断保护）。
//!
//! 设计目标：
//! 1. 彻底解耦外部 MCP 依赖，为主 Agent 提供原生的终端命令执行与测试运行能力；
//! 2. 工作区物理边界牢笼：命令默认在当前项目工作区根目录执行，自定义 cwd 必须通过 path_guard 双白名单核验；
//! 3. 超时与死锁熔断：基于 tokio::time::timeout 实现异步自动中断（默认 60s，上限 300s）；
//! 4. 上下文保护：超长输出智能截断（保留关键头部与尾部摘要，最大 50KB / 1000 行）；
//! 5. 结构化结果输出：stdout、stderr、exit_code、duration_ms。

use crate::agent::path_guard::ensure_allowed_path;
use crate::agent::types::DispatchResult;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50KB 输出上限

pub fn dispatch(
    session_id: &str,
    _action: &str,
    params: &serde_json::Value,
    workspace_root: Option<&Path>,
) -> DispatchResult {
    let ws_root = match workspace_root {
        Some(p) => p,
        None => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ [系统错误] 未提供当前活跃的工作区路径，拒绝执行 shell 命令。".to_string(),
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
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ local_shell 缺少必填参数 'command'（待执行的命令行）。".to_string(),
                success: false,
                route: "local_shell:missing_command".to_string(),
            };
        }
    };

    // 确定执行工作目录 (cwd)
    let execution_cwd: PathBuf = match params.get("cwd").and_then(|v| v.as_str()) {
        Some(custom_cwd) if !custom_cwd.trim().is_empty() => {
            match ensure_allowed_path(custom_cwd.trim(), ws_root, Some(session_id), false) {
                Ok(resolved) => resolved.canonical_path,
                Err(e) => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: format!("❌ [执行目录拒绝] 指定的 cwd 不在合法白名单内: {}", e),
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

    execute_command_blocking(raw_cmd, &execution_cwd, timeout_secs)
}

fn execute_command_blocking(cmd_str: &str, cwd: &Path, timeout_secs: u64) -> DispatchResult {
    let start_time = Instant::now();

    // 构建命令调用（跨平台适配）
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/C").arg(cmd_str);
        c
    };

    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd_str);
        c
    };

    command.current_dir(cwd);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    // 使用 tokio 异步派发并挂载超时控制
    let fut = async move {
        #[cfg(target_os = "windows")]
        let mut child = tokio::process::Command::new("cmd.exe")
            .arg("/C")
            .arg(cmd_str)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("命令进程启动失败: {e}"))?;

        #[cfg(not(target_os = "windows"))]
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd_str)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("命令进程启动失败: {e}"))?;

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
            Ok(Err(e)) => Err(format!("命令等待执行结果失败: {e}")),
            Err(_) => {
                let _ = child.kill().await;
                Err(format!(
                    "⚠️ [执行超时] 命令运行超过 {} 秒上限，已被系统强制终止杀死。",
                    timeout_secs
                ))
            }
        }
    };

    let run_in_rt = || -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("创建 Tokio 运行时失败: {e}"))?;
        rt.block_on(fut)
    };

    let result = if tokio::runtime::Handle::try_current().is_ok() {
        // 若当前已在 Tokio 运行时内（如主循环/异步任务线程），派生独立 OS 线程运行独立的单线程 Tokio Reactor，
        // 既保障有完整的进程 Reactor 信号监听，又彻底消除"在运行时内重入启动运行时"或死锁隐患。
        std::thread::scope(|s| {
            s.spawn(|| run_in_rt())
                .join()
                .unwrap_or_else(|_| Err("命令执行线程意外退出".to_string()))
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

            let status_badge = if success { "✅ [成功]" } else { "⚠️ [退出码非零]" };

            let mut formatted_stdout = format!(
                "{} 命令执行完成 (耗时: {}ms, exit_code: {})\n$ {}\n",
                status_badge, duration_ms, exit_code, cmd_str
            );

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
            });

            DispatchResult {
                stdout: format!("{}\n\n<!-- 结构化元数据: {} -->", formatted_stdout.trim_end(), payload),
                stderr: if success { String::new() } else { format!("命令退出码: {}", exit_code) },
                success,
                route: if success { "local_shell:success".to_string() } else { "local_shell:non_zero_exit".to_string() },
            }
        }
        Err(err_msg) => DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 命令执行失败: {}", err_msg),
            success: false,
            route: "local_shell:execution_error".to_string(),
        },
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

        let res = dispatch("test_sess", "exec", &params, Some(ws));
        assert!(res.success);
        assert!(res.stdout.contains("antigravity_test_ok"));
        assert!(res.stdout.contains("exit_code: 0"));
    }

    #[test]
    fn test_local_shell_non_zero_exit() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({
            "command": "exit 42"
        });

        let res = dispatch("test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stdout.contains("exit_code: 42"));
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

        let res = dispatch("test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stderr.contains("指定的 cwd 不在合法白名单内"));
    }

    #[test]
    fn test_local_shell_missing_command() {
        let temp = TempDirGuard::new();
        let ws = temp.path();

        let params = json!({});
        let res = dispatch("test_sess", "exec", &params, Some(ws));
        assert!(!res.success);
        assert!(res.stderr.contains("缺少必填参数 'command'"));
    }
}
