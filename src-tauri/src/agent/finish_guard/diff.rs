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

/// 核对审查契约中声称 `evidenced` 的义务证据是否真实落在代码修改 (diff) 中。
///
/// 遵循原则：
/// 1. 按证据核对，不按写入次数对账：单次编辑同时修改多个函数/位置，只要证据行号均在 diff 中即可放行（防止误报）；
/// 2. 严防虚假交付：只修一个文件却把另外几项也声称 evidenced 的行为，会被精确定位拦截；
/// 3. 义务若带有宿主可执行验证命令 (verify_command)，则交由宿主执行器判定，此处不强制要求代码 diff；
/// 4. 纯只读任务（side_effect_count == 0）免于 diff 行号门禁。
pub(crate) fn review_contract_evidence_blockers(
    contract: &ReviewContract,
    side_effect_count: usize,
    diff_context: Option<&DiffContext>,
) -> Vec<String> {
    if side_effect_count == 0 {
        return Vec::new();
    }
    let Some(ctx) = diff_context else {
        return Vec::new();
    };

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

        let mut has_valid_evidence = false;
        let mut failed_refs = Vec::new();

        for ref_str in &obligation.evidence_refs {
            if let Some((ref_path, start_line, end_line)) = parse_file_line_ref(ref_str) {
                if ctx.matches_diff(&ref_path, start_line, end_line) {
                    has_valid_evidence = true;
                    break;
                } else {
                    failed_refs.push(ref_str.trim().to_string());
                }
            } else {
                failed_refs.push(ref_str.trim().to_string());
            }
        }

        if !has_valid_evidence {
            if obligation.evidence_refs.is_empty() {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但未提供任何 evidence_refs 证据引用。请提供落入本次 diff 的代码行号证据，或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim()
                ));
            } else if failed_refs.iter().any(|r| parse_file_line_ref(r).is_some()) {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但引用的代码证据 `{}` 未在本次真实代码改动 (diff) 中找到对应修改行。请补充落入本次真实 diff 的代码行号证据，或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", ")
                ));
            } else {
                blockers.push(format!(
                    "审查义务 [{}]（{}）声称已证实 (evidenced)，但在包含代码写入的任务中，引用的证据 `{}` 缺少具体代码行号（需形如 `文件:行号`，且须落入真实 diff）。请补充行号证据，或将状态改为 waived 并说明理由。",
                    obligation.id,
                    obligation.question.trim(),
                    failed_refs.join(", ")
                ));
            }
        }
    }

    blockers
}

