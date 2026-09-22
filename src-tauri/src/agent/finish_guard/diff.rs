//! Diff 解析与审查契约证据行号核对门禁。

use crate::agent::review_contract::{ReviewContract, ReviewObligation};

/// 代码变更与真实 Diff 映射上下文，用于核实审查契约中 evidenced 义务的行号证据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffContext {
    /// 真实 diff 中的修改文件与对应的修改行号范围：file_path -> [(start_line, end_line)]
    pub changed_files: std::collections::HashMap<String, Vec<(u32, u32)>>,
    /// 任务过程中实际产生副作用的文件路径（作为 non-git 或 fallback 校验依据）
    pub all_affected_paths: Vec<String>,
}

impl DiffContext {
    pub fn from_diff_and_paths(raw_diff: Option<&str>, affected_paths: &[String]) -> Self {
        let changed_files = if let Some(diff) = raw_diff {
            parse_unified_diff(diff)
        } else {
            std::collections::HashMap::new()
        };
        Self {
            changed_files,
            all_affected_paths: affected_paths.to_vec(),
        }
    }

    /// 检查引用的文件和行号是否与本次真实 diff 相符
    pub fn matches_diff(&self, ref_path: &str, start_line: u32, end_line: u32) -> bool {
        const LINE_TOLERANCE: u32 = 5;

        // 1. 如果存在解析好的 git diff
        for (diff_path, hunks) in &self.changed_files {
            if file_paths_match(ref_path, diff_path) {
                if hunks.is_empty() {
                    // 文件出现在 diff 中（如新增文件、二进制文件或整文件替换），视为匹配
                    return true;
                }
                for &(hunk_start, hunk_end) in hunks {
                    let eff_start = hunk_start.saturating_sub(LINE_TOLERANCE);
                    let eff_end = hunk_end.saturating_add(LINE_TOLERANCE);
                    if start_line <= eff_end && end_line >= eff_start {
                        return true;
                    }
                }
            }
        }

        // 2. 如果 changed_files 为空（如非 git 仓库或未暂存），回退到工具调用的 affected_paths 检查
        if self.changed_files.is_empty() {
            for affected in &self.all_affected_paths {
                if file_paths_match(ref_path, affected) {
                    return true;
                }
            }
        }

        false
    }
}

/// 路径对比辅助函数（容忍相对路径、基名、前导斜杠与大小写差异）
pub fn file_paths_match(ref_path: &str, diff_path: &str) -> bool {
    let norm_ref = ref_path
        .replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .to_lowercase();
    let norm_diff = diff_path
        .replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .to_lowercase();

    if norm_ref == norm_diff {
        return true;
    }
    if norm_diff.ends_with(&format!("/{}", norm_ref))
        || norm_ref.ends_with(&format!("/{}", norm_diff))
    {
        return true;
    }
    // 若引用未指定任何目录层级（纯文件名，如 "ChatView.vue"），匹配基名
    if !norm_ref.contains('/') {
        if let Some(diff_file_name) = norm_diff.rsplit('/').next() {
            if diff_file_name == norm_ref {
                return true;
            }
        }
    }
    false
}

/// 解析 unified diff 格式文本，提取每个修改文件的行号范围列表
pub fn parse_unified_diff(diff_text: &str) -> std::collections::HashMap<String, Vec<(u32, u32)>> {
    let mut result: std::collections::HashMap<String, Vec<(u32, u32)>> =
        std::collections::HashMap::new();
    let mut current_file: Option<String> = None;

    for line in diff_text.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("diff --git ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 4 {
                let b_path = parts[3].trim_start_matches("b/");
                current_file = Some(b_path.to_string());
                result.entry(b_path.to_string()).or_default();
            }
        } else if trimmed.starts_with("+++ b/") {
            let b_path = &trimmed[6..];
            current_file = Some(b_path.to_string());
            result.entry(b_path.to_string()).or_default();
        } else if trimmed.starts_with("--- 新增文件: ") && trimmed.ends_with(" ---") {
            let path = trimmed
                .trim_start_matches("--- 新增文件: ")
                .trim_end_matches(" ---")
                .trim();
            current_file = Some(path.to_string());
            result
                .entry(path.to_string())
                .or_default()
                .push((1, u32::MAX));
        } else if trimmed.starts_with("【新增二进制产物】文件: ") {
            if let Some(file_part) = trimmed.strip_prefix("【新增二进制产物】文件: ") {
                let path = file_part.split_whitespace().next().unwrap_or("").trim();
                if !path.is_empty() {
                    current_file = Some(path.to_string());
                    result
                        .entry(path.to_string())
                        .or_default()
                        .push((1, u32::MAX));
                }
            }
        } else if trimmed.starts_with("@@ ") {
            if let Some(ref file) = current_file {
                if let Some(hunk_range) = parse_hunk_header(trimmed) {
                    result.entry(file.clone()).or_default().push(hunk_range);
                }
            }
        }
    }
    result
}

/// 解析 @@ -x,y +start,count @@ 形式的 Hunk 头
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let plus_idx = line.find('+')?;
    let after_plus = &line[plus_idx + 1..];
    let end_idx = after_plus.find([' ', '@'])?;
    let spec = after_plus[..end_idx].trim();
    if let Some(comma_idx) = spec.find(',') {
        let start: u32 = spec[..comma_idx].parse().ok()?;
        let count: u32 = spec[comma_idx + 1..].parse().ok()?;
        let end = if count == 0 {
            start
        } else {
            start.saturating_add(count).saturating_sub(1)
        };
        Some((start, end))
    } else {
        let start: u32 = spec.parse().ok()?;
        Some((start, start))
    }
}

/// 从字符串中提取形如 `file:line` 或 `file:start-end` 的证据定位
pub fn parse_file_line_ref(raw: &str) -> Option<(String, u32, u32)> {
    let mut s = raw.trim();
    s = s.trim_matches(|c| c == '`' || c == '*' || c == '"' || c == '\'');

    // 提取 markdown 链接 [link text](url)
    if let (Some(open_bracket), Some(close_bracket), Some(open_paren), Some(close_paren)) =
        (s.find('['), s.find(']'), s.find('('), s.rfind(')'))
    {
        if open_bracket < close_bracket && close_bracket < open_paren && open_paren < close_paren {
            let link_text = &s[open_bracket + 1..close_bracket];
            if let Some(parsed) = parse_file_line_ref(link_text) {
                return Some(parsed);
            }
            let url = &s[open_paren + 1..close_paren];
            if let Some(parsed) = parse_file_line_ref(url) {
                return Some(parsed);
            }
        }
    }

    if let Some(stripped) = s.strip_prefix("file://") {
        s = stripped;
    }

    let (file_part, line_part) = if let Some(hash_pos) = s.find('#') {
        (&s[..hash_pos], &s[hash_pos + 1..])
    } else if let Some(colon_pos) = s.rfind(':') {
        let after_colon = &s[colon_pos + 1..];
        let trimmed_after = after_colon.trim_start().trim_start_matches(['L', 'l']);
        if trimmed_after
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit())
        {
            (&s[..colon_pos], after_colon)
        } else {
            return None;
        }
    } else {
        return None;
    };

    let file_path = file_part.trim().to_string();
    if file_path.is_empty() {
        return None;
    }

    let line_token = line_part
        .split(|c: char| c.is_whitespace() || c == '(' || c == '[' || c == ',')
        .find(|seg| !seg.is_empty())
        .unwrap_or("")
        .trim_end_matches([')', ']', ';', '.']);

    let parts: Vec<&str> = if let Some(idx) = line_token.find("..") {
        vec![&line_token[..idx], &line_token[idx + 2..]]
    } else if let Some(idx) = line_token.find('-') {
        vec![&line_token[..idx], &line_token[idx + 1..]]
    } else if let Some(idx) = line_token.find('~') {
        vec![&line_token[..idx], &line_token[idx + 1..]]
    } else {
        vec![line_token]
    };

    let parse_num = |p: &str| -> Option<u32> {
        let trimmed = p.trim().trim_start_matches(['L', 'l', '#']);
        trimmed.parse::<u32>().ok()
    };

    if parts.len() == 1 {
        let line = parse_num(parts[0])?;
        if line == 0 {
            return None;
        }
        Some((file_path, line, line))
    } else if parts.len() == 2 {
        let start = parse_num(parts[0])?;
        let end = parse_num(parts[1])?;
        if start == 0 || end == 0 {
            return None;
        }
        let (min_l, max_l) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Some((file_path, min_l, max_l))
    } else {
        None
    }
}

/// 核对审查契约中声称 `evidenced` 的义务证据是否真实成立。
///
/// 以**客观物理事实（工作区有无真实代码改动 diff）**分流，解耦"有副作用"与"必改代码"：
/// 1. 有真实 diff → 严格对账：证据行号必须落入本次修改（防虚假交付，原逻辑不变）；
/// 2. 无真实 diff（排查/测试/运维类任务的命令副作用）→ 证据降级为"静态可定位"校验：
///    引用的 file:line 在当前工作区源码中客观存在即可，绝不要求落入不存在的 diff；
/// 3. 无真实 diff 但交付**声称已修改代码** → 照常拦截（撒谎 writer 不因"无 diff"放行）；
/// 4. 义务带宿主可执行验证命令 (verify_command) 的，交由宿主执行器判定，此处不核对；
/// 5. 纯只读任务（side_effect_count == 0）不进入本门禁。
/// 诊断义务判别：义务的 question 与分流判决（triage）下发的
/// investigation_questions 规范化一致即为诊断义务——其证据天然位于
/// "读过的文件"而非 diff 内，走静态定位校验而非 diff 对账。
/// 判别依据是宿主侧持久化的 triage 原单，模型无法伪造归属。
fn obligation_is_diagnostic(question: &str, diagnostic_questions: &[String]) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<String>()
            .to_lowercase()
            .trim_end_matches(['？', '?', '。', '.'])
            .to_string()
    };
    let q = norm(question);
    !q.is_empty() && diagnostic_questions.iter().any(|d| norm(d) == q)
}

pub(crate) fn review_contract_evidence_blockers(
    contract: &ReviewContract,
    side_effect_count: usize,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
    delivery_text: Option<&str>,
    diagnostic_questions: &[String],
) -> Vec<String> {
    if side_effect_count == 0 {
        return Vec::new();
    }
    let Some(ctx) = diff_context else {
        return Vec::new();
    };

    let has_actual_diff = !ctx.changed_files.is_empty();

    if !has_actual_diff {
        // 反撒谎硬拦截：声称修改了代码却拿不出任何真实改动
        if delivery_text
            .map(delivery_claims_code_modification)
            .unwrap_or(false)
        {
            return vec![format!(
                "交付声称已修改/修复代码，但工作区 Git diff 为空、亦无任何真实文件改动记录。请要么真实完成修改后重新交付，要么将\"已修复\"类措辞改为\"已定位根因\"，以诊断/分析报告形式交付。"
            )];
        }
        return diagnostic_static_evidence_blockers(contract, ctx, workspace_root);
    }

    // 当前 diff 覆盖地图：编辑会导致行号漂移，模型引用的证据行号极易变陈旧——
    // 驳回时直接给出真实可用的 file:[起-止] 区间，让模型照图选行号，斩断"陈旧证据→驳回→原样重试"循环
    let diff_map = format!(
        "【当前 diff 实际覆盖区间（证据行号必须从此图中选取）】{}\n",
        {
            let mut parts: Vec<String> = ctx
                .changed_files
                .iter()
                .map(|(f, hs)| {
                    if hs.is_empty() {
                        f.clone()
                    } else {
                        let ranges = hs
                            .iter()
                            .map(|(a, b)| format!("{}-{}", a, b))
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("{} [{}]", f, ranges)
                    }
                })
                .collect();
            parts.sort();
            parts.join("; ")
        }
    );

    let mut blockers = Vec::new();

    for obligation in &contract.obligations {
        let status = obligation.status.trim().to_ascii_lowercase();
        if status != "evidenced" {
            continue;
        }
        // 如果声明了可执行的验证命令/脚本，交由 host_verify 退出码判定
        if obligation.verify_command.is_some() || obligation.verify_script.is_some() {
            continue;
        }

        // 诊断义务（triage 取证单下发的问题）的证据天然位于"读过的文件"，
        // 强行要求落入 diff 属于门型错配——只要引用客观存在于工作区即可成立，
        // 语义成立性由终审逐条核销。修复义务维持 diff 对账。
        let is_diagnostic = obligation_is_diagnostic(&obligation.question, diagnostic_questions);

        let mut has_valid_evidence = false;
        let mut failed_refs = Vec::new();

        for ref_str in &obligation.evidence_refs {
            if let Some((ref_path, start_line, end_line)) = parse_file_line_ref(ref_str) {
                if ctx.matches_diff(&ref_path, start_line, end_line)
                    || (is_diagnostic
                        && ref_statically_locatable(&ref_path, start_line, workspace_root))
                {
                    has_valid_evidence = true;
                    break;
                }
                failed_refs.push(ref_str.trim().to_string());
            } else if looks_like_file_ref(ref_str, workspace_root) {
                // 诊断义务放宽一档：行号损坏（如全角括号注释）但文件客观存在，
                // 也算"找到位置"——行级语义由终审核验，不在此死磕格式。
                let head = ref_str
                    .trim()
                    .trim_matches(|c| matches!(c, '`' | '*' | '"' | '\''))
                    .split([':', '#'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if is_diagnostic && ref_resolves_on_disk(head, workspace_root) {
                    has_valid_evidence = true;
                    break;
                }
                // 形似代码位置声明但行号缺失/非法 → 仍属代码证据域判失败，
                // 防止把"写个路径蒙混"伪装成非代码证据逃避对账
                failed_refs.push(ref_str.trim().to_string());
            }
            // 非代码证据（命令输出/退出码/API 响应/运维状态）不参与 diff 对账：
            // 由 noncode_evidence_skip_reasons 生成 Gate Notice 移交终审语义核验
        }

        // 逐文件修正建议：对每个失败引用，直接给出该文件在 diff 中的实际区间，
        // 把"驳回→模型自行换算行号→可能再次失败"的多轮循环压缩为"照抄建议"的一次性修正
        let fix_suggestions = {
            let mut seen = std::collections::BTreeSet::new();
            let mut suggestions = Vec::new();
            for r in &failed_refs {
                if let Some((ref_path, _, _)) = parse_file_line_ref(r) {
                    for (df, hs) in &ctx.changed_files {
                        if file_paths_match(&ref_path, df) && seen.insert(df.clone()) {
                            let ranges = hs
                                .iter()
                                .map(|(a, b)| format!("{}-{}", a, b))
                                .collect::<Vec<_>>()
                                .join(",");
                            suggestions
                                .push(format!("`{}` 的 diff 实际区间为 [{}] → 可引用 {}:<区间内行号>", ref_path, ranges, df));
                            break;
                        }
                    }
                }
            }
            suggestions.join("\n")
        };

        if !has_valid_evidence {
            if obligation.evidence_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但未提供任何 evidence_refs 证据引用。请提供落入本次 diff 的代码行号证据，或将状态改为 waived 并说明理由。\n{}",
                    obligation.id,
                    obligation.question.trim(),
                    diff_map
                ));
            } else if !failed_refs.is_empty() && is_diagnostic {
                blockers.push(format!(
                    "审查义务 [{}]（{}）为分流下发的诊断问题，免于 diff 对账——但引用的代码证据 `{}` 未能在当前工作区源码中定位（文件须客观存在、行号须在文件行数内）。请核对路径与行号；或引用真实命令观测（`命令 → 结果`/output_id）；或改 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", "),
                ));
            } else if !failed_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的代码证据 `{}` 不在本次真实改动 (diff) 覆盖区间内。\n【直接修法——二选一，不要原样重发 finish】① 在 `review_contract_update` 中把该义务的 evidence_refs 改为下方 diff 区间内的行号（你的编辑已使行号漂移，旧行号一律无效，finish 报告正文里的行号不参与校验，必须更新契约义务本身）；② 若该义务靠命令/测试输出证明 → 改挂 `verify_command` 由宿主执行判分；若属「解释病因」类义务、病灶代码无需改动 → 改 waived 并说明理由。\n{}\n💡 可引用区间：\n{}",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", "),
                    diff_map,
                    fix_suggestions
                ));
            } else if !obligation
                .evidence_refs
                .iter()
                .any(|r| looks_like_operational_evidence(r))
            {
                // 既非代码位置也非可核验观测（裸结论如"已完成优化"）→ 证据不成立，仍然拦截
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的证据 `{}` 缺少具体代码行号，且不构成可核验的观测记录（需 file:line 代码域引用，或指向真实观测的命令/输出句柄/退出码/返回结果形态）。三条出路任选：①补充落入 diff 的行号证据；②引用真实发生的命令观测（形如 `命令 → 结果` 或 output_id 句柄），或挂 `verify_command` 由宿主判分；③改 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    obligation
                        .evidence_refs
                        .iter()
                        .map(|r| r.trim())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            // 其余情况：证据全部为非代码形态且含真实观测 → 本门证据域不存在，
            // 不适用而不拦截：由 Gate Notice 移交终审做语义核验
        }
    }

    blockers
}

/// 交付文本中声称"已修改代码"的关键词（启发式兜底，宁缺毋滥）：
/// 漏判的最坏结果是放行撒谎 writer，误判的最坏结果是误拦诊断任务——
/// 因此只收录强修改语义词，不收录"已更新/已调整"等常见于报告措辞的弱信号。
pub(crate) const MODIFICATION_CLAIM_KEYWORDS: &[&str] = &[
    "已修改", "已修复", "已更正", "已改写", "已重构", "已实现", "已写入", "已新增", "已添加",
    "已删除", "已移除", "已编辑", "修改完成", "修复完成", "改动完成",
];

/// 判定交付文本是否声称"已经修改了代码"。用于无 diff 场景的反撒谎拦截。
pub(crate) fn delivery_claims_code_modification(delivery_text: &str) -> bool {
    let text = delivery_text.trim();
    !text.is_empty() && MODIFICATION_CLAIM_KEYWORDS.iter().any(|kw| text.contains(kw))
}

/// 无真实 diff 时的诊断证据校验：引用能通过 affected_paths 回退，
/// 或 file:line 在当前工作区源码中静态可定位（文件存在且行号在行数范围内）。
fn diagnostic_static_evidence_blockers(
    contract: &ReviewContract,
    ctx: &DiffContext,
    workspace_root: Option<&std::path::Path>,
) -> Vec<String> {
    let mut blockers = Vec::new();

    for obligation in &contract.obligations {
        let status = obligation.status.trim().to_ascii_lowercase();
        if status != "evidenced" {
            continue;
        }
        if obligation.verify_command.is_some() || obligation.verify_script.is_some() {
            continue;
        }

        let mut has_valid_evidence = false;
        let mut failed_refs = Vec::new();

        for ref_str in &obligation.evidence_refs {
            match parse_file_line_ref(ref_str) {
                Some((ref_path, start_line, end_line))
                    if ctx.matches_diff(&ref_path, start_line, end_line)
                        || ref_statically_locatable(&ref_path, start_line, workspace_root) =>
                {
                    has_valid_evidence = true;
                    break;
                }
                // file:line 解析成功但无法定位，或形似代码引用但行号非法 → 代码域判失败
                Some(_) => failed_refs.push(ref_str.trim().to_string()),
                None if looks_like_file_ref(ref_str, workspace_root) => {
                    failed_refs.push(ref_str.trim().to_string());
                }
                // 非代码证据不参与静态定位：由 Gate Notice 移交终审语义核验
                None => {}
            }
        }

        if !has_valid_evidence {
            if obligation.evidence_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但未提供任何证据引用。诊断/排查任务免于 diff 行号对账，但需给出可核验证据（代码域引用需客观存在；命令输出类证据请挂 verify_command 由宿主判分），或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim()
                ));
            } else if !failed_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的证据 `{}` 未能在当前工作区源码中定位。诊断/排查任务免于 diff 行号对账，但代码域引用需客观存在（文件存在且行号在文件行数范围内）。请核对文件路径与行号，或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", ")
                ));
            } else if !obligation
                .evidence_refs
                .iter()
                .any(|r| looks_like_operational_evidence(r))
            {
                // 既非代码位置也非可核验观测（裸结论）→ 证据不成立，仍然拦截
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的证据 `{}` 不构成可核验形态（非 file:line 代码域引用，亦非指向真实观测的命令/输出句柄/退出码/返回结果）。请引用真实发生的命令观测或代码位置，或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    obligation
                        .evidence_refs
                        .iter()
                        .map(|r| r.trim())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            // 其余情况：全部为非代码且含真实观测 → 本门不适用，不拦截
        }
    }

    blockers
}

/// 引用的候选落盘路径：引用原样（绝对/CWD 相对）→ 工作区相对 → 去 "src-tauri/" 前缀 → 工作区上级拼接（仓库根布局）。
fn ref_path_candidates(
    ref_path: &str,
    workspace_root: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(ref_path)];
    if let Some(root) = workspace_root {
        candidates.push(root.join(ref_path));
        if let Some(stripped) = ref_path.strip_prefix("src-tauri/") {
            candidates.push(root.join(stripped));
        }
        if let Some(parent) = root.parent() {
            candidates.push(parent.join(ref_path));
        }
    }
    candidates
}

/// 引用路径在磁盘上客观存在（文件或目录均可——目录引用同样属于代码位置声明）。
fn ref_resolves_on_disk(ref_path: &str, workspace_root: Option<&std::path::Path>) -> bool {
    ref_path_candidates(ref_path, workspace_root)
        .iter()
        .any(|c| c.exists())
}

/// 静态定位：文件存在于候选路径下且 start_line 未超出文件总行数。
/// 纯文件名引用不做递归搜索（避免目录遍历开销与误匹配）。
fn ref_statically_locatable(
    ref_path: &str,
    start_line: u32,
    workspace_root: Option<&std::path::Path>,
) -> bool {
    for candidate in ref_path_candidates(ref_path, workspace_root) {
        let Ok(content) = std::fs::read(&candidate) else {
            continue;
        };
        let total_lines = content.iter().filter(|&&b| b == b'\n').count() as u32
            + u32::from(!content.is_empty() && *content.last().unwrap() != b'\n');
        // 文件找到但行号越界 → 引用无效（防随手编造行号），不再尝试其余候选
        return start_line <= total_lines.max(1);
    }
    false
}

/// 判定证据引用是否属于"代码位置声明"域（区别于命令输出/运维状态等非代码证据）：
/// - `file:line`/`file#line` 语法形态：分隔符前的部分形如裸路径
///   （含路径分隔符且可落盘，或以源码扩展名结尾）→ 代码域；
/// - 无分隔符：整条引用本身是磁盘上可定位的路径 → 代码域（缺行号的裸路径引用）。
/// 其余（`pacman -Qs x 返回空`、`output_id:xx`、`POST /api → 200` 等）→ 非代码证据域。
/// 判定只认物理事实（磁盘存在性/路径形态），不做自然语言猜测。
pub(crate) fn looks_like_file_ref(raw: &str, workspace_root: Option<&std::path::Path>) -> bool {
    let s = raw
        .trim()
        .trim_matches(|c| matches!(c, '`' | '*' | '"' | '\''))
        .trim_end_matches(['。', '，', ',', '；', ';', ')']);
    if s.is_empty() {
        return false;
    }
    if s.contains(':') || s.contains('#') {
        let head = s.split([':', '#']).next().unwrap_or("").trim();
        return head_has_path_shape(head, workspace_root);
    }
    ref_resolves_on_disk(s, workspace_root)
}

/// "裸路径形态"判定：无空白、以源码扩展名结尾，或含路径分隔符且能在磁盘上定位。
fn head_has_path_shape(head: &str, workspace_root: Option<&std::path::Path>) -> bool {
    if head.is_empty() || head.contains(char::is_whitespace) {
        return false;
    }
    const SRC_EXTS: &[&str] = &[
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".vue", ".py", ".go", ".java", ".rb",
        ".c", ".cc", ".cpp", ".h", ".hpp", ".cs", ".php", ".swift", ".kt", ".m",
        ".css", ".scss", ".less", ".html", ".json", ".toml", ".yaml", ".yml",
        ".xml", ".sql", ".sh", ".bash", ".zsh", ".md", ".lock",
    ];
    let lower = head.to_lowercase();
    if SRC_EXTS.iter().any(|e| lower.ends_with(e)) {
        return true;
    }
    if head.contains('/') || head.contains('\\') {
        return ref_resolves_on_disk(head, workspace_root);
    }
    false
}

/// 非代码证据的"可观测形态"判定：引用必须指向一次真实发生的观测——
/// 要么以已知命令/探针二进制开头（可与 side_effects 台账对账），
/// 要么显式携带指向具体对象的句柄（output_id:值 / step_N / 结果词+数字）。
/// 裸结论（"已完成优化"/"验证通过"/"返回正常"）没有可核验对象，不算证据。
fn looks_like_operational_evidence(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return false;
    }
    let lower = s.to_lowercase();
    // ① 命令形态：首个 token 是已知命令/探针二进制（可与 side_effects 台账核验）
    const CMD_HEADS: &[&str] = &[
        "which", "test", "stat", "pacman", "dpkg", "rpm", "apt", "apt-get", "apt-cache",
        "brew", "snap", "flatpak", "pkg", "systemctl", "service", "journalctl",
        "hostnamectl", "timedatectl", "localectl", "loginctl", "resolvectl",
        "pgrep", "pidof", "ps", "curl", "wget", "nc", "ping", "grep", "rg", "diff",
        "cmp", "jq", "docker", "podman", "kubectl", "git", "cargo", "npm", "pnpm",
        "npx", "pytest", "python", "python3", "go", "cat", "ls", "find", "ss",
        "netstat", "lsof", "ip", "md5sum", "sha1sum", "sha256sum", "tar", "unzip",
        "zipinfo", "sed", "awk", "echo", "openssl", "ssh", "scp",
    ];
    let head = s.split_whitespace().next().unwrap_or("");
    let base = head.rsplit('/').next().unwrap_or(head);
    let base = base.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if CMD_HEADS.contains(&base) {
        return true;
    }
    // ② 句柄形态：必须指向具体对象——句柄词 + 紧跟的值/数字才算"有观测对象"
    let has_digit = lower.chars().any(|c| c.is_ascii_digit());
    // output_id:<值> / output_id=<值> / output_id <值>
    if let Some(pos) = lower.find("output_id") {
        let rest = lower[pos + "output_id".len()..].trim_start_matches([':', '=', '：']);
        if !rest.trim().is_empty() {
            return true;
        }
    }
    // step_<数字>（指向具体步骤）
    if let Some(pos) = lower.find("step_") {
        if lower[pos + "step_".len()..]
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit())
        {
            return true;
        }
    }
    // 结果词 + 数字：退出码 0 / exit code 1 / HTTP 200 / 状态码 204 / 返回 200 / → 0
    const RESULT_WORDS: &[&str] = &[
        "退出码", "exit", "http", "状态码", "返回", "→", "->", "=>", "stdout", "stderr",
    ];
    if has_digit && RESULT_WORDS.iter().any(|w| lower.contains(w)) {
        return true;
    }
    false
}

/// 义务的证据引用是否全部为非代码形态且至少一条指向真实观测（命令/句柄/结果）。
/// 此时 diff 对账与静态定位均无判据——机械证据门不适用，移交终审语义核验。
fn obligation_evidence_is_operational_only(
    obligation: &ReviewObligation,
    workspace_root: Option<&std::path::Path>,
) -> bool {
    !obligation.evidence_refs.is_empty()
        && obligation
            .evidence_refs
            .iter()
            .all(|r| parse_file_line_ref(r).is_none() && !looks_like_file_ref(r, workspace_root))
        && obligation
            .evidence_refs
            .iter()
            .any(|r| looks_like_operational_evidence(r))
}

/// 机械证据门的 NotApplicable 交接：返回每条被跳过义务的跳过原因（含义务 id 与证据形态）。
/// 与 review_contract_evidence_blockers 共用同一套判定谓词（obligation_evidence_is_operational_only），
/// 保证"机械门跳过的项"与"终审交接的项"严格一致——Skip 绝不静默。
pub(crate) fn noncode_evidence_skip_reasons(
    contract: &ReviewContract,
    side_effect_count: usize,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
) -> Vec<String> {
    if side_effect_count == 0 || diff_context.is_none() {
        return Vec::new();
    }
    contract
        .obligations
        .iter()
        .filter(|o| o.status.trim().eq_ignore_ascii_case("evidenced"))
        .filter(|o| o.verify_command.is_none() && o.verify_script.is_none())
        .filter(|o| obligation_evidence_is_operational_only(o, workspace_root))
        .map(|o| {
            let refs = o
                .evidence_refs
                .iter()
                .map(|r| r.trim())
                .collect::<Vec<_>>()
                .join("`, `");
            format!(
                "契约义务 [{}]（{}）的证据引用为非代码形态（`{}`），diff/静态定位对账无判据",
                o.id,
                o.question.trim(),
                refs
            )
        })
        .collect()
}

