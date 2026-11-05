/// Budgets include headings and omission notices, in Unicode characters.
/// Every section receives its own allocation; no final whole-packet truncation.
pub(crate) fn section(title: &str, body: &str, budget: usize, tail: bool) -> String {
    let header = format!("【{title}】\n");
    let notice = if tail {
        "\n[前段截断；完整内容用 git_diff 回查]\n"
    } else {
        "\n[本节有省略；完整内容请按证据目录/原文入口回查]\n"
    };
    let available = budget.saturating_sub(header.chars().count() + 1);
    let count = body.chars().count();
    if header.chars().count() + count + 1 <= budget {
        return format!("{header}{body}\n");
    }
    if budget <= header.chars().count() + notice.chars().count() + 1 {
        return format!("【{title}：有省略】")
            .chars()
            .take(budget)
            .collect();
    }
    let keep = available.saturating_sub(notice.chars().count() + 1);
    let snippet: String = if tail {
        body.chars().skip(count.saturating_sub(keep)).collect()
    } else {
        body.chars().take(keep).collect()
    };
    format!("{header}{snippet}{notice}")
}

/// Reserve each block's share first, then lend unused capacity to blocks that
/// still have content. This is character accounting, not semantic ranking.
pub(crate) fn shares(demands: &[usize], reserved: &[usize], total: usize) -> Vec<usize> {
    let mut left = total;
    let mut result: Vec<_> = demands
        .iter()
        .zip(reserved)
        .map(|(need, floor)| {
            let n = (*need).min(*floor).min(left);
            left -= n;
            n
        })
        .collect();
    while left > 0 {
        let pending: Vec<_> = demands
            .iter()
            .enumerate()
            .filter(|(i, n)| **n > result[*i])
            .map(|(i, _)| i)
            .collect();
        if pending.is_empty() {
            break;
        }
        let chunk = (left / pending.len()).max(1);
        for i in pending {
            let n = (demands[i] - result[i]).min(chunk).min(left);
            result[i] += n;
            left -= n;
        }
    }
    result
}
