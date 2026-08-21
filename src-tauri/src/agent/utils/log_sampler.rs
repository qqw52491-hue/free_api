pub fn chars_preview(s: &str, limit: usize) -> String {
    let s = s.replace("\n", " ");
    if s.chars().count() > limit {
        let truncated: String = s.chars().take(limit).collect();
        format!("{}...", truncated)
    } else {
        s.to_string()
    }
}

/// 智能采样历史执行日志 (Head + Tail 采样 + 关键错误行去重聚合保留)
/// 严格采用三段式预算分账：保证头部前置环境、中间关键去重报错、尾部致命诊断 100% 不超预算
pub fn sample_execution_log(raw: &str, head_chars: usize, tail_chars: usize) -> String {
    let char_count = raw.chars().count();
    let safe_threshold = head_chars + tail_chars + 60;
    if char_count <= safe_threshold {
        return raw.to_string();
    }

    let head: String = raw.chars().take(head_chars).collect();
    let tail: String = raw
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect();
    let middle_omitted = char_count.saturating_sub(head_chars + tail_chars);

    // 零堆内存中间分配：通过行迭代器直接在原字符串切片上扫描中间被省略区间的关键错误行 (去重 + 计数 + 长度截断)
    let mut error_counts: Vec<(String, usize)> = Vec::new();
    let mut total_error_chars = 0usize;
    const MAX_ERROR_BUDGET_CHARS: usize = 800;
    const MAX_UNIQUE_ERRORS: usize = 15;

    let tail_start_char = char_count.saturating_sub(tail_chars);
    let mut current_char_idx = 0;

    for line in raw.lines() {
        let line_len = line.chars().count();
        let line_start = current_char_idx;
        let line_end = current_char_idx + line_len;
        current_char_idx = line_end + 1; // 计入换行符

        // 跳过完全落在头部已展示或尾部已展示区间的行
        if line_end <= head_chars || line_start >= tail_start_char {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        let is_critical = lower.contains("error")
            || lower.contains("panic")
            || lower.contains("fatal")
            || lower.contains("exception")
            || lower.contains("traceback")
            || lower.contains("caused by")
            || lower.contains("failed")
            || lower.contains("failure")
            || lower.contains("assert")
            || lower.contains("segfault")
            || lower.contains("500 internal")
            || lower.contains("403 forbidden")
            || lower.contains("拒绝访问")
            || lower.contains("权限不足")
            || lower.contains("未找到")
            || lower.contains("命令不存在")
            || lower.contains("编译失败")
            || lower.contains("异常")
            || lower.contains("错误");

        if is_critical {
            // 单行截断至 140 字符，防止长 JSON 或巨型单行撑爆
            let capped_line: String = trimmed.chars().take(140).collect();
            if let Some(existing) = error_counts.iter_mut().find(|(k, _)| k == &capped_line) {
                existing.1 += 1;
            } else if error_counts.len() < MAX_UNIQUE_ERRORS
                && total_error_chars + capped_line.len() < MAX_ERROR_BUDGET_CHARS
            {
                total_error_chars += capped_line.len();
                error_counts.push((capped_line, 1));
            }
        }
    }

    let middle_note = if error_counts.is_empty() {
        format!(
            "\n\n[... 已折叠中间 {} 字符，首尾关键信息已保留 ...]\n\n",
            middle_omitted
        )
    } else {
        let formatted_errors: Vec<String> = error_counts
            .into_iter()
            .map(|(line, count)| {
                if count > 1 {
                    format!("  ⚠️ [捕获错误 ×{}] {}", count, line)
                } else {
                    format!("  ⚠️ [捕获错误] {}", line)
                }
            })
            .collect();

        format!(
            "\n\n[... 已折叠中间 {} 字符，提取关键异常行如下 ...]\n{}\n[... 尾部最新诊断输出如下 ...]\n\n",
            middle_omitted,
            formatted_errors.join("\n")
        )
    };

    format!("{}{}{}", head, middle_note, tail)
}
