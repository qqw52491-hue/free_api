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

// 只选择原始输出中的诊断片段，不据此推断失败原因或改变退出码判定。
fn verification_stream_excerpt(text: &str, limit: usize) -> String {
    let text = text.trim();
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut selected = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if ["[error]", "error:", "error[", "error ts", "exception", "failed", "panicked", "caused by:", "fatal:", "not found", "no such file", "失败", "错误"]
            .iter().any(|marker| lower.contains(marker))
        {
            for keep in selected.iter_mut().take((i + 4).min(lines.len())).skip(i.saturating_sub(1)) {
                *keep = true;
            }
        }
    }
    let mut diagnostics = String::new();
    for (line, keep) in lines.iter().zip(selected) {
        if keep {
            diagnostics.push_str(line);
            diagnostics.push('\n');
            if diagnostics.chars().count() >= limit / 2 {
                break;
            }
        }
    }
    if diagnostics.is_empty() {
        let head: String = text.chars().take(limit / 2).collect();
        return format!("[原始输出 {} 字符，以下仅保留开头和末尾，已截断]\n{}\n…\n{}", total, head, tail(text, limit / 2));
    }
    let head: String = text.chars().take(limit / 4).collect();
    let diagnostics: String = diagnostics.chars().take(limit / 2).collect();
    format!("[原始输出 {} 字符，以下为不连续诊断摘录，已截断；不得视为完整日志]\n[诊断标记及相邻行]\n{}\n[输出开头]\n{}\n[输出末尾]\n{}", total, diagnostics, head, tail(text, limit / 4))
}

fn obligation_output_excerpt(stdout: &str, stderr: &str) -> String {
    let mut sections = Vec::new();
    if !stdout.trim().is_empty() {
        sections.push(format!("[stdout]\n{}", verification_stream_excerpt(stdout, 3000)));
    }
    if !stderr.trim().is_empty() {
        sections.push(format!("[stderr]\n{}", verification_stream_excerpt(stderr, 2000)));
    }
    if sections.is_empty() { "（stdout/stderr 均为空）".to_string() } else { sections.join("\n") }
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

impl HostVerifyOutcome {
    /// 检查工具自身无法运行（环境问题，而非代码缺陷）：命令不存在（127/126）、子进程启动失败，
    /// 或 Node 包导出崩溃（如 Node 26 下 vue-tsc 报 ERR_PACKAGE_PATH_NOT_EXPORTED）。
    /// 这类失败不能证明改动有问题，不应打回 finish。有 `error TS` 等真实类型错误输出时不算。
    pub fn tool_unavailable(&self) -> bool {
        if self.passed {
            return false;
        }
        if self.exit_code == "spawn-failed" || self.exit_code == "127" || self.exit_code == "126" {
            return true;
        }
        self.output_tail.contains("ERR_PACKAGE_PATH_NOT_EXPORTED") && !self.output_tail.contains("error TS")
    }
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

/// 逐义务验证结果：命令、退出码、输出尾部。可随宿主交付收据持久化，
/// 但不会因为记录存在就自动认定对后续产物仍然适用。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObligationVerifyResult {
    pub obligation_id: String,
    pub command: String,
    /// sh 启动时的目录；命令内显式 cd 的后续目录由命令本身决定。
    pub working_dir: std::path::PathBuf,
    pub exit_code: String,
    pub output_tail: String,
    pub passed: bool,
}

/// prepare_finish 与 finish 共用同一份失败诊断，避免预检仅剩退出码。
pub fn format_obligation_failure(result: &ObligationVerifyResult) -> String {
    let execution = match result.exit_code.as_str() {
        "fail-closed" => "验证命令未能执行（启动前或启动时失败）",
        "timeout" => "验证命令已启动，执行超时",
        _ => "验证命令已执行，未通过",
    };
    let failure = format!("义务 [{}] 验证失败（退出码 {}；{}）\n验证命令: `{}`\n宿主启动目录: {}\n输出/诊断:\n{}", result.obligation_id, result.exit_code, execution, result.command, result.working_dir.display(), result.output_tail);
    // finish/prepare_finish 直接使用本函数，不能只在旧反馈包装器中附修法。
    let hint = obligation_missing_file_hint(std::slice::from_ref(&failure)).unwrap_or("");
    format!("{failure}{hint}")
}

pub fn obligation_failure_report(failures: &[String]) -> String {
    let report = format!("【⚠️ 交付门禁拦截打回：契约绑定的宿主验证命令未通过，finish 未生效！】\n{}\n（说明：命令退出码即为判定依据，脚本可显式修订；执行结果仍须核实）。\n下一步：依据上述真实诊断定位失败原因（验证命令、运行环境或代码），处理后重跑验证；未验证通过不得宣称交付完成。", failures.join("\n"));
    report
}

/// 快照使用独立私有目录，生命周期覆盖命令执行；不把模型提供的 id 拼进路径。
struct VerificationSnapshot {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    sha256: String,
}

impl VerificationSnapshot {
    fn create(script: &str, sha256: String) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("free-api-verify-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir)?;
        let snapshot = Self { path: dir.join("verify-script"), dir, sha256 };
        std::fs::write(&snapshot.path, script)?;
        Ok(snapshot)
    }
}

impl Drop for VerificationSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) const OBLIGATION_EXECUTOR_VERSION: u32 = 1;

const VERIFY_SCRIPT_ENV: &str = "FREE_API_VERIFY_SCRIPT";

/// 只核对命令是否声明了宿主提供的变量引用，不解析 Shell 控制流，
/// 不把引用存在等同于脚本实际被执行（例如条件分支可能不进入）。
fn references_snapshot(command: &str) -> bool {
    command.contains("${FREE_API_VERIFY_SCRIPT}")
        || command.match_indices("$FREE_API_VERIFY_SCRIPT").any(|(i, name)| {
            !command[i + name.len()..].chars().next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// 【B 路径执行器】执行当前契约命令。普通命令不改写；可选快照通过环境变量
/// FREE_API_VERIFY_SCRIPT 提供，由命令显式引用，不猜文件名/解释器/参数。
/// 回执区分“提供了某版快照”与“Shell 命令退出码”；脚本是否真的在控制流中
/// 被调用、验证是否覆盖题面仍须核验，不能由变量引用或退出 0 推断。
/// 配置/快照损坏返回未执行，不偷偷回退到工作区另一份脚本。
pub async fn run_obligation_verifications(
    workspace: &Path,
    obligations: &[crate::agent::review_contract::ReviewObligation],
) -> Vec<ObligationVerifyResult> {
    use sha2::{Digest, Sha256};
    let mut results = Vec::new();
    for ob in obligations {
        let script = ob.verify_script.as_deref().filter(|s| !s.trim().is_empty());
        let command = ob.verify_command.as_deref().unwrap_or("");
        if command.trim().is_empty() && script.is_none() {
            continue;
        }
        let working_dir = resolve_verify_dir(workspace, command);
        let not_executed = |message: String| ObligationVerifyResult {
            obligation_id: ob.id.clone(), command: command.to_string(),
            working_dir: working_dir.clone(), exit_code: fail_closed(),
            output_tail: message, passed: false,
        };
        let mut snapshot = None;
        if let Some(script) = script {
            let digest = format!("{:x}", Sha256::digest(script.as_bytes()));
            if ob.verify_sha256.as_deref().is_some_and(|expected| expected != digest) {
                results.push(not_executed("脚本快照哈希不匹配（快照损坏或被篡改），未执行验证命令".into()));
                continue;
            }
            if !references_snapshot(command) {
                results.push(not_executed(
                    "已附 verify_script，但 verify_command 未引用宿主快照，未执行验证命令，也未运行工作区同名脚本。可用 review_contract_update 将命令改为例如 sh \"$FREE_API_VERIFY_SCRIPT\" 或 python3 \"$FREE_API_VERIFY_SCRIPT\" --mode strict（解释器与参数自行选择）；若希望按工作区文件或其他普通命令验证，请显式将 verify_script 改为空字符串（同时撤下旧摘要，或改用新的 verify_command）。这里未取得验证结果，不代表业务代码失败。".into()
                ));
                continue;
            }
            match VerificationSnapshot::create(script, digest) {
                Ok(value) => snapshot = Some(value),
                Err(e) => {
                    results.push(not_executed(format!("脚本快照临时文件创建失败，未执行验证命令: {e}")));
                    continue;
                }
            }
        } else if references_snapshot(command) {
            results.push(not_executed("verify_command 引用了 FREE_API_VERIFY_SCRIPT，但未提供 verify_script；未执行，也不继承父进程中的同名变量。请提供快照或改用普通验证命令。".into()));
            continue;
        }
        let mut verify_cmd = tokio::process::Command::new("sh");
        verify_cmd.arg("-c").arg(command).current_dir(&working_dir).kill_on_drop(true);
        // 无快照时也清除此保留变量，防止意外引用宿主旧值。
        verify_cmd.env_remove(VERIFY_SCRIPT_ENV);
        if let Some(ref snapshot) = snapshot {
            verify_cmd.env(VERIFY_SCRIPT_ENV, &snapshot.path);
        }
        crate::agent::tools::local_shell::sanitize_debugger_env(&mut verify_cmd);
        let outcome = tokio::time::timeout(Duration::from_secs(120), verify_cmd.output()).await;
        let (exit_code, mut tail, passed) = match outcome {
            Err(_) => ("timeout".to_string(), "120s 超时，宿主强制终止".to_string(), false),
            Ok(Err(e)) => (fail_closed(), format!("启动失败: {e}"), false),
            Ok(Ok(out)) => {
                let code = out.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                (code.to_string(), obligation_output_excerpt(&stdout, &stderr), code == 0)
            }
        };
        if let Some(ref snapshot) = snapshot {
            tail = format!(
                "[宿主快照提供记录]\n环境变量: {VERIFY_SCRIPT_ENV}\n路径: {}\nSHA-256: {}\n上述记录仅证明命令获提供此快照；不证明分支实际调用了脚本，也不代表用户目标达成。\n{tail}",
                snapshot.path.display(), snapshot.sha256,
            );
        }
        results.push(ObligationVerifyResult {
            obligation_id: ob.id.clone(), command: command.to_string(), working_dir,
            exit_code, output_tail: tail, passed,
        });
        // snapshot 在此释放，成功、非零、启动失败、超时及 future 取消均走 Drop。
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

#[cfg(test)]
mod tool_unavailable_tests {
    use super::HostVerifyOutcome;
    fn o(passed: bool, code: &str, tail: &str) -> HostVerifyOutcome {
        HostVerifyOutcome { passed, exit_code: code.to_string(), output_tail: tail.to_string() }
    }
    #[test]
    fn tool_crash_is_unavailable_but_real_failures_are_not() {
        assert!(o(false, "127", "sh: vue-tsc: not found").tool_unavailable());
        assert!(o(false, "1", "Error [ERR_PACKAGE_PATH_NOT_EXPORTED]: Package subpath './x' is not defined").tool_unavailable());
        assert!(!o(false, "1", "src/a.ts(3,1): error TS2307: Cannot find module './b'").tool_unavailable());
        assert!(!o(false, "2", "1 failing").tool_unavailable());
        assert!(!o(true, "0", "").tool_unavailable());
    }
}


/// 契约验证命令失败原因是「引用的文件不存在」（多为产物实际文件名和契约里的路径不一致）时，
/// 给模型的补充说明——默认文案只说「修复代码」，对改了产物名的情形会误导。
/// 方法可显式修订；这里只披露路径与工作目录的核对方式，不猜测失败原因。
pub fn obligation_missing_file_hint(failure_lines: &[String]) -> Option<&'static str> {
    let missing = failure_lines.iter().any(|l| {
        l.contains("No such file or directory") || l.contains("没有那个文件或目录")
    });
    if missing {
        Some(
            "\n【路径核对】先核对宿主启动目录与最终产物的物理路径。可用 review_contract_update 按原义务 id 更新 verify_command 或 verify_script（包括内嵌脚本里的路径），说明修订原因后核验当前对象；无需另造副本迎合旧文件名。验证方法已允许修订，用户要求与真实检查结果仍交终审核验，不能以修改命令代替验证通过。文件工具的 tmp/ 别名不等于 shell 的相对 tmp/ 目录。",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod missing_file_hint_tests {
    use super::obligation_missing_file_hint as hint;

    #[test]
    fn hints_only_when_file_missing() {
        let miss = vec!["- 义务 [v] 验证命令 `ffprobe a.mp4` 退出码 1:  | stderr: a.mp4: No such file or directory".to_string()];
        assert!(hint(&miss).unwrap().contains("review_contract_update"));
        let other = vec!["- 义务 [t] 验证命令 `cargo test` 退出码 101: tests failed".to_string()];
        assert!(hint(&other).is_none());
        assert!(hint(&[]).is_none());
    }

    #[test]
    fn actual_finish_failure_contains_path_repair_without_loosening_assertions() {
        let result = super::ObligationVerifyResult {
            obligation_id:"excel".into(), command:"python -c \"p='tmp/today_news.xlsx'\"".into(),
            working_dir:"/session".into(), exit_code:"1".into(), passed:false,
            output_tail:"FileNotFoundError: No such file or directory: tmp/today_news.xlsx".into(),
        };
        let feedback = super::format_obligation_failure(&result);
        assert!(feedback.contains("review_contract_update") && feedback.contains("内嵌脚本"));
        assert!(feedback.contains("退出码 1") && feedback.contains("宿主启动目录: /session"));
        assert!(!feedback.contains("硬链接"));
    }
}


#[cfg(test)]
mod feedback_tests {
    use super::*;
    use crate::agent::review_contract::ReviewObligation;
    use crate::agent::context::SandwichContext;

    fn obligation(command: &str) -> ReviewObligation {
        serde_json::from_value(serde_json::json!({
            "id": "unit", "question": "验证测试", "verify_command": command
        })).unwrap()
    }

    #[test]
    fn maven_excerpt_keeps_real_error_before_generic_help_tail() {
        let mut log = "[INFO] downloading dependency\n".repeat(150);
        log.push_str("[ERROR] No tests matching pattern LicenseServiceTest were executed!\n[ERROR] module: adp-common\n");
        log.push_str(&"[INFO] additional build output\n".repeat(150));
        log.push_str("[ERROR] For more information see http://cwiki.apache.org/MojoFailureException\n");
        let excerpt = obligation_output_excerpt(&log, "");
        assert!(excerpt.contains("No tests matching pattern LicenseServiceTest"), "{}", excerpt);
        assert!(excerpt.contains("adp-common"), "{}", excerpt);
        assert!(excerpt.contains("已截断"), "{}", excerpt);
        assert!(excerpt.contains("不得视为完整日志"), "{}", excerpt);
        assert!(excerpt.chars().count() < 3400);
    }

    #[test]
    fn excerpts_preserve_both_streams_and_bound_unicode_output() {
        let log = format!("{}\nerror[E0308]: 错误类型\n{}", "输出\n".repeat(2000), "后续\n".repeat(2000));
        let excerpt = obligation_output_excerpt(&log, "sh: verifier: not found");
        assert!(excerpt.contains("error[E0308]: 错误类型"));
        assert!(excerpt.contains("[stdout]") && excerpt.contains("[stderr]"));
        assert!(excerpt.contains("sh: verifier: not found"));
        assert!(excerpt.chars().count() < 3700);
        assert_eq!(obligation_output_excerpt("", ""), "（stdout/stderr 均为空）");
        let unmarked = verification_stream_excerpt(&"中".repeat(5000), 2000);
        assert!(unmarked.contains("开头和末尾"));
        assert!(unmarked.chars().count() < 2100);
    }

    #[tokio::test]
    async fn executed_failure_returns_command_directory_exit_and_both_streams() {
        let dir = std::env::temp_dir().join(format!("verify-feedback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let command = "printf '%s\\n' '[ERROR] No tests matching pattern were executed'; printf '%s\\n' 'stderr detail' >&2; exit 7";
        let results = run_obligation_verifications(&dir, &[obligation(command)]).await;
        let result = &results[0];
        assert!(!result.passed);
        assert_eq!(result.exit_code, "7");
        assert_eq!(result.working_dir, dir);
        let feedback = format_obligation_failure(result);
        assert!(feedback.contains(command));
        assert!(feedback.contains(&dir.to_string_lossy().to_string()));
        assert!(feedback.contains("验证命令已执行"));
        assert!(feedback.contains("No tests matching pattern") && feedback.contains("stderr detail"));
        let report = obligation_failure_report(&[feedback]);
        assert!(report.contains("验证命令、运行环境或代码"));
        assert!(!report.contains("请真实修复对应代码"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn frozen_hash_failure_stays_fail_closed_and_reports_not_executed() {
        let mut ob = obligation("sh verify.sh");
        ob.verify_script = Some("exit 0".to_string());
        ob.verify_sha256 = Some("incorrect-hash".to_string());
        let results = run_obligation_verifications(&std::env::temp_dir(), &[ob]).await;
        assert!(!results[0].passed);
        assert_eq!(results[0].exit_code, "fail-closed");
        let feedback = format_obligation_failure(&results[0]);
        assert!(feedback.contains("验证命令未能执行"));
        assert!(feedback.contains("脚本快照哈希不匹配"));
    }

    #[tokio::test]
    async fn command_success_remains_success_with_empty_output() {
        let results = run_obligation_verifications(&std::env::temp_dir(), &[obligation("exit 0")]).await;
        assert!(results[0].passed);
        assert_eq!(results[0].exit_code, "0");
        assert_eq!(results[0].output_tail, "（stdout/stderr 均为空）");
    }

    #[test]
    fn separate_obligation_feedback_keeps_all_diagnostics_without_generic_error_wrapper() {
        let mut ctx = SandwichContext::new("sys".into(), "goal".into());
        for id in ["unit", "symptom"] {
            let failure = ObligationVerifyResult {
                obligation_id: id.to_string(), command: format!("mvn -Dtest={}", id),
                working_dir: "/tmp/ws".into(), exit_code: "1".into(),
                output_tail: format!("DIAGNOSTIC_{}\n{}\nEND_{}", id, "真实诊断\n".repeat(1000), id),
                passed: false,
            };
            ctx.add_host_feedback(&obligation_failure_report(&[format_obligation_failure(&failure)]));
        }
        let messages = ctx.assemble_messages();
        for id in ["unit", "symptom"] {
            let msg = messages.iter().find_map(|m| m.content.as_str().filter(|t| t.contains(&format!("DIAGNOSTIC_{}", id)))).unwrap();
            assert!(msg.contains(&format!("END_{}", id)));
            assert!(msg.contains("宿主启动目录"));
            assert!(!msg.contains("格式或执行错误"));
        }
    }
}

#[cfg(test)]
mod snapshot_execution_tests {
    use super::*;
    use crate::agent::review_contract::ReviewObligation;
    use sha2::{Digest, Sha256};

    struct Workspace(std::path::PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("verify 测试 ' {}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    fn obligation(command: &str, script: Option<&str>) -> ReviewObligation {
        serde_json::from_value(serde_json::json!({
            "id":"same-id", "question":"验证当前对象", "verify_command":command,
            "verify_script":script,
            "verify_sha256":script.map(|s| format!("{:x}", Sha256::digest(s.as_bytes())))
        })).unwrap()
    }
    fn snapshot_path(result: &ObligationVerifyResult) -> std::path::PathBuf {
        result.output_tail.lines().find_map(|line| line.strip_prefix("路径: "))
            .expect("快照回执须保留提供的实际路径").into()
    }

    #[tokio::test]
    async fn snapshot_result_ignores_opposite_disk_script_and_preserves_identity() {
        let ws = Workspace::new();
        for (disk_exit, snapshot_exit) in [(0, 9), (7, 0)] {
            std::fs::write(ws.0.join("verify.sh"), format!("echo DISK_SCRIPT_RAN; exit {disk_exit}\n")).unwrap();
            let script = format!("echo SNAPSHOT_SCRIPT_RAN; exit {snapshot_exit}\n");
            let command = "sh \"$FREE_API_VERIFY_SCRIPT\"";
            let results = run_obligation_verifications(&ws.0, &[obligation(command, Some(&script))]).await;
            let r = &results[0];
            assert_eq!(r.exit_code, snapshot_exit.to_string());
            assert_eq!(r.passed, snapshot_exit == 0);
            assert_eq!(r.command, command);
            assert_eq!(r.working_dir, ws.0);
            assert!(r.output_tail.contains("SNAPSHOT_SCRIPT_RAN"));
            assert!(!r.output_tail.contains("DISK_SCRIPT_RAN"));
            assert!(r.output_tail.contains(&format!("{:x}", Sha256::digest(script.as_bytes()))));
            assert!(!snapshot_path(r).exists(), "结束后必须清理快照");
        }
    }

    #[tokio::test]
    async fn unbound_legacy_and_script_only_never_fall_back_or_run_side_effects() {
        let ws = Workspace::new();
        std::fs::write(ws.0.join("verify.sh"), "touch UNEXPECTED; exit 0").unwrap();
        for command in ["sh verify.sh", "", "sh $FREE_API_VERIFY_SCRIPT_OLD"] {
            let result = run_obligation_verifications(&ws.0, &[obligation(command, Some("exit 9"))]).await;
            assert_eq!(result[0].exit_code, "fail-closed");
            assert!(!result[0].passed);
            assert!(result[0].output_tail.contains("未执行"));
            assert!(result[0].output_tail.contains("review_contract_update"));
            assert!(!ws.0.join("UNEXPECTED").exists());
        }
    }

    #[tokio::test]
    async fn ordinary_complex_commands_remain_unchanged_and_need_no_snapshot() {
        let ws = Workspace::new();
        let command = "printf 'BUILD FAILURE\n'; false; printf 'ordinary-command\n'; exit 0";
        let result = run_obligation_verifications(&ws.0, &[obligation(command, None)]).await;
        assert!(result[0].passed, "仅如实反映外层退出码，不用关键词替代语义判断");
        assert_eq!(result[0].command, command);
        assert!(result[0].output_tail.contains("BUILD FAILURE"));
        assert!(result[0].output_tail.contains("ordinary-command"));
        assert!(!result[0].output_tail.contains("宿主快照"));
    }

    #[tokio::test]
    async fn arbitrary_interpreter_arguments_and_shell_composition_are_preserved() {
        let ws = Workspace::new();
        let script = "[ \"$1\" = '空 格' ] && [ \"$2\" = \"引'号\" ] && printf 'ARGS_OK'";
        let command = "if test -d .; then /bin/sh \"${FREE_API_VERIFY_SCRIPT}\" '空 格' \"引'号\"; else exit 8; fi";
        let result = run_obligation_verifications(&ws.0, &[obligation(command, Some(script))]).await;
        assert!(result[0].passed, "{:?}", result[0]);
        assert!(result[0].output_tail.contains("ARGS_OK"));
    }

    #[tokio::test]
    async fn python_snapshot_is_not_forced_through_a_shell_interpreter() {
        let ws = Workspace::new();
        let result = run_obligation_verifications(&ws.0, &[obligation(
            "python3 \"$FREE_API_VERIFY_SCRIPT\" '空 格'",
            Some("import sys\nassert sys.argv[1] == '空 格'\nprint('PYTHON_OK')\n"),
        )]).await;
        assert!(result[0].passed, "{:?}", result[0]);
        assert!(result[0].output_tail.contains("PYTHON_OK"));
    }

    #[tokio::test]
    async fn concurrent_same_obligation_ids_use_separate_snapshots() {
        let ws = Workspace::new();
        let a = [obligation("sh \"$FREE_API_VERIFY_SCRIPT\"", Some("sleep 0.05; echo SNAPSHOT_A"))];
        let b = [obligation("sh \"$FREE_API_VERIFY_SCRIPT\"", Some("sleep 0.05; echo SNAPSHOT_B"))];
        let (a, b) = tokio::join!(run_obligation_verifications(&ws.0, &a), run_obligation_verifications(&ws.0, &b));
        assert!(a[0].passed && b[0].passed);
        assert!(a[0].output_tail.contains("SNAPSHOT_A") && !a[0].output_tail.contains("SNAPSHOT_B"));
        assert!(b[0].output_tail.contains("SNAPSHOT_B") && !b[0].output_tail.contains("SNAPSHOT_A"));
        assert_ne!(snapshot_path(&a[0]), snapshot_path(&b[0]));
        assert!(!snapshot_path(&a[0]).exists() && !snapshot_path(&b[0]).exists());
    }

    #[tokio::test]
    async fn missing_snapshot_or_bad_hash_stops_before_execution() {
        let ws = Workspace::new();
        let mut bad_hash = obligation("touch UNEXPECTED; sh \"$FREE_API_VERIFY_SCRIPT\"", Some("exit 0"));
        bad_hash.verify_sha256 = Some("incorrect".into());
        for ob in [bad_hash, obligation("touch UNEXPECTED; sh \"$FREE_API_VERIFY_SCRIPT\"", None)] {
            let r = run_obligation_verifications(&ws.0, &[ob]).await;
            assert_eq!(r[0].exit_code, "fail-closed");
            assert!(r[0].output_tail.contains("未执行"));
            assert!(!ws.0.join("UNEXPECTED").exists());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_cleans_up_snapshot_and_remains_failure() {
        let ws = Workspace::new();
        let r = run_obligation_verifications(&ws.0, &[obligation(
            "exec sh \"$FREE_API_VERIFY_SCRIPT\"", Some("exec sleep 600"),
        )]).await;
        assert_eq!(r[0].exit_code, "timeout");
        assert!(!r[0].passed);
        assert!(!snapshot_path(&r[0]).exists());
    }

    #[tokio::test]
    async fn spawn_failure_cleans_up_snapshot_and_records_not_executed() {
        let ws = Workspace::new();
        let r = run_obligation_verifications(&ws.0.join("missing"), &[obligation(
            "sh \"$FREE_API_VERIFY_SCRIPT\"", Some("exit 0"),
        )]).await;
        assert_eq!(r[0].exit_code, "fail-closed");
        assert!(!r[0].passed);
        assert!(!snapshot_path(&r[0]).exists());
    }

    #[tokio::test]
    async fn variable_reference_does_not_claim_the_script_was_executed() {
        let ws = Workspace::new();
        let r = run_obligation_verifications(&ws.0, &[obligation(
            "if false; then sh \"$FREE_API_VERIFY_SCRIPT\"; fi", Some("exit 9"),
        )]).await;
        assert!(r[0].passed, "如实记录 Shell 状态，不替模型猜控制流验收");
        assert!(r[0].output_tail.contains("不证明分支实际调用了脚本"));
    }

    #[test]
    fn dropping_snapshot_cleans_private_directory_and_excludes_user_id_from_path() {
        let snapshot = VerificationSnapshot::create("exit 0", "digest".into()).unwrap();
        let path = snapshot.path.clone();
        let dir = snapshot.dir.clone();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        drop(snapshot);
        assert!(!path.exists() && !dir.exists());
    }
}
