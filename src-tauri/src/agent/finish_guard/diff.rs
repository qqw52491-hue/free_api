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

/// 识别历史诊断问题，只用于引用定位失败时的说明，不决定证据是否充分。
pub(crate) fn obligation_is_diagnostic(question: &str, diagnostic_questions: &[String]) -> bool {
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

/// 校验已申报义务中可机械核对的引用：代码位置可在 diff 或磁盘上定位。
/// 带验证命令的义务由宿主执行器处理；其他引用移交终审，不按命令名或文字外观裁决。
/// 纯只读且无副作用的任务不进入本门。
pub(crate) fn review_contract_evidence_blockers(
    contract: &ReviewContract,
    side_effect_count: usize,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
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
        return diagnostic_static_evidence_blockers(contract, ctx, workspace_root);
    }

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

        // 历史诊断问题的身份只影响定位失败时的错误说明。
        let is_diagnostic = obligation_is_diagnostic(&obligation.question, diagnostic_questions);

        let mut has_valid_evidence = false;
        let mut failed_refs = Vec::new();

        for ref_str in &obligation.evidence_refs {
            if let Some((ref_path, start_line, end_line)) = parse_file_line_ref(ref_str) {
                if ctx.matches_diff(&ref_path, start_line, end_line) {
                    has_valid_evidence = true;
                    break;
                }
                // 证据形状分层（物种二分）：file:line 能在磁盘上真实定位 = 具体、
                // 可审计、非凭空编造的代码证据——机械门到此为止。它「该不该落在
                // diff 里」是语义判断（诊断义务的证据天然在未修改文件里；模型
                // 对取证单做同义改写时逐字匹配靠不住），移交终审逐条核销。
                if ref_statically_locatable(&ref_path, start_line, workspace_root) {
                    has_valid_evidence = true;
                    break;
                }
                if embedded_ref_locatable(ref_str, ctx, workspace_root) {
                    has_valid_evidence = true;
                    break;
                }
                failed_refs.push(ref_str.trim().to_string());
            } else if looks_like_file_ref(ref_str, workspace_root) {
                // 行号缺失/损坏但文件客观存在 → 同样按"可定位"放行移交终审；
                // 行级语义本来就不归机械门管。
                let head = ref_str
                    .trim()
                    .trim_matches(|c| matches!(c, '`' | '*' | '"' | '\''))
                    .split([':', '#'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if ref_resolves_on_disk(head, workspace_root) {
                    has_valid_evidence = true;
                    break;
                }
                if embedded_ref_locatable(ref_str, ctx, workspace_root) {
                    has_valid_evidence = true;
                    break;
                }
                // 代码位置无法定位，报告该事实；不据此推断主观造假。
                failed_refs.push(ref_str.trim().to_string());
            }
            // 非代码证据（命令输出/退出码/API 响应/运维状态）不参与 diff 对账：
            // 由 noncode_evidence_skip_reasons 生成 Gate Notice 移交终审语义核验
        }

        if !has_valid_evidence {
            if obligation.evidence_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但未提供任何 evidence_refs 证据引用。请附现有源码位置、工具回执或其他取证引用；不适用时可标 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                ));
            } else if !failed_refs.is_empty() && is_diagnostic {
                blockers.push(format!(
                    "审查义务 [{}]（{}）为分流下发的诊断问题，免于 diff 对账——但引用的代码证据 `{}` 未能在当前工作区源码中定位（文件须客观存在、行号须在文件行数内）。请核对路径与行号；或引用真实命令观测（`命令 → 结果`/output_id）；或改 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", "),
                ));
            } else if !failed_refs.is_empty() {
                // 走到这里的引用解析形态像 file:line 但磁盘上无法定位——
                // 文件不存在或行号越界，属伪造/陈旧证据域，仍然硬拦。
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的代码证据 `{}` 在当前工作区无法定位（文件不存在，或行号超出文件实际行数）。\n【修法——任选，不要原样重发 finish】① 用 local_file 重新确认目标位置的真实行号（此前的编辑可能已使行号漂移），并在 `review_contract_update` 中更新该义务的 evidence_refs；② 若靠命令/测试输出证明 → 挂 `verify_command` 由宿主执行判分；③ 若义务不再适用 → 改 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", "),
                ));
            }
            // 其余情况：没有可机械核验的代码位置 → 本门不判定证据是否成立，
            // 不适用而不拦截：由 Gate Notice 移交终审做语义核验
        }
    }

    blockers
}

/// 「可定位但未落 diff」披露清单：义务已证实且引用了真实存在的代码位置，
/// 但没有任何引用落在本次改动区间内。这类证据放行不拦截（机械门只裁决
/// 「证据是否客观存在」），但须生成 Gate Notice 移交终审复核语义充分性——
/// 可能是诊断义务（证据本就在未修改文件里），也可能是修复义务引用了
/// 漂移前的陈旧行号，语义判定权在终审。
pub(crate) fn undiffed_locatable_notices(
    contract: &ReviewContract,
    diff_context: Option<&DiffContext>,
    workspace_root: Option<&std::path::Path>,
) -> Vec<String> {
    let Some(ctx) = diff_context else {
        return Vec::new();
    };
    if ctx.changed_files.is_empty() {
        return Vec::new();
    }
    let mut notices = Vec::new();
    for obligation in &contract.obligations {
        if obligation.status.trim().to_ascii_lowercase() != "evidenced" {
            continue;
        }
        if obligation.verify_command.is_some() || obligation.verify_script.is_some() {
            continue;
        }
        let mut in_diff = false;
        let mut locatable = false;
        for ref_str in &obligation.evidence_refs {
            if let Some((ref_path, start_line, end_line)) = parse_file_line_ref(ref_str) {
                if ctx.matches_diff(&ref_path, start_line, end_line) {
                    in_diff = true;
                }
                if ref_statically_locatable(&ref_path, start_line, workspace_root) {
                    locatable = true;
                }
            } else if looks_like_file_ref(ref_str, workspace_root) {
                locatable = true;
            }
        }
        if locatable && !in_diff {
            notices.push(format!(
                "义务 [{}]（{}）的代码证据真实可定位但不在本次 diff 覆盖区间内（诊断型证据或行号漂移所致），其语义充分性移交终审逐条核验",
                obligation.id,
                obligation.question.trim(),
            ));
        }
    }
    notices
}

/// 从一整句描述性证据里扫出所有形如 `path.ext:行号[-行号]` 的嵌入引用。
/// 模型常把证据写成一整句（"输出 abc：proxy.js:562 严格全等…→ 被拒"），
/// parse_file_line_ref 取最后一个冒号之前的全部文字当路径，必然定位失败。
/// 这里逐个冒号/#向前取路径 token（遇空白/全角标点/引号即停），仅作为「补救」——
/// 原解析已通过的不受影响，原解析失败时其中任一嵌入引用真实可定位即视为证据成立。
pub(crate) fn extract_embedded_file_line_refs(raw: &str) -> Vec<(String, u32, u32)> {
    let chars: Vec<char> = raw.chars().collect();
    let is_path_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '\\' | '-' | '~');
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' || chars[i] == '#' {
            let mut j = i + 1;
            if j < chars.len() && (chars[j] == 'L' || chars[j] == 'l') {
                j += 1;
            }
            let ds = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > ds {
                let start: Option<u32> = chars[ds..j].iter().collect::<String>().parse().ok();
                let mut end = start;
                // 可选范围 -N / ~N / ..N
                let mut k = j;
                if k < chars.len() && (chars[k] == '-' || chars[k] == '~') {
                    k += 1;
                } else if k + 1 < chars.len() && chars[k] == '.' && chars[k + 1] == '.' {
                    k += 2;
                } else {
                    k = usize::MAX;
                }
                if k != usize::MAX {
                    if k < chars.len() && (chars[k] == 'L' || chars[k] == 'l') {
                        k += 1;
                    }
                    let es = k;
                    while k < chars.len() && chars[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > es {
                        end = chars[es..k].iter().collect::<String>().parse().ok();
                    }
                }
                // 向前取路径 token
                let mut b = i;
                while b > 0 && is_path_char(chars[b - 1]) {
                    b -= 1;
                }
                let token: String = chars[b..i].iter().collect();
                let token = token.trim_matches(|c| c == '.' || c == '-').to_string();
                let path_shaped = token.contains('/') || token.rsplit_once('.').map_or(false, |(_, ext)| !ext.is_empty());
                if !token.is_empty() && path_shaped {
                    if let (Some(st), Some(en)) = (start, end) {
                        out.push((token, st, en.max(st)));
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// 描述性证据中是否至少有一个嵌入的 file:line 真实可定位（落入 diff 或磁盘上存在且行号在范围内）。
fn embedded_ref_locatable(
    raw: &str,
    ctx: &DiffContext,
    workspace_root: Option<&std::path::Path>,
) -> bool {
    extract_embedded_file_line_refs(raw).into_iter().any(|(path, st, en)| {
        ctx.matches_diff(&path, st, en) || ref_statically_locatable(&path, st, workspace_root)
    })
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
                // 整句描述里嵌着真实可定位的 file:line → 补救通过
                Some(_) | None if embedded_ref_locatable(ref_str, ctx, workspace_root) => {
                    has_valid_evidence = true;
                    break;
                }
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
            }
            // 其余情况：没有可机械核验的代码位置 → 移交终审，不凭文字外观拦截
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

/// 义务引用中没有可解析的代码位置时，宿主不判断自由文本是否构成证据。
/// 此时 diff 对账与静态定位均无判据——机械证据门不适用，移交终审语义核验。
fn obligation_evidence_is_noncode_only(
    obligation: &ReviewObligation,
    workspace_root: Option<&std::path::Path>,
) -> bool {
    !obligation.evidence_refs.is_empty()
        && obligation
            .evidence_refs
            .iter()
            .all(|r| parse_file_line_ref(r).is_none() && !looks_like_file_ref(r, workspace_root))
}

/// 机械证据门的 NotApplicable 交接：返回每条被跳过义务的跳过原因（含义务 id 与证据形态）。
/// 与 review_contract_evidence_blockers 共用同一套判定谓词（obligation_evidence_is_noncode_only），
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
        .filter(|o| obligation_evidence_is_noncode_only(o, workspace_root))
        .map(|o| {
            let refs = o
                .evidence_refs
                .iter()
                .map(|r| r.trim())
                .collect::<Vec<_>>()
                .join("`, `");
            format!(
                "契约义务 [{}]（{}）的证据引用为非代码形态（`{}`），diff/静态定位对账无判据；引用本身不证明观测已发生或义务已成立，移交终审核验",
                o.id,
                o.question.trim(),
                refs
            )
        })
        .collect()
}

