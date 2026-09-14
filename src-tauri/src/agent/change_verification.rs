use serde::{Deserialize, Serialize};

const MAX_EVIDENCE_REFS: usize = 8;
const MAX_EVIDENCE_REF_CHARS: usize = 240;
const MAX_WAIVER_REASON_CHARS: usize = 600;

/// 对本会话最近一批真实写入的验证闭合。
///
/// `verified` 需要可审计的证据引用；`waived` 需要明确解释为何不验证。
/// 每次成功副作用都会由 `SandwichContext::record_side_effect` 清空该状态，
/// 因此旧验证不能覆盖后续写入。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeVerification {
    pub status: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub waived_reason: Option<String>,
}

impl ChangeVerification {
    pub fn is_resolved(&self) -> bool {
        matches!(
            normalized_status(self).as_deref(),
            Some("verified" | "waived")
        )
    }

    pub fn summary(&self) -> String {
        match normalized_status(self).as_deref() {
            Some("verified") => format!("已验证：{}", self.evidence_refs.join("；")),
            Some("waived") => format!(
                "已豁免验证：{}",
                self.waived_reason.as_deref().unwrap_or_default()
            ),
            _ => "验证状态无效".to_string(),
        }
    }
}

pub fn apply_change_verification_update(
    current: &mut Option<ChangeVerification>,
    update: ChangeVerification,
) -> Result<(), String> {
    validate_change_verification(&update)?;

    if let Some(existing) = current {
        if existing != &update {
            return Err(
                "当前写入批次已有已闭合验证，不能静默改写。若又发生真实写入，宿主会自动清空旧验证并要求重新验证或豁免。".to_string(),
            );
        }
        return Ok(());
    }

    *current = Some(update);
    Ok(())
}

pub fn change_verification_blocker(
    side_effect_count: usize,
    verification: Option<&ChangeVerification>,
) -> Option<String> {
    if side_effect_count == 0 || verification.is_some_and(ChangeVerification::is_resolved) {
        return None;
    }

    Some(format!(
        "本会话已记录 {} 次真实写入，但最近写入后尚无验证证据或明确豁免",
        side_effect_count
    ))
}

fn validate_change_verification(verification: &ChangeVerification) -> Result<(), String> {
    let status = normalized_status(verification)
        .ok_or_else(|| "change_verification.status 只能是 verified 或 waived。".to_string())?;

    let refs_are_valid = !verification.evidence_refs.is_empty()
        && verification.evidence_refs.len() <= MAX_EVIDENCE_REFS
        && verification.evidence_refs.iter().all(|reference| {
            !reference.trim().is_empty() && reference.chars().count() <= MAX_EVIDENCE_REF_CHARS
        });
    let waiver = verification.waived_reason.as_deref().unwrap_or("").trim();

    match status.as_str() {
        "verified" if refs_are_valid && waiver.is_empty() => Ok(()),
        "verified" => Err(format!(
            "verified 必须包含 1–{} 条非空 evidence_refs，且不能带 waived_reason。",
            MAX_EVIDENCE_REFS
        )),
        "waived"
            if verification.evidence_refs.is_empty()
                && !waiver.is_empty()
                && waiver.chars().count() <= MAX_WAIVER_REASON_CHARS =>
        {
            Ok(())
        }
        "waived" => Err(format!(
            "waived 不能带 evidence_refs，且必须包含 1–{} 个字符的 waived_reason。",
            MAX_WAIVER_REASON_CHARS
        )),
        _ => unreachable!("normalized_status already restricts variants"),
    }
}

fn normalized_status(verification: &ChangeVerification) -> Option<String> {
    let status = verification.status.trim().to_ascii_lowercase();
    matches!(status.as_str(), "verified" | "waived").then_some(status)
}

/// 通用状态探测/断言命令：非编码任务（运维/系统/外部资源）的天然验证手段。
/// 识别依据是"命令本身构成一次客观状态断言"（退出码/输出即判据），而非生态工具名；
/// 纯只读浏览命令（git status / cat / ls）不构成断言，不收录。
/// 成功执行即算验证发生；其中"缺失即证"子集的非零退出同样是状态断言（见
/// is_absence_probe_command）。
pub fn is_state_probe_command(cmd: &str) -> bool {
    let lower = cmd.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const PROBE_PATTERNS: &[&str] = &[
        // 存在性/可定位断言
        "which ",
        "command -v",
        "type -p",
        "whereis ",
        "test -",
        "[ -",
        "[[ -",
        "stat ",
        // 包/安装状态断言
        "pacman -q",
        "dpkg -s",
        "dpkg -l",
        "dpkg-query",
        "rpm -q",
        "apt list --installed",
        "apt-cache policy",
        "brew list",
        "flatpak info",
        "snap list",
        "pkg info",
        // 服务/进程状态断言
        "systemctl status",
        "systemctl is-active",
        "systemctl is-enabled",
        "systemctl is-failed",
        "systemctl show",
        "journalctl",
        "pgrep",
        "pidof",
        "ps -p",
        // 网络/HTTP 断言（-f/-w/-o 断言形态；裸 curl 不收录——它只证明发出了请求）
        "curl -f",
        "curl --fail",
        "curl -o /dev/null",
        "-w '%{http_code}",
        "-w \"%{http_code}",
        "curl -i ",
        "curl -I ",
        "curl --head",
        "wget --spider",
        "nc -z",
        "ping -c",
        // 输出匹配断言（退出码即判据）
        "grep -q",
        "grep --quiet",
        "rg -q",
        "rg --quiet",
        "diff -q",
        "cmp -s",
        "jq -e",
        "jq --exit-status",
        // 容器/编排/系统资源存在性断言
        "docker inspect",
        "kubectl get",
        "hostnamectl",
        "timedatectl",
    ];
    if PROBE_PATTERNS.iter().any(|p| lower.contains(p)) {
        return true;
    }
    // service <name> status 形态
    lower.starts_with("service ") && lower.contains(" status")
}

/// "缺失即断言成立"的纯存在性探针子集：这些命令的非零退出本身就是一次状态观测
/// （卸载后 `which x` 失败、禁用后 `systemctl is-active` 失败都构成有效验证证据）。
/// 输出匹配类（grep -q/diff -q）不在此列——其失败更可能是调试中途而非状态断言。
pub fn is_absence_probe_command(cmd: &str) -> bool {
    let lower = cmd.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const ABSENCE_PATTERNS: &[&str] = &[
        "which ",
        "command -v",
        "type -p",
        "whereis ",
        "test -",
        "[ -",
        "[[ -",
        "stat ",
        "pacman -q",
        "dpkg -s",
        "dpkg -l",
        "dpkg-query",
        "rpm -q",
        "brew list",
        "flatpak info",
        "snap list",
        "pkg info",
        "systemctl is-active",
        "systemctl is-enabled",
        "systemctl is-failed",
        "pgrep",
        "pidof",
        "ps -p",
        "docker inspect",
        "kubectl get",
        "nc -z",
        "ping -c",
        "curl -f",
        "curl --fail",
    ];
    ABSENCE_PATTERNS.iter().any(|p| lower.contains(p))
}

/// 判断给定的 shell 命令是否包含明确的自动化测试、类型检查或构建验证特征
pub fn is_test_or_verification_command(cmd: &str) -> bool {
    let lower = cmd.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let test_patterns = [
        "cargo test",
        "cargo check",
        "npm test",
        "npm run test",
        "npx vitest",
        "npx jest",
        "pnpm test",
        "bun test",
        "deno test",
        "npx tsc",
        "npx vue-tsc",
        "tsc --noemit",
        "vue-tsc --noemit",
        "typecheck",
        "type-check",
        "npm run check",
        "npm run build",
        "cargo build",
        "pytest",
        "python -m unittest",
        "go test",
        "go vet",
        "mvn test",
        "gradle test",
        "ctest",
        "npm run lint",
        "eslint",
        "golangci-lint",
        "mypy",
        "flake8",
        "ruff check",
        "biome check",
    ];
    test_patterns.iter().any(|pattern| lower.contains(pattern))
}

/// 判断文件路径是否为纯 UI 界面、样式、媒体或静态文档文件
/// 🚨 注意：.vue / .tsx / .jsx / .ts 包含可执行脚本与类型声明，绝不能作为纯静态资源无脑豁免！
pub fn is_pure_ui_or_doc_path(path_str: &str) -> bool {
    let lower = path_str.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let non_ui_configs = [
        "package.json",
        "cargo.toml",
        "tsconfig.json",
        "go.mod",
        "pom.xml",
    ];
    if non_ui_configs.iter().any(|cfg| lower.ends_with(cfg)) {
        return false;
    }
    let pure_exts = [
        ".css",
        ".scss",
        ".sass",
        ".less",
        ".html",
        ".htm",
        ".svg",
        ".png",
        ".jpg",
        ".jpeg",
        ".ico",
        ".gif",
        ".md",
        ".markdown",
        ".txt",
        ".json",
        ".yaml",
        ".yml",
    ];
    pure_exts.iter().any(|ext| lower.ends_with(ext))
}

/// 根据受影响的文件列表与工程特征上下文，自适应推导最匹配的验证/类型检查命令：
/// 1. 契约优先：检查 package.json 是否声明了标准的 typecheck / check / test 脚本；
/// 2. 依赖嗅探：
///    - 存在 tsconfig.json 且依赖包含 vue -> npx vue-tsc --noEmit
///    - 存在 tsconfig.json 且依赖包含 react 或 typescript -> npx tsc --noEmit
/// 3. 生态嗅探：
///    - 若为 Rust 项目（Cargo.toml）且包含 .rs 改动 -> cargo check
///    - 若为 Python 项目（pyproject.toml/requirements.txt）且包含 .py 改动 -> pytest
///    - 若为 Go 项目（go.mod）且包含 .go 改动 -> go test ./...
/// 4. 若无法推导任何可执行的类型检查或测试套件 -> 返回 None（由上层自适应跳过豁免）
pub fn infer_adaptive_verification_command(affected_paths: &[String]) -> Option<String> {
    if affected_paths.is_empty() {
        return None;
    }

    let has_web_files = affected_paths.iter().any(|p| {
        p.ends_with(".vue")
            || p.ends_with(".ts")
            || p.ends_with(".tsx")
            || p.ends_with(".jsx")
            || p.ends_with(".js")
    });
    let has_rs_files = affected_paths.iter().any(|p| p.ends_with(".rs"));
    let has_py_files = affected_paths.iter().any(|p| p.ends_with(".py"));
    let has_go_files = affected_paths.iter().any(|p| p.ends_with(".go"));

    let mut candidate_roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidate_roots.push(cwd.clone());
        if let Some(parent) = cwd.parent() {
            candidate_roots.push(parent.to_path_buf());
        }
    }

    for path_str in affected_paths {
        let p = std::path::Path::new(path_str);
        let mut starts = Vec::new();
        if p.is_absolute() {
            if let Some(parent) = p.parent() {
                starts.push(parent.to_path_buf());
            }
        } else {
            for base in &candidate_roots.clone() {
                starts.push(base.join(p));
            }
        }

        for start in starts {
            let mut curr = start.as_path();
            while let Some(parent) = curr.parent() {
                if parent.join("package.json").exists()
                    || parent.join("Cargo.toml").exists()
                    || parent.join("pyproject.toml").exists()
                    || parent.join("go.mod").exists()
                {
                    if !candidate_roots.contains(&parent.to_path_buf()) {
                        candidate_roots.push(parent.to_path_buf());
                    }
                }
                curr = parent;
            }
        }
    }

    // 1. 如果包含 Web/TS/JS 文件，优先在所有候选根目录中寻找匹配 package.json
    if has_web_files {
        for root in &candidate_roots {
            let pkg_json_path = root.join("package.json");
            if pkg_json_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&pkg_json_path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        let scripts = json.get("scripts").and_then(|s| s.as_object());
                        if let Some(s) = scripts {
                            if s.contains_key("typecheck") {
                                return Some("npm run typecheck".to_string());
                            }
                            if s.contains_key("type-check") {
                                return Some("npm run type-check".to_string());
                            }
                            if s.contains_key("check") {
                                return Some("npm run check".to_string());
                            }
                        }

                        let has_tsconfig = root.join("tsconfig.json").exists();
                        if has_tsconfig {
                            let deps = json.get("dependencies").and_then(|d| d.as_object());
                            let dev_deps = json.get("devDependencies").and_then(|d| d.as_object());
                            let has_dep = |name: &str| -> bool {
                                deps.map_or(false, |d| d.contains_key(name))
                                    || dev_deps.map_or(false, |d| d.contains_key(name))
                            };

                            if has_dep("vue") || has_dep("@vue/runtime-core") {
                                return Some("npx vue-tsc --noEmit".to_string());
                            }
                            if has_dep("react") || has_dep("@types/react") || has_dep("typescript") {
                                return Some("npx tsc --noEmit".to_string());
                            }
                        }

                        if let Some(s) = scripts {
                            if let Some(test_cmd) = s.get("test").and_then(|t| t.as_str()) {
                                let test_lower = test_cmd.to_lowercase();
                                if !test_lower.contains("no test specified")
                                    && !test_lower.contains("exit 1")
                                {
                                    return Some("npm test".to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. 如果包含 Rust 文件，寻找 Cargo.toml
    if has_rs_files {
        for root in &candidate_roots {
            if root.join("Cargo.toml").exists() {
                return Some("cargo check".to_string());
            }
        }
    }

    // 3. 如果包含 Python 文件，寻找 pyproject.toml / requirements.txt
    if has_py_files {
        for root in &candidate_roots {
            if root.join("pyproject.toml").exists() || root.join("requirements.txt").exists() {
                return Some("pytest".to_string());
            }
        }
    }

    // 4. 如果包含 Go 文件，寻找 go.mod
    if has_go_files {
        for root in &candidate_roots {
            if root.join("go.mod").exists() {
                return Some("go test ./...".to_string());
            }
        }
    }

    None
}

/// 从工具参数 JSON 中提取可能被修改的文件路径
pub fn extract_file_paths_from_params(params: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let serde_json::Value::Object(map) = params {
        for (k, v) in map {
            let key_lower = k.to_lowercase();
            if (key_lower == "path"
                || key_lower == "target_file"
                || key_lower == "targetfile"
                || key_lower == "file_path"
                || key_lower == "filepath"
                || key_lower == "absolutepath")
                && v.is_string()
            {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        paths.push(s.trim().to_string());
                    }
                }
            }
        }
    }
    paths
}

/// 从工具参数 JSON 中提取 shell 命令字符串
pub fn extract_command_from_params(params: &serde_json::Value) -> Option<String> {
    if let serde_json::Value::Object(map) = params {
        for (k, v) in map {
            let key_lower = k.to_lowercase();
            if (key_lower == "command" || key_lower == "commandline" || key_lower == "cmd")
                && v.is_string()
            {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        return Some(s.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

/// 检查给定路径切片是否全部为纯 UI 或静态文档文件
pub fn are_all_paths_ui_or_doc(paths: &[String]) -> bool {
    if paths.is_empty() {
        return false;
    }
    paths.iter().all(|p| is_pure_ui_or_doc_path(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified() -> ChangeVerification {
        ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-42".to_string()],
            waived_reason: None,
        }
    }

    #[test]
    fn only_verified_or_waived_can_close_real_writes() {
        assert!(apply_change_verification_update(&mut None, verified()).is_ok());

        let waived = ChangeVerification {
            status: "waived".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: Some("用户明确要求只修改，不运行测试。".to_string()),
        };
        assert!(apply_change_verification_update(&mut None, waived).is_ok());

        let invalid = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: None,
        };
        assert!(apply_change_verification_update(&mut None, invalid).is_err());
    }

    #[test]
    fn unresolved_writes_block_finish_but_readonly_work_does_not() {
        assert!(change_verification_blocker(0, None).is_none());
        assert!(change_verification_blocker(1, None).is_some());
        assert!(change_verification_blocker(1, Some(&verified())).is_none());
    }

    #[test]
    fn closed_verification_is_idempotent_but_cannot_be_rewritten() {
        let mut current = Some(verified());
        assert!(apply_change_verification_update(&mut current, verified()).is_ok());

        let rewrite = ChangeVerification {
            status: "waived".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: Some("改成豁免".to_string()),
        };
        assert!(apply_change_verification_update(&mut current, rewrite).is_err());
    }

    #[test]
    fn test_is_test_or_verification_command() {
        assert!(is_test_or_verification_command("cargo test --lib"));
        assert!(is_test_or_verification_command("npm test"));
        assert!(is_test_or_verification_command("npx vitest run"));
        assert!(is_test_or_verification_command("npx vue-tsc --noEmit"));
        assert!(is_test_or_verification_command("pytest tests/test_api.py"));
        assert!(!is_test_or_verification_command("git status"));
        assert!(!is_test_or_verification_command("cat src/main.rs"));
        assert!(!is_test_or_verification_command("ls -la"));
    }

    #[test]
    fn state_probe_commands_count_as_verification() {
        // 非编码任务的天然验证手段：命令本身即客观状态断言（退出码/输出即判据）
        for cmd in [
            "which workbuddy",
            "command -v workbuddy",
            "test -e /opt/WorkBuddy",
            "[ -f /usr/bin/workbuddy ]",
            "stat /opt/WorkBuddy",
            "pacman -Q workbuddy",
            "pacman -Qs workbuddy",
            "dpkg -s workbuddy",
            "rpm -q workbuddy",
            "systemctl status workbuddy.service",
            "systemctl is-active workbuddy",
            "pgrep -x workbuddy",
            "curl -f http://localhost:8790/health",
            "curl -s -o /dev/null -w '%{http_code}' http://x",
            "wget --spider http://x",
            "nc -z 127.0.0.1 8045",
            "ping -c 1 127.0.0.1",
            "journalctl -u foo.service -n 20",
            "docker inspect workbuddy",
            "kubectl get pods",
            "grep -q needle haystack.txt",
            "diff -q a.txt b.txt",
            "service nginx status",
        ] {
            assert!(is_state_probe_command(cmd), "应为状态探针: {cmd}");
        }
        // 纯浏览命令不构成断言
        for cmd in [
            "git status",
            "cat src/main.rs",
            "ls -la",
            "grep foo bar.txt", // 无 -q 的过滤不是断言
            "curl http://x",    // 裸 curl 只证明发了请求
            "tail -f log.txt",
        ] {
            assert!(!is_state_probe_command(cmd), "非断言命令不应算验证: {cmd}");
        }
    }

    #[test]
    fn absence_probes_verify_on_nonzero_exit() {
        // "缺失即证"子集：卸载/禁用/删除任务中探针失败恰是目标状态证据
        for cmd in [
            "which workbuddy",
            "test -e /opt/WorkBuddy",
            "pacman -Q workbuddy",
            "systemctl is-active workbuddy",
            "pgrep -f workbuddy",
        ] {
            assert!(is_absence_probe_command(cmd), "应为缺失探针: {cmd}");
        }
        // 输出匹配类不在此列：失败更可能是调试中途而非状态断言
        assert!(!is_absence_probe_command("grep -q x f.txt"));
        assert!(!is_absence_probe_command("diff -q a b"));
        // 非探针一律不算
        assert!(!is_absence_probe_command("git status"));
    }


    #[test]
    fn test_ui_and_doc_path_detection() {
        assert!(!is_pure_ui_or_doc_path("src/components/AgentDeck.vue"));
        assert!(!is_pure_ui_or_doc_path("src/components/AgentDeck.tsx"));
        assert!(is_pure_ui_or_doc_path("src/style.css"));
        assert!(is_pure_ui_or_doc_path("README.md"));
        assert!(is_pure_ui_or_doc_path("docs/ARCHITECTURE.md"));
        assert!(!is_pure_ui_or_doc_path("src-tauri/src/main.rs"));
        assert!(!is_pure_ui_or_doc_path("src/auth.ts"));
        assert!(!is_pure_ui_or_doc_path("package.json"));
        assert!(!is_pure_ui_or_doc_path("Cargo.toml"));

        assert!(are_all_paths_ui_or_doc(&[
            "src/components/Card.html".to_string(),
            "src/index.css".to_string(),
        ]));
        assert!(!are_all_paths_ui_or_doc(&[
            "src/components/Card.vue".to_string(),
            "src/index.css".to_string(),
        ]));
        assert!(!are_all_paths_ui_or_doc(&[
            "src/components/Card.html".to_string(),
            "src-tauri/src/main.rs".to_string(),
        ]));
    }

    #[test]
    fn test_infer_adaptive_verification_command() {
        let rust_paths = vec!["src-tauri/src/main.rs".to_string()];
        assert_eq!(
            infer_adaptive_verification_command(&rust_paths),
            Some("cargo check".to_string())
        );

        let vue_paths = vec!["src/components/agent/AgentChatStream.vue".to_string()];
        assert_eq!(
            infer_adaptive_verification_command(&vue_paths),
            Some("npx vue-tsc --noEmit".to_string())
        );

        let empty_paths: Vec<String> = Vec::new();
        assert_eq!(infer_adaptive_verification_command(&empty_paths), None);
    }
}
