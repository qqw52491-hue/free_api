//! 审查辅助材料：字段声明/既有写方线索，以及显式证据的范围对账。
//!
//! 自动符号消费点枚举及其逐项裁决义务已退出审查流程；
//! 写方材料只提供定位线索，不产生消费点清单或额外裁决字段。

use crate::agent::utils::runtime_logs::is_runtime_log_dir;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 全仓扫描文件数/单文件字节上限（防爆）
const MAX_SCAN_FILES: usize = 4000;
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// 保留原环境变量兼容：FREE_API_IMPACT_PACK=0|false 关闭写方线索（默认开）。
pub fn write_context_enabled() -> bool {
    !matches!(
        std::env::var("FREE_API_IMPACT_PACK")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "disabled"
    )
}

/// 常见关键字/高频噪声标识符停用表（<4 字符的另由长度门槛过滤）
const STOPWORDS: &[&str] = &[
    "self", "true", "false", "some", "none", "string", "option", "result", "unwrap",
    "clone", "format", "return", "await", "async", "const", "static", "match",
    "else", "loop", "break", "continue", "where", "trait", "impl", "enum", "struct",
    "println", "eprintln", "assert", "debug", "todo", "into", "from", "with",
    "push", "insert", "iter", "collect", "map", "filter", "then", "catch", "expect",
    "default", "serde", "json", "value", "error", "path", "join", "display",
    "warn", "info", "trace", "debug", "info!", "warn!", "error!", "this", "super",
    "null", "undefined", "function", "const", "export", "import", "require", "var",
];

/// 标识符候选：长度 >=4 且（含下划线或多处大写），排除纯小写单词与停用词。
/// 启发式依据：有语义的业务标识符几乎都是 snake_case / camelCase / CamelCase，
/// 而 let/fn/impl 等关键字与 self/ok/err 短词天然被长度与大小写规则滤掉。
fn is_candidate_ident(tok: &str) -> bool {
    if tok.len() < 4 || !tok.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_') {
        return false;
    }
    if !tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    let lower = tok.to_ascii_lowercase();
    if STOPWORDS.contains(&lower.as_str()) {
        return false;
    }
    let has_underscore = tok.contains('_');
    let upper_count = tok.chars().filter(|c| c.is_ascii_uppercase()).count();
    let all_lower = tok.chars().all(|c| !c.is_ascii_uppercase());
    has_underscore || upper_count >= 2 || (all_lower && tok.len() >= 8)
}

/// 递归收集可扫描源码/配置文件（跳过依赖/构建/隐藏目录及已知生成日志）。
/// `agent_logs` 在任意层级都是运行日志目录，保存的历史代码不属于当前消费点；
/// 在共享入口剪枝，同时避免消费点与写方情报读入日志，其他 JSON 配置仍参与扫描。
fn collect_scan_files(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 12 || out.len() >= MAX_SCAN_FILES {
        return;
    }
    const SKIP_DIRS: &[&str] = &[
        "node_modules", "target", ".git", "dist", "build", "out", ".next",
        "coverage", "vendor", "test-artifacts", ".idea", ".vscode",
    ];
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt",
        "vue", "svelte", "sql", "toml", "yaml", "yml", "json", "c", "cpp", "h",
        "hpp", "cs", "rb", "php", "swift", "sh",
    ];
    let Ok(rd) = std::fs::read_dir(root) else { return };
    // 目录序不确定 → pack 内容随之漂移，前缀缓存与"首个声明点命中"都受害；
    // 按文件名排序保证同一代码两次扫描产出逐字节一致。
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_SCAN_FILES {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.')
                && !SKIP_DIRS.contains(&name.as_ref())
                && !is_runtime_log_dir(&name)
            {
                collect_scan_files(&path, out, depth + 1);
            }
        } else if ft.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if EXTS.contains(&ext.as_str()) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size > 0 && size <= MAX_FILE_BYTES {
                    out.push(path);
                }
            }
        }
    }
}

/// 提取 diff 中被赋值的字段/变量名（写侧符号）：
/// + 行里裸 `=` 左侧最近的标识符。排除 let 绑定（新变量非既有状态写方）。
/// 读侧消费点只回答"谁受影响"，写侧清单回答"谁在碰这块状态"——
/// 新增写方是否破坏"唯一写者/冻结锚点"类契约，只有列出既有写方才审得出来。
fn write_targets_from_diff(diff: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in diff.lines() {
        if let Some(body) = line.strip_prefix('+') {
            if !line.starts_with("+++") {
                lines.push(body);
            }
        }
    }
    write_targets_from_lines(lines.iter().map(|l| *l))
}

/// 方案/文本中的写侧符号：方案文本常含 `context.user_goal = goal` 这类
/// 代码片段——计划阶段的"拟写目标"同样破坏唯一写者契约，提前暴露给审查。
fn write_targets_from_text(text: &str) -> Vec<String> {
    write_targets_from_lines(text.lines())
}

fn write_targets_from_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for raw in lines {
        let body = raw.trim();
        if body.starts_with("let ") || body.starts_with("//") || body.starts_with('*') {
            continue;
        }
        let bytes = body.as_bytes();
        for i in 0..bytes.len() {
            if bytes[i] == b'='
                && (i + 1 >= bytes.len() || (bytes[i + 1] != b'=' && bytes[i + 1] != b'>'))
                && (i == 0
                    || (bytes[i - 1] != b'=' && bytes[i - 1] != b'!'
                        && bytes[i - 1] != b'<' && bytes[i - 1] != b'>'))
            {
                // = 左侧末尾最近的标识符即被写目标（self.x / obj.field / field 均可）
                let name: String = body[..i]
                    .trim_end()
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if is_candidate_ident(&name) {
                    out.push(name);
                }
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(4);
    out
}

/// 单字段的写侧情报：声明点（含紧邻不变量注释）+ 全仓既有写方
struct WriteContext {
    /// 声明行 + 其上连续 /// 或 // 注释（不变量/契约声明的语义居住地）
    decl_block: Option<String>,
    /// 既有写方站点 path:line（不含本次改动文件——diff 里已是新写方）
    write_sites: Vec<String>,
}

fn scan_write_context(workspace: &Path, field: &str, exclude: &BTreeSet<String>) -> WriteContext {
    let mut files = Vec::new();
    collect_scan_files(workspace, &mut files, 0);
    let mut ctx = WriteContext { decl_block: None, write_sites: Vec::new() };
    let assign = format!("{} =", field);
    let decl_pat = format!("{}:", field);
    'files: for f in files {
        let rel = f
            .strip_prefix(workspace)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(content) = std::fs::read_to_string(&f) else { continue };
        let lines: Vec<&str> = content.lines().collect();
        for (idx, l) in lines.iter().enumerate() {
            let t = l.trim();
            // 写方：`field =` 或 `.field =`（裸等号词边界；== / => 天然含紧邻字符被排除）
            if t.contains(&assign) || t.contains(&format!(".{}", assign)) {
                if t.starts_with("//") {
                    continue;
                }
                if let Some(pos) = t.find(&assign) {
                    let after = t[pos + assign.len()..].trim_start();
                    if !after.starts_with('=') && !after.starts_with('>') {
                        if !exclude.contains(&rel) {
                            ctx.write_sites.push(format!("{}:{}", rel, idx + 1));
                        }
                    }
                }
            }
            // 声明点：`pub field:` / `field:` 在 struct/初始化语境
            if ctx.decl_block.is_none() && t.contains(&decl_pat) && !t.starts_with("//") {
                let mut block = vec![format!("{}:{} | {}", rel, idx + 1, t)];
                // 向上吸收紧邻注释行（不变量文档的语义居住地）
                let mut j = idx;
                while j > 0 && lines[j - 1].trim_start().starts_with("//") {
                    j -= 1;
                    block.insert(0, lines[j].trim().to_string());
                }
                ctx.decl_block = Some(block.join("\n    "));
            }
            if ctx.write_sites.len() >= 6 && ctx.decl_block.is_some() {
                break 'files;
            }
        }
    }
    ctx.write_sites.truncate(6);
    ctx
}

/// 渲染写侧契约段（纯情报，不产生裁决义务）：新增写方 → 既有写方 + 声明不变量
fn render_write_context(workspace: &Path, fields: &[String], exclude: &BTreeSet<String>) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "【✍️ 写方契约情报】本次改动对以下字段新增/变更了写方——\n\
        下列声明点注释为原始文档（可能过期，非裁决依据）；请对照既有写方清单核实\n\
        \"唯一写者 / 冻结锚点 / 不变量\"类契约是否仍生效，确有破坏再升级为 blocking：\n",
    );
    let mut used = s.chars().count();
    for field in fields {
        let ctx = scan_write_context(workspace, field, exclude);
        if ctx.write_sites.is_empty() && ctx.decl_block.is_none() {
            continue;
        }
        let mut seg = format!("◆ 字段 `{}`：\n", field);
        if let Some(d) = &ctx.decl_block {
            seg.push_str(&format!("    声明点: {}\n", d));
        }
        if ctx.write_sites.is_empty() {
            seg.push_str("    既有写方: （无——本次改动是首个写方）\n");
        } else {
            seg.push_str(&format!("    既有写方: {}\n", ctx.write_sites.join(", ")));
        }
        if used + seg.chars().count() > 900 {
            break;
        }
        used += seg.chars().count();
        s.push_str(&seg);
    }
    s
}

/// 由 git diff 解析改动文件相对路径集（+++ b/path / diff --git a/x b/y）
pub fn changed_files_from_diff(diff: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            out.insert(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(b) = rest.rsplit(" b/").next() {
                if rest.contains(" a/") {
                    out.insert(b.trim().to_string());
                }
            }
        }
    }
    out
}

/// 阶段/终审入口：只提取新增写方并生成字段声明、既有写方线索。
pub fn build_write_context_from_diff(workspace: &Path, diff: &str) -> String {
    render_write_context(workspace, &write_targets_from_diff(diff), &changed_files_from_diff(diff))
}

/// 方案入口：只查询方案代码片段中的拟写字段；目标文件用于排除本次新增写方。
pub fn build_write_context_for_plan(
    workspace: &Path,
    target_files: &[String],
    plan_text: &str,
) -> String {
    let changed = target_files.iter()
        .map(|tf| tf.trim().trim_start_matches("./").replace('\\', "/"))
        .filter(|tf| !tf.is_empty())
        .collect();
    render_write_context(workspace, &write_targets_from_text(plan_text), &changed)
}

/// 证据范围对账：方案把某文件引为证据（读过、用作推断依据）却未列入
/// target_files——「引用 ≠ 管辖」。被引用文件可能依赖被修改对象的语义
/// （如白名单判定依赖 workspace_root 取值），纯集合差集把这类文件显性
/// 摆到审查者面前裁决，而不是靠审查者碰巧注意到。
/// 只认物理事实：引用须能解析出磁盘文件（file:line / file#L10 / 裸路径形态），
/// output_id、命令输出、step_N 等非文件证据不进清单；已在 target_files 中的
/// 文件天然有审查覆盖，也不重复列出。
pub fn evidence_scope_refs(
    target_files: &[String],
    evidence_refs: &[String],
    workspace_root: Option<&Path>,
) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for raw in evidence_refs {
        let path = if let Some((p, _, _)) =
            crate::agent::finish_guard::diff::parse_file_line_ref(raw)
        {
            Some(p)
        } else if crate::agent::finish_guard::diff::looks_like_file_ref(raw, workspace_root) {
            let s = raw
                .trim()
                .trim_matches(|c| matches!(c, '`' | '*' | '"' | '\''))
                .trim_end_matches(['。', '，', ',', '；', ';', ')']);
            Some(s.to_string())
        } else {
            None
        };
        let Some(path) = path else { continue };
        let norm = path.trim().trim_start_matches("./").replace('\\', "/");
        if norm.is_empty() {
            continue;
        }
        // 已列入修改目标的文件不算"引而未改"
        if target_files
            .iter()
            .any(|t| crate::agent::finish_guard::diff::file_paths_match(&norm, t))
        {
            continue;
        }
        out.insert(norm);
    }
    out.into_iter().collect()
}

/// 覆盖校验（通用）：result 的指定字段必须以 ref 前缀逐条裁决 refs 中每一项。
/// refs 为空直接放行；verdicts 缺失或漏项返回违约定因（供 contract_violation 通道使用）。
fn check_named_verdicts(
    refs: &[String],
    result: &Value,
    field: &str,
    label: &str,
) -> Result<(), String> {
    if refs.is_empty() {
        return Ok(());
    }
    let verdicts = result
        .get(field)
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            format!(
                "缺少必填字段 {}：审查包附带了 {} 个{}须逐条裁决",
                field,
                refs.len(),
                label
            )
        })?;
    let texts: Vec<&str> = verdicts.iter().filter_map(|v| v.as_str()).collect();
    let missing: Vec<&str> = refs
        .iter()
        .filter(|r| {
            !texts
                .iter()
                .any(|t| t.trim_start().starts_with(r.as_str()))
        })
        .map(|s| s.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} 漏裁决{}（共 {} 个，缺 {} 个）：{}",
            field,
            label,
            refs.len(),
            missing.len(),
            missing.join(", ")
        ))
    }
}

/// 覆盖校验：result 的 evidence_scope_verdicts 必须以路径前缀逐条裁决
/// refs 中每个"引为证据但未列入修改目标"的文件。
pub fn check_evidence_scope_verdicts(refs: &[String], result: &Value) -> Result<(), String> {
    check_named_verdicts(refs, result, "evidence_scope_verdicts", "证据范围文件")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn ident_filter_rejects_keywords_and_short_names() {
        assert!(!is_candidate_ident("self"));
        assert!(!is_candidate_ident("let"));
        assert!(!is_candidate_ident("foo")); // 纯小写短名噪声
        assert!(is_candidate_ident("workspace_dir"));
        assert!(is_candidate_ident("get_session_tmp_dir"));
        assert!(is_candidate_ident("PathGuard"));
        assert!(is_candidate_ident("orchestrator")); // 长纯小写业务词保留
    }

    #[test]
    fn same_name_source_and_json_no_longer_create_consumer_material() {
        let dir = std::env::temp_dir().join(format!("impact_no_consumers_{}", uuid::Uuid::new_v4()));
        write(&dir, "src/changed.rs", "pub fn record_request() {}\n");
        write(&dir, "src/consumer.rs", "fn run() { record_request(); }\n");
        write(&dir, "config/routing.json", "{\"handler\": \"record_request\"}\n");
        let diff = "diff --git a/src/changed.rs b/src/changed.rs\n+++ b/src/changed.rs\n+pub fn record_request() {}\n";
        assert!(build_write_context_from_diff(&dir, diff).is_empty());
        assert!(build_write_context_for_plan(&dir, &["src/changed.rs".into()], "修改 record_request").is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn generated_logs_excluded_but_source_and_json_writers_preserved_in_both_entries() {
        let dir = std::env::temp_dir().join(format!("impact_writers_{}", uuid::Uuid::new_v4()));
        write(&dir, "src/changed.rs", "fn update_request() {}\n");
        write(&dir, "src/state.rs", "struct C { pub request_state: String }\n");
        write(&dir, "src/writer.rs", "fn update(c: &mut C) { c.request_state = value; }\n");
        write(&dir, "config/agent_logs.json", "{\"script\": \"ctx.request_state = value;\"}\n");
        let log = json!([{"messages": [{"role": "user", "content":
            "旧代码: pub request_state: String; ctx.request_state = value; update_request();"
        }]}]).to_string();
        for path in ["agent_logs/old.json", "src-tauri/agent_logs/old.json"] {
            write(&dir, path, &log);
        }
        let mut files = Vec::new();
        collect_scan_files(&dir, &mut files, 0);
        assert!(files.contains(&dir.join("src/writer.rs")));
        assert!(files.contains(&dir.join("config/agent_logs.json")));
        assert!(!files.iter().any(|p| p.components().any(|c| c.as_os_str() == "agent_logs")));
        let ctx = scan_write_context(&dir, "request_state", &BTreeSet::new());
        assert!(ctx.decl_block.unwrap().contains("src/state.rs:1"));
        assert_eq!(ctx.write_sites, ["config/agent_logs.json:1", "src/writer.rs:1"]);
        let diff = "diff --git a/src/changed.rs b/src/changed.rs\n+++ b/src/changed.rs\n+    ctx.request_state = update_request();\n";
        for material in [
            build_write_context_from_diff(&dir, diff),
            build_write_context_for_plan(&dir, &["src/changed.rs".into()], "ctx.request_state = update_request();"),
        ] {
            for expected in ["src/state.rs:1", "src/writer.rs:1", "config/agent_logs.json:1"] {
                assert!(material.contains(expected), "{material}");
            }
            assert!(!material.contains("agent_logs/"), "{material}");
            assert!(!material.contains("consumer_verdicts"));
            assert!(!material.contains("影响面机械枚举"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 证据范围对账：引为证据但未列入 target_files 的文件必须浮出水面；
    /// 已列入目标的、非文件证据、目标文件自身引用都不进清单。
    #[test]
    fn evidence_scope_diffs_cited_but_unmodified_files() {
        let dir = std::env::temp_dir().join(format!("evidence_scope_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "src/agent/path_guard.rs", "pub fn check() {}\n");
        write(&dir, "src/agent/host_verify.rs", "pub fn run() {}\n");

        let targets = vec!["src/agent/main_loop.rs".to_string()];
        let evidence = vec![
            "src/agent/main_loop.rs:532".to_string(),       // 目标文件自身 → 不算差集
            "src/agent/path_guard.rs:65-120".to_string(),   // 引而未改 → 进清单
            "src/agent/host_verify.rs".to_string(),          // 裸路径引用 → 进清单
            "output_id:abc123".to_string(),                  // 非文件证据 → 排除
            "cargo check → exit 0".to_string(),              // 命令输出 → 排除
        ];
        let refs = evidence_scope_refs(&targets, &evidence, Some(&dir));
        assert_eq!(
            refs,
            vec![
                "src/agent/host_verify.rs".to_string(),
                "src/agent/path_guard.rs".to_string(),
            ],
            "只有引而未改的文件进清单: {:?}",
            refs
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 实弹复现：方案引用 path_guard.rs:144/host_verify.rs:43 但 target 仅 main_loop。
    #[test]
    fn evidence_scope_real_submission() {
        let targets = vec!["src-tauri/src/agent/main_loop.rs".to_string()];
        let evidence = vec![
            "src-tauri/src/agent/main_loop.rs:532-548".to_string(),
            "src-tauri/src/agent/path_guard.rs:144".to_string(),
            "src-tauri/src/agent/host_verify.rs:43".to_string(),
        ];
        let refs = evidence_scope_refs(&targets, &evidence, None);
        assert_eq!(
            refs,
            vec![
                "src-tauri/src/agent/host_verify.rs".to_string(),
                "src-tauri/src/agent/path_guard.rs".to_string(),
            ]
        );
    }

    /// evidence_scope_verdicts 覆盖校验：漏项/缺字段判违约，空 refs 豁免。
    #[test]
    fn evidence_scope_verdict_coverage() {
        let refs = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let ok = json!({"evidence_scope_verdicts": ["src/a.rs => HOLDS: 仅作背景", "src/b.rs => BROKEN: 依赖被改语义"]});
        assert!(check_evidence_scope_verdicts(&refs, &ok).is_ok());
        let missing = json!({"evidence_scope_verdicts": ["src/a.rs => HOLDS: x"]});
        assert!(check_evidence_scope_verdicts(&refs, &missing).unwrap_err().contains("src/b.rs"));
        let no_field = json!({"findings": []});
        assert!(check_evidence_scope_verdicts(&refs, &no_field).is_err());
        assert!(check_evidence_scope_verdicts(&[], &no_field).is_ok());
    }

#[test]
    fn write_context_surfaces_sole_writer_violation() {
        let dir = std::env::temp_dir().join(format!("impact_pack_write_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = &dir;
        // compaction 是唯一合法写方 + 声明点注释写明冻结语义
        write(root, "src/compaction.rs",
              "struct C {\n    /// M1 静态锚点：仅 compaction 写入，字节级冻结供前缀缓存\n    pub user_goal: String,\n}\nfn lift(c: &mut C, g: String) { c.user_goal = g; }\n");
        // 消费点文件（ assembler 读侧）
        write(root, "src/assembler.rs", "fn render(c: &C) { let _ = &c.user_goal; }\n");
        // 改动：main_loop 里新增写方
        let diff = "diff --git a/src/main_loop.rs b/src/main_loop.rs\n+++ b/src/main_loop.rs\n@@\n+    ctx.user_goal = format!(\"{}\", goal);\n";
        let material = build_write_context_from_diff(root, diff);
        assert!(material.contains("写方契约情报"), "pack 应含写侧段:\n{}", material);
        assert!(material.contains("user_goal"));
        assert!(material.contains("compaction.rs"), "应列出既有写方 compaction:\n{}", material);
        assert!(material.contains("静态锚点"), "声明点注释应透出冻结不变量:\n{}", material);
    }

    #[test]
    fn write_targets_skip_let_and_comments() {
        let diff = "+    let new_var = compute();\n+    // foo = bar\n+    ctx.field_x = 1;\n";
        let targets = write_targets_from_diff(diff);
        assert_eq!(targets, vec!["field_x".to_string()]);
    }
}
