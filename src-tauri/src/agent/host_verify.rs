//! 宿主执行验证：把「模型自报测试通过」升级为「宿主亲测退出码」。
//!
//! finish 门禁检查的是 ChangeVerification 声明的结构合规性（verified 必须带
//! evidence_refs），但"模型说跑过"和"真的跑过"之间隔着整个幻觉空间。本模块
//! 在收尾审查前由宿主亲自执行配置的验证命令（channel-config.json 的
//! verify_test_command，如 "cargo test"），把真实退出码与输出尾部塞进 reviewer
//! 的证据包——审查从文本审查升级为行为验证。命令失败不直接拦截（v1 只注入
//! 证据），由 reviewer 据实评判。

use std::path::Path;
use std::time::Duration;

fn tail(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > max {
        format!("…{}", chars[chars.len() - max..].iter().collect::<String>())
    } else {
        s.to_string()
    }
}

/// 验证命令的工作目录解析：模型/审查员声明的 verify 命令常是裸工具调用
/// （如 `cargo test --lib`），而仓库 manifest 常在子目录（如 src-tauri/）。
/// 在 worktree 根裸跑必然 "could not find manifest" —— 实测一场 finish 被同一
/// 无效命令拦 12 次。项目根是机械事实：按命令工具链找 manifest，单一命中才改
/// 用该目录；根目录已有 manifest 或多义时保持现状不猜。
fn resolve_verify_dir(workspace: &Path, cmd: &str) -> std::path::PathBuf {
    let head = cmd.trim_start();
    // 已显式 cd 的命令尊重作者意图，不干预
    if head.starts_with("cd ") {
        return workspace.to_path_buf();
    }
    let manifest: &str = if head.starts_with("cargo ") || head.starts_with("cargo-") {
        "Cargo.toml"
    } else if head.starts_with("npm ")
        || head.starts_with("pnpm ")
        || head.starts_with("yarn ")
        || head.starts_with("bun ")
        || head.starts_with("npx ")
        || head.starts_with("vitest")
        || head.starts_with("jest")
    {
        "package.json"
    } else if head.starts_with("pytest") || head.starts_with("python -m pytest") {
        "pyproject.toml"
    } else if head.starts_with("go test") || head.starts_with("go build") {
        "go.mod"
    } else {
        return workspace.to_path_buf();
    };
    if workspace.join(manifest).exists() {
        return workspace.to_path_buf();
    }
    // 只下探一层：恰好一个直接子目录含该 manifest 时才采用，多义不猜
    let mut hits = std::fs::read_dir(workspace)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir() && e.path().join(manifest).exists())
        .map(|e| e.path());
    match (hits.next(), hits.next()) {
        (Some(dir), None) => dir,
        _ => workspace.to_path_buf(),
    }
}

/// 宿主权威验证的结构化结果：退出码 + 输出尾部 + 通过与否。
/// 与 run() 的文本报告不同，这里把"是否通过"显式结构化——调用方据此做硬判。
pub struct HostVerifyOutcome {
    pub passed: bool,
    pub exit_code: String,
    pub output_tail: String,
}

/// 宿主亲自执行一条验证命令。净化 IDE 调试器注入（NODE_OPTIONS 里的
/// js-debug bootloader 会让 node 子进程退出时挂在 "Waiting for the debugger
/// to disconnect"）、stdin 置空（.output() 默认即如此）、kill_on_drop 保证
/// 超时丢弃 future 时子进程被回收而非成孤儿。
pub async fn run_command(
    workspace: &Path,
    cmd: &str,
    timeout_secs: u64,
) -> HostVerifyOutcome {
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(resolve_verify_dir(workspace, cmd))
        .kill_on_drop(true);
    crate::agent::tools::local_shell::sanitize_debugger_env(&mut command);
    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), command.output()).await;
    match result {
        Err(_) => HostVerifyOutcome {
            passed: false,
            exit_code: "timeout".to_string(),
            output_tail: format!("{}s 超时，宿主强制终止", timeout_secs),
        },
        Ok(Err(e)) => HostVerifyOutcome {
            passed: false,
            exit_code: "spawn-failed".to_string(),
            output_tail: format!("启动失败: {e}"),
        },
        Ok(Ok(out)) => {
            let code = out.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let mut output_tail = tail(&stdout, 1200);
            if !stderr.trim().is_empty() {
                output_tail.push_str(" | stderr: ");
                output_tail.push_str(&tail(&stderr, 400));
            }
            HostVerifyOutcome {
                passed: code == 0,
                exit_code: code.to_string(),
                output_tail,
            }
        }
    }
}

/// 执行配置的验证命令。未配置或命令为空返回 None。
pub async fn run(workspace: &Path) -> Option<String> {
    let cmd = crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.verify_test_command.clone())
        .filter(|s| !s.trim().is_empty())?;
    let timeout_secs = crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.verify_timeout_secs)
        .unwrap_or(120)
        .clamp(10, 600);

    let outcome = run_command(workspace, &cmd, timeout_secs).await;
    Some(match outcome.exit_code.as_str() {
        "timeout" => format!(
            "命令: {}\n结果: 超时（{}s 上限，宿主强制终止）",
            cmd, timeout_secs
        ),
        "spawn-failed" => format!("命令: {}\n结果: 启动失败: {}", cmd, outcome.output_tail),
        code => format!(
            "命令: {}\n退出码: {}\n输出尾部:\n{}",
            cmd, code, outcome.output_tail
        ),
    })
}

/// 逐义务验证结果：命令、退出码、输出尾部。
pub struct ObligationVerifyResult {
    pub obligation_id: String,
    pub command: String,
    pub exit_code: String,
    pub output_tail: String,
    pub passed: bool,
}

/// 【B 路径执行器】运行契约义务绑定的冻结验证脚本/命令。
/// 冻结副本语义：脚本内容取自 checkpoint 里的快照（批准时冻结），写临时文件执行，
/// 工作区内同名脚本被模型改动不影响判定；sha256 不匹配 = 快照损坏，fail-closed。
/// 任一验证失败 = 返回集合里存在 passed=false（调用方进 blocker），基础设施故障
/// （无法写临时文件等）同样判 failed 并注明——验证器故障绝不放行。
pub async fn run_obligation_verifications(
    workspace: &Path,
    obligations: &[crate::agent::review_contract::ReviewObligation],
) -> Vec<ObligationVerifyResult> {
    use sha2::{Digest, Sha256};
    let mut results = Vec::new();
    for ob in obligations {
        let Some(command) = ob.verify_command.as_deref().filter(|c| !c.trim().is_empty()) else {
            continue;
        };
        let id = ob.id.clone();
        // 冻结脚本：写临时文件运行（sha256 核验快照完整性）
        let mut script_path: Option<std::path::PathBuf> = None;
        if let Some(script) = ob.verify_script.as_deref().filter(|c| !c.trim().is_empty()) {
            let digest = format!("{:x}", Sha256::digest(script.as_bytes()));
            let expected = ob.verify_sha256.as_deref();
            if let Some(expected) = expected {
                if expected != digest {
                    results.push(ObligationVerifyResult {
                        obligation_id: id,
                        command: command.to_string(),
                        exit_code: "fail-closed".to_string(),
                        output_tail: "冻结脚本哈希不匹配（快照损坏或被篡改），按失败处理".to_string(),
                        passed: false,
                    });
                    continue;
                }
            }
            let tmp = std::env::temp_dir().join(format!(
                "obligation-verify-{}-{}",
                ob.id,
                std::process::id()
            ));
            match std::fs::write(&tmp, script) {
                Ok(()) => script_path = Some(tmp),
                Err(e) => {
                    results.push(ObligationVerifyResult {
                        obligation_id: id,
                        command: command.to_string(),
                        exit_code: "fail-closed".to_string(),
                        output_tail: format!("冻结脚本写入临时文件失败: {e}"),
                        passed: false,
                    });
                    continue;
                }
            }
        }
        // 命令若引用脚本，统一改指冻结副本（工作区内路径不再可信）
        let effective_command = match (&script_path, ob.verify_script.as_deref()) {
            (Some(tmp), Some(script)) => {
                let base = std::path::Path::new(script)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("verify.py");
                command.replace(base, tmp.to_string_lossy().as_ref())
            }
            _ => command.to_string(),
        };
        let mut verify_cmd = tokio::process::Command::new("sh");
        verify_cmd
            .arg("-c")
            .arg(&effective_command)
            .current_dir(resolve_verify_dir(workspace, &effective_command))
            .kill_on_drop(true);
        crate::agent::tools::local_shell::sanitize_debugger_env(&mut verify_cmd);
        let outcome = tokio::time::timeout(Duration::from_secs(120), verify_cmd.output()).await;
        let (exit_code, tail, passed) = match outcome {
            Err(_) => ("timeout".to_string(), "120s 超时，宿主强制终止".to_string(), false),
            Ok(Err(e)) => (fail_closed(), format!("启动失败: {e}"), false),
            Ok(Ok(out)) => {
                let code = out.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut out_tail = tail(&stdout, 400);
                if !stderr.is_empty() {
                    out_tail.push_str(" | stderr: ");
                    out_tail.push_str(&tail(&stderr, 200));
                }
                (code.to_string(), out_tail, code == 0)
            }
        };
        results.push(ObligationVerifyResult {
            obligation_id: id,
            command: command.to_string(),
            exit_code,
            output_tail: tail,
            passed,
        });
    }
    results
}

fn fail_closed() -> String {
    "fail-closed".to_string()
}

#[cfg(test)]
mod tests {
    use super::resolve_verify_dir;
    use std::fs;

    #[test]
    fn bare_cargo_resolves_to_single_manifest_subdir() {
        let ws = std::env::temp_dir().join(format!("hv-ws-{}", std::process::id()));
        fs::create_dir_all(ws.join("src-tauri")).unwrap();
        fs::write(ws.join("src-tauri/Cargo.toml"), "[package]").unwrap();
        let got = resolve_verify_dir(&ws, "cargo test --lib");
        assert_eq!(got, ws.join("src-tauri"));
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn manifest_at_root_stays() {
        let ws = std::env::temp_dir().join(format!("hv-root-{}", std::process::id()));
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(resolve_verify_dir(&ws, "cargo test"), ws);
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn ambiguous_manifests_keep_root() {
        let ws = std::env::temp_dir().join(format!("hv-amb-{}", std::process::id()));
        fs::create_dir_all(ws.join("a")).unwrap();
        fs::create_dir_all(ws.join("b")).unwrap();
        fs::write(ws.join("a/Cargo.toml"), "[package]").unwrap();
        fs::write(ws.join("b/Cargo.toml"), "[package]").unwrap();
        assert_eq!(resolve_verify_dir(&ws, "cargo test"), ws);
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn explicit_cd_and_unknown_tool_untouched() {
        let ws = std::env::temp_dir().join(format!("hv-cd-{}", std::process::id()));
        fs::create_dir_all(ws.join("src-tauri")).unwrap();
        fs::write(ws.join("src-tauri/Cargo.toml"), "[package]").unwrap();
        assert_eq!(resolve_verify_dir(&ws, "cd src-tauri && cargo test"), ws);
        assert_eq!(resolve_verify_dir(&ws, "node scripts/check.mjs"), ws);
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn npm_resolves_package_json_subdir() {
        let ws = std::env::temp_dir().join(format!("hv-npm-{}", std::process::id()));
        fs::create_dir_all(ws.join("web")).unwrap();
        fs::write(ws.join("web/package.json"), "{}").unwrap();
        assert_eq!(resolve_verify_dir(&ws, "npm test"), ws.join("web"));
        fs::remove_dir_all(&ws).ok();
    }
}
