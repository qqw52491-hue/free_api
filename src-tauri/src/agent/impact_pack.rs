//! 影响面机械枚举包（Impact Pack）
//!
//! 背景教训（实测）：方案/阶段审查连续两轮把 `path_guard.rs` 判定块引为证据仍放行——
//! 审查者拿到材料却没人强制它回答"改动后原假设是否仍成立"。
//! 本模块把"要不要查耦合"从模型意志降级为协议义务：
//!   宿主机械枚举改动符号的跨文件消费点（substring + 词边界匹配，不依赖 rg 外部命令），
//!   注入审查证据包；消费点 refs 经 params 透传，派发方校验 `consumer_verdicts`
//!   必须逐条覆盖（缺一条 = contract_violation，走既有 failover 通道）。
//!
//! 已知边界：只能发现"源码里可被词边界搜索到的"静态消费点；运行时反射、
//! 字符串拼接出的引用、数据库列名、配置文件键不在射程内——这类耦合仍需人工/测试兜底。

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 最多枚举的改动符号数（防 pack 膨胀）
const MAX_SYMBOLS: usize = 10;
/// 每个符号最多展示的消费点行数
const MAX_HITS_PER_SYMBOL: usize = 4;
/// 消费点 refs 总数上限（= 裁决义务上限；再大模型容易漏项且校验噪声大）
// 上限 8：契约要求逐条裁决全覆盖，16 点实测三个模型 5 次派发全 contract_violation
//（3.1pro 最好漏 6、k3 最差漏 15）——超过 ~8 点模型覆盖率崩塌，审查预算空转烧光。
// 清单按 def-site 优先排序，截断尾部低值点不削弱义务严肃性。
pub const MAX_CONSUMER_REFS: usize = 8;
/// 渲染小节总字符上限
const MAX_PACK_CHARS: usize = 1800;
/// 单条消费点行截断长度
const MAX_SNIPPET_CHARS: usize = 120;
/// 全仓扫描文件数/单文件字节上限（防爆）
const MAX_SCAN_FILES: usize = 4000;
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// 环境变量总开关：FREE_API_IMPACT_PACK=0|false 关闭注入与校验（默认开）
pub fn impact_pack_enabled() -> bool {
    !matches!(
        std::env::var("FREE_API_IMPACT_PACK")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "disabled"
    )
}

/// 影响面包：渲染好的提示词小节 + 需要逐条裁决的消费点引用
#[derive(Debug, Clone, Default)]
pub struct ImpactPack {
    pub section: String,
    pub refs: Vec<String>,
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

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

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

/// 从一行源码中提取候选标识符（词边界切分）
fn idents_in_line(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_char(bytes[i]) && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            while i < bytes.len() && is_ident_char(bytes[i]) {
                i += 1;
            }
            let tok = &line[start..i];
            if is_candidate_ident(tok) {
                out.push(tok.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// 从 diff 文本提取改动符号：仅 +/- 行（跳过 +++/--- 头），按出现频次排序取前 N
fn symbols_from_diff(diff: &str) -> Vec<String> {
    let mut freq: BTreeMap<String, u32> = BTreeMap::new();
    for line in diff.lines() {
        let body = match line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            Some(b) if !line.starts_with("+++") && !line.starts_with("---") => b,
            _ => continue,
        };
        for tok in idents_in_line(body) {
            *freq.entry(tok).or_insert(0) += 1;
        }
    }
    rank_symbols(freq)
}

/// 从目标文件内容提取符号：pub 定义项 + `use` 导入的叶子名（方案期无 diff 时的代用品）
fn symbols_from_file_content(content: &str) -> Vec<String> {
    let mut freq: BTreeMap<String, u32> = BTreeMap::new();
    for line in content.lines() {
        let t = line.trim_start();
        // `use crate::agent::path_guard::{a, b}` / `use foo::Bar` —— 叶子名即被消费的外部符号
        if t.starts_with("use ") || t.starts_with("pub use ") {
            for seg in t.split(|c: char| c == ':' || c == '{' || c == '}' || c == ',' || c == ';' || c.is_whitespace()) {
                let seg = seg.trim();
                if is_candidate_ident(seg) {
                    *freq.entry(seg.to_string()).or_insert(0) += 1;
                }
            }
            continue;
        }
        // pub fn/struct/enum/trait/const/type 定义名：本文件的对外契约面
        for marker in ["pub fn ", "pub struct ", "pub enum ", "pub trait ", "pub const ", "pub type ", "fn "] {
            if let Some(rest) = t.find(marker).map(|i| &t[i + marker.len()..]) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if is_candidate_ident(&name) {
                    *freq.entry(name).or_insert(0) += 3; // 定义点加权：对外契约更值得枚举
                }
            }
        }
    }
    rank_symbols(freq)
}

fn rank_symbols(freq: BTreeMap<String, u32>) -> Vec<String> {
    let mut v: Vec<(String, u32)> = freq.into_iter().collect();
    // 频次降序，同频次按长度降序（长名更可能是特异业务符号）
    v.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(&b.0)));
    v.into_iter().take(MAX_SYMBOLS).map(|(s, _)| s).collect()
}

/// 递归收集可扫描源码文件（跳过依赖/构建/隐藏目录）
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
    for entry in rd.flatten() {
        if out.len() >= MAX_SCAN_FILES {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
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

/// 在单行中做词边界 substring 匹配（名字两侧不得是标识符字符）
fn line_contains_ident(line: &str, sym: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = sym.as_bytes();
    if needle.is_empty() || bytes.len() < needle.len() {
        return false;
    }
    for start in 0..=bytes.len() - needle.len() {
        if &bytes[start..start + needle.len()] == needle {
            let left_ok = start == 0 || !is_ident_char(bytes[start - 1]);
            let right_idx = start + needle.len();
            let right_ok = right_idx >= bytes.len() || !is_ident_char(bytes[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
    }
    false
}

struct ConsumerHit {
    rel_path: String,
    line_no: usize,
    snippet: String,
    /// 语义定义行（pub/fn/impl/struct 等）优先展示——耦合的"语义居住地"
    is_def_site: bool,
}

/// 测试/fixture 文件不计入影响面消费点——测试代码的破坏由测试套件本身兜底验证，
/// 让审查为 mock-server/fixture/_tests.rs 逐条裁决只会稀释契约、烧光审查预算
///（实测 16 点清单混入 4 条测试路径，3 个模型 5 次派发全部 contract_violation）。
fn is_test_or_fixture(rel: &str) -> bool {
    let p = rel.to_lowercase();
    p.starts_with("test/")
        || p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/fixtures/")
        || p.ends_with("_test.rs")
        || p.ends_with("_tests.rs")
        || p.ends_with(".test.ts")
        || p.ends_with(".spec.ts")
        || p.ends_with(".test.mjs")
        || p.ends_with(".spec.mjs")
}

/// 全仓扫描某符号的消费点；exclude 为改动文件相对路径集（改动文件自身命中不算下游）
fn scan_consumers(workspace: &Path, sym: &str, exclude: &BTreeSet<String>) -> Vec<ConsumerHit> {
    let mut files = Vec::new();
    collect_scan_files(workspace, &mut files, 0);
    let mut hits = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(workspace)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        if exclude.contains(&rel) || is_test_or_fixture(&rel) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&f) else { continue };
        for (idx, line) in content.lines().enumerate() {
            if line_contains_ident(line, sym) {
                let t = line.trim();
                // 注释行降权但仍收录（注释引用也是耦合证据）；纯字符串命中不过滤（宁多勿漏）
                let is_def = t.contains("fn ") || t.contains("pub ") || t.contains("impl ")
                    || t.contains("struct ") || t.contains("const ") || t.contains("interface ")
                    || t.contains("def ") || t.contains("class ");
                hits.push(ConsumerHit {
                    rel_path: rel.clone(),
                    line_no: idx + 1,
                    snippet: t.chars().take(MAX_SNIPPET_CHARS).collect(),
                    is_def_site: is_def,
                });
                if hits.len() >= MAX_HITS_PER_SYMBOL * 4 {
                    break; // 单文件命中过多时先截断再排序精选
                }
            }
        }
    }
    // 定义点优先，其次按路径稳定排序；每符号展示上限
    hits.sort_by(|a, b| {
        b.is_def_site
            .cmp(&a.is_def_site)
            .then(a.rel_path.cmp(&b.rel_path))
            .then(a.line_no.cmp(&b.line_no))
    });
    // 锚定副作用修正（实测）：同一文件对同一符号最多保留 2 条命中——
    // 4 条相同的 `advance_run_state(` 调用行曾把整份清单挤成单文件流水账。
    // 排序后 def 站点天然排在前列，按序保留即实现"is_def_site 优先"。
    const MAX_HITS_PER_FILE: usize = 2;
    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
    hits.retain(|h| {
        let n = per_file.entry(h.rel_path.clone()).or_insert(0);
        *n += 1;
        *n <= MAX_HITS_PER_FILE
    });
    hits.truncate(MAX_HITS_PER_SYMBOL);
    hits
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

/// 渲染影响面包并返回实际展示的消费点 refs——【所见即所裁】：
/// pack 文本受 MAX_PACK_CHARS 预算约束，超出时按整符号段取舍（不拦腰斩段），
/// refs 只含真正展示给审查员的点，保证 consumer_verdicts 义务裁的都是看得见的点。
/// （此前拦腰截断导致尾部消费点不可见仍被契约要求裁决，三模型连炸 5 次的根因）
fn render_pack(by_symbol: &BTreeMap<String, Vec<ConsumerHit>>) -> (String, Vec<String>) {
    let mut s = String::from(
        "【🧨 影响面机械枚举（宿主静态扫描结果，非模型推断）】\n\
        本次改动触及的符号存在以下跨文件消费点——审查义务不是评估方案本身，\n\
        而是逐点核实\"改动后该处原假设是否仍成立\"（例如两个条件谓词开始同址重叠、\n\
        语义收束导致判定顺序改变、签名变化影响调用链）：\n",
    );
    // 义务段尾部也要计入预算——先渲染占位再回填实际数量
    let obligation_fmt = |n: usize| {
        format!(
            "【硬义务】输出 JSON 必须含 \"consumer_verdicts\": [\"<path:line> => HOLDS|BROKEN|NA: 一句话理由\", ...]，\n\
            覆盖上面列出的全部 {} 个消费点；确认无关的点可判 NA 但禁止漏项（缺项按 contract_violation 打回重修）。\n\
            HOLDS = 核实该消费点在改动后行为不变；BROKEN = 改动破坏该处假设（须同步产出 blocking finding）。\n\
            本清单为宿主机械枚举的下限，仅保证列出项有据可查，不替代你主动探查清单外的耦合点。",
            n
        )
    };
    let mut shown_refs: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut dropped = 0usize;
    for (sym, hits) in by_symbol {
        if hits.is_empty() {
            continue;
        }
        let mut section = format!("◆ 符号 `{}` 的下游消费点：\n", sym);
        for h in hits {
            section.push_str(&format!("  - {}:{} | {}\n", h.rel_path, h.line_no, h.snippet));
        }
        // 预算判断：正文 + 该段 + 义务尾部（按最坏数字位数估算）不超限才整段采纳
        if s.chars().count() + section.chars().count() + 450 > MAX_PACK_CHARS {
            dropped += hits.len();
            continue;
        }
        s.push_str(&section);
        for h in hits {
            let r = format!("{}:{}", h.rel_path, h.line_no);
            if seen.insert(r.clone()) {
                shown_refs.push(r);
            }
        }
    }
    if dropped > 0 {
        s.push_str(&format!("（另有 {} 个低优先消费点因篇幅未列示，仅作参考不要求裁决）\n", dropped));
    }
    s.push_str(&obligation_fmt(shown_refs.len()));
    (s, shown_refs)
}

fn build(workspace: &Path, changed_files: BTreeSet<String>, symbols: Vec<String>) -> ImpactPack {
    if symbols.is_empty() {
        return ImpactPack::default();
    }
    let mut by_symbol: BTreeMap<String, Vec<ConsumerHit>> = BTreeMap::new();
    // 聚合级去重（跨符号）：同一文件内 snippet 完全相同的行是同一条证据——
    // 既压掉"多符号共命中同一行"的重复列示，也压掉同文件同文调用行的堆叠。
    // 键含 rel_path：跨文件同文行仍是独立消费点，不误删（宁多勿漏）。
    let mut seen_snippets: BTreeSet<(String, String)> = BTreeSet::new();
    for sym in symbols {
        let hits = scan_consumers(workspace, &sym, &changed_files);
        let mut kept: Vec<ConsumerHit> = Vec::with_capacity(hits.len());
        for h in hits {
            if seen_snippets.insert((h.rel_path.clone(), h.snippet.clone())) {
                kept.push(h);
            }
        }
        by_symbol.insert(sym, kept);
    }
    let (section, refs) = render_pack(&by_symbol);
    if refs.is_empty() {
        return ImpactPack::default();
    }
    ImpactPack { section, refs }
}

/// 阶段审查入口：有真实累计 diff 时按改动行提取符号
pub fn build_from_diff(workspace: &Path, diff: &str) -> ImpactPack {
    let changed = changed_files_from_diff(diff);
    let symbols = symbols_from_diff(diff);
    build(workspace, changed, symbols)
}

/// 方案审查入口：尚无 diff，符号源三路并采——
///   ① target_files 的 pub 契约面 + use 依赖边（被改文件的对外耦合）
///   ② 方案文本（plan_summary/root_cause/assumptions）中出现的标识符
///      ——实测教训：真正的耦合常是"值流"（局部变量喂给谓词函数），
///      不是 use 边也不是 pub 项，但模型写方案时几乎必然点名它
///      （workspace_leak 方案 root_cause 就明写 workspace_dir/session_tmp_dir）。
///      文本挖出的符号权重 +2（被点名 = 语义相关性的强先验）。
pub fn build_for_targets(
    workspace: &Path,
    target_files: &[String],
    plan_text: &str,
) -> ImpactPack {
    let mut changed: BTreeSet<String> = BTreeSet::new();
    let mut freq: BTreeMap<String, u32> = BTreeMap::new();
    for tf in target_files {
        let rel = tf.trim().trim_start_matches("./").replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        changed.insert(rel.clone());
        let path = workspace.join(&rel);
        if let Ok(content) = std::fs::read_to_string(&path) {
            for s in symbols_from_file_content(&content) {
                *freq.entry(s).or_insert(0) += 1;
            }
        }
    }
    for line in plan_text.lines() {
        for tok in idents_in_line(line) {
            *freq.entry(tok).or_insert(0) += 2;
        }
    }
    build(workspace, changed, rank_symbols(freq))
}

/// 覆盖校验：result 的 consumer_verdicts 必须以 "path:line" 前缀逐条裁决 refs 中每个消费点。
/// refs 为空直接放行；verdicts 缺失或漏项返回违约定因（供 contract_violation 通道使用）。
pub fn check_consumer_verdicts(refs: &[String], result: &Value) -> Result<(), String> {
    if refs.is_empty() {
        return Ok(());
    }
    let verdicts = result
        .get("consumer_verdicts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            format!(
                "缺少必填字段 consumer_verdicts：影响面包附带了 {} 个消费点须逐条裁决",
                refs.len()
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
            "consumer_verdicts 漏裁决影响面消费点（共 {} 个，缺 {} 个）：{}",
            refs.len(),
            missing.len(),
            missing.join(", ")
        ))
    }
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
    fn symbols_extracted_only_from_changed_diff_lines() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n\
                    +++ b/src/a.rs\n\
                    +let workspace_dir = get_session_tmp_dir(session_id);\n\
                    -let workspace_dir = std::env::current_dir().unwrap();\n\
                     context_line_unchanged\n";
        let syms = symbols_from_diff(diff);
        assert!(syms.contains(&"workspace_dir".to_string()));
        assert!(syms.contains(&"get_session_tmp_dir".to_string()));
        assert!(!syms.contains(&"context_line_unchanged".to_string()));
    }

    #[test]
    fn word_boundary_match_avoids_substring_false_positives() {
        assert!(line_contains_ident("let x = workspace_dir.clone();", "workspace_dir"));
        assert!(!line_contains_ident("let workspace_dir_extra = 1;", "workspace_dir"));
        assert!(!line_contains_ident("my_workspace_dir", "workspace_dir"));
    }

    #[test]
    fn pack_enumerates_cross_file_consumers() {
        let dir = std::env::temp_dir().join(format!("impact_pack_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "src/agent/main_loop.rs", "use crate::agent::path_guard::get_session_tmp_dir;\nfn run() {}\n");
        write(&dir, "src/agent/path_guard.rs", "pub fn get_session_tmp_dir(sid: &str) -> PathBuf { todo!() }\npub fn check(workspace_root: &Path) {}\n");
        write(&dir, "src/agent/finalization.rs", "fn fin() { let g = get_session_tmp_dir(\"x\"); }\n");

        let pack = build_for_targets(
            &dir,
            &["src/agent/main_loop.rs".to_string()],
            "修复：workspace_dir 回落到 get_session_tmp_dir",
        );
        assert!(!pack.refs.is_empty(), "应枚举出跨文件消费点");
        assert!(
            pack.refs.iter().any(|r| r.starts_with("src/agent/path_guard.rs:")),
            "path_guard 定义点必须在 refs 中: {:?}",
            pack.refs
        );
        assert!(
            pack.refs.iter().any(|r| r.starts_with("src/agent/finalization.rs:")),
            "finalization 调用点必须在 refs 中: {:?}",
            pack.refs
        );
        // 改动文件自身命中不列入
        assert!(!pack.refs.iter().any(|r| r.starts_with("src/agent/main_loop.rs:")));
        assert!(pack.section.contains("consumer_verdicts"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 锚定副作用回归：同一文件对同一符号的 4 条命中，经"每文件≤2"去重后
    /// refs 中该文件仅剩 2 条（实测 4 条相同 advance_run_state( 调用行曾挤占整份清单）。
    #[test]
    fn same_file_hits_capped_at_two_per_symbol() {
        let dir = std::env::temp_dir().join(format!("impact_pack_dedup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/agent/consumer.rs",
            "fn a() { advance_run_state_x(1); }\n\
             fn b() { advance_run_state_x(2); }\n\
             fn c() { advance_run_state_x(3); }\n\
             fn d() { advance_run_state_x(4); }\n",
        );
        write(
            &dir,
            "src/agent/changed.rs",
            "pub fn advance_run_state_x(n: i32) { let _ = n; }\n",
        );

        let diff = "diff --git a/src/agent/changed.rs b/src/agent/changed.rs\n\
                    +++ b/src/agent/changed.rs\n\
                    +pub fn advance_run_state_x(n: i32) -> i32 { n + 1 }\n";
        let pack = build_from_diff(&dir, diff);
        let file_refs: Vec<&String> = pack
            .refs
            .iter()
            .filter(|r| r.starts_with("src/agent/consumer.rs:"))
            .collect();
        assert_eq!(
            file_refs.len(),
            2,
            "单文件同符号命中须压到 2 条: {:?}",
            pack.refs
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verdict_coverage_requires_every_ref() {
        let refs = vec!["a/b.rs:10".to_string(), "c/d.rs:20".to_string()];
        let ok = json!({"consumer_verdicts": ["a/b.rs:10 => HOLDS: fine", "c/d.rs:20 => NA: unrelated"]});
        assert!(check_consumer_verdicts(&refs, &ok).is_ok());

        let missing_one = json!({"consumer_verdicts": ["a/b.rs:10 => HOLDS: fine"]});
        let err = check_consumer_verdicts(&refs, &missing_one).unwrap_err();
        assert!(err.contains("c/d.rs:20"), "漏项须点名: {}", err);

        let no_field = json!({"findings": []});
        assert!(check_consumer_verdicts(&refs, &no_field).is_err());

        // 空 refs 豁免
        assert!(check_consumer_verdicts(&[], &no_field).is_ok());
    }

    #[test]
    fn target_file_pub_surface_and_use_edges_become_symbols() {
        let content = "use crate::agent::path_guard::get_session_tmp_dir;\n\
                       use std::collections::HashMap;\n\
                       pub fn resolve_workspace_dir() {}\n\
                       fn helper() {}\n";
        let syms = symbols_from_file_content(content);
        assert!(syms.contains(&"resolve_workspace_dir".to_string()));
        assert!(syms.contains(&"get_session_tmp_dir".to_string()));
    }
}
