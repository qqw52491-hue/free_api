use std::process::Command;

/// 异步子进程带超时执行。git 等外部命令在锁竞争、fsmonitor 异常、磁盘
/// stall 下可能永不返回——裸 `.output().await` 曾造成派发链整路冻结
/// （实弹三场僵尸会话：dispatch:child_reviewer 内采集 git status/diff
/// 挂死）。超时返回 Err，采集/探测类调用方据此降级，绝不阻塞主循环。
/// 注意：超时只放弃等待，不主动 kill 子进程（tokio 默认非 kill_on_drop，
/// 遗留只读 git 无害；写入型调用方如需严格回收请自行 kill_on_drop）。
pub async fn command_output_with_timeout(
    command: &mut tokio::process::Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        command.output(),
    )
    .await
    {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("子进程执行失败: {}", e)),
        Err(_) => Err(format!("子进程执行超时（{}s），已放弃等待", timeout_secs)),
    }
}

pub fn run_shell(cmd: &str) -> (String, String, bool) {
    let result = Command::new("bash").arg("-c").arg(cmd).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}

pub fn run_osascript(script: &str) -> (String, String, bool) {
    let result = Command::new("osascript").arg("-e").arg(script).output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            (stdout, stderr, out.status.success())
        }
        Err(e) => (String::new(), e.to_string(), false),
    }
}
