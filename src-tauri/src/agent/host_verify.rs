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

/// 逐义务验证结果：命令、退出码、输出尾部。
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
    format!("义务 [{}] 验证失败（退出码 {}；{}）\n验证命令: `{}`\n宿主启动目录: {}\n输出/诊断:\n{}", result.obligation_id, result.exit_code, execution, result.command, result.working_dir.display(), result.output_tail)
}

pub fn obligation_failure_report(failures: &[String]) -> String {
    let report = format!("【⚠️ 交付门禁拦截打回：契约绑定的宿主验证命令未通过，finish 未生效！】\n{}\n（说明：命令退出码即为判定依据，冻结脚本不可修改）。\n下一步：依据上述真实诊断定位失败原因（验证命令、运行环境或代码），处理后重跑验证；未验证通过不得宣称交付完成。", failures.join("\n"));
    match obligation_missing_file_hint(failures) {
        Some(hint) => format!("{}{}", report, hint),
        None => report,
    }
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
        let working_dir = resolve_verify_dir(workspace, command);
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
                        working_dir: working_dir.clone(),
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
                        working_dir: working_dir.clone(),
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
        let working_dir = resolve_verify_dir(workspace, &effective_command);
        let mut verify_cmd = tokio::process::Command::new("sh");
        verify_cmd
            .arg("-c")
            .arg(&effective_command)
            .current_dir(&working_dir)
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
                let excerpt = obligation_output_excerpt(&stdout, &stderr);
                (code.to_string(), excerpt, code == 0)
            }
        };
        results.push(ObligationVerifyResult {
            obligation_id: id,
            command: command.to_string(),
            working_dir,
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


/// 冻结验证命令失败原因是「引用的文件不存在」（多为产物实际文件名和契约里的路径不一致）时，
/// 给模型的补充说明——默认文案只说「修复代码」，对改了产物名的情形会误导。
/// 冻结机制本身不变（命令仍不可改），这里只点明合法出路。
pub fn obligation_missing_file_hint(failure_lines: &[String]) -> Option<&'static str> {
    let missing = failure_lines.iter().any(|l| {
        l.contains("No such file or directory") || l.contains("没有那个文件或目录")
    });
    if missing {
        Some(
            "\n【若失败原因是验证命令引用的文件不存在】多半是产物实际文件名与契约里的路径不一致。检查本身冻结不能改，但**命令里的文件路径可以改成真实产物路径**：①用 review_contract_update 只把该义务 verify_command 里的路径换成实际交付文件（其余参数一字不动），并保证 artifacts 登记的是该最终路径；②或让产物出现在原路径（重命名、`ln` 硬链接，避免复制大文件）；③若该义务整体不再适用，标 waived 并写明理由、新增新义务。请如实说明，不要为通过验证而造假。",
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
        assert!(feedback.contains("冻结脚本哈希不匹配"));
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
