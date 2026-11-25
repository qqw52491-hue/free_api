use crate::agent::context::budget::{conservative_token_estimate, ContextBudget};
use crate::agent::context::types::{EvidenceReceiptEntry, SandwichContext};
use crate::agent::plan_tree::output::branch_text;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;

pub(in crate::agent::context) const PARALLEL_LABEL: &str = "【执行输出 · 并行分支】";

pub(super) fn record_file_reads(
    context: &mut SandwichContext,
    nodes: &[Value],
    step_id: usize,
    output_id: &str,
) -> BTreeMap<String, String> {
    let mut notices = BTreeMap::new();
    if output_id.is_empty() || output_id == "unknown_output_id" {
        return notices;
    }
    for node in nodes {
        if node["tool"] != "local_file" || node["ok"] != true {
            continue;
        }
        let Some(id) = node["id"].as_str() else {
            continue;
        };
        if nodes.iter().filter(|other| other["id"] == id).count() != 1 {
            continue;
        }
        // The native read receipt includes the resolved path and actual line window.
        let Some(body) = node["data"]["output"].as_str() else {
            continue;
        };
        let mut lines = body.lines();
        let Some(range) = lines
            .next()
            .filter(|line| line.starts_with("📄 文件: "))
            .and_then(|line| line.rsplit_once(" | 显示第 "))
            .and_then(|(_, range)| range.strip_suffix(" 行"))
            .and_then(|range| range.split_once(" 至 "))
        else {
            continue;
        };
        let (Ok(start), Ok(end)) = (range.0.parse::<usize>(), range.1.parse::<usize>()) else {
            continue;
        };
        let Some(path) = lines
            .next()
            .and_then(|line| line.strip_prefix("实际路径: "))
        else {
            continue;
        };
        if start == 0
            || start > end
            || !std::path::Path::new(path).is_absolute()
            || path.contains('|')
            || lines.next() != Some("---")
        {
            continue;
        }
        let key = format!("read|{path}|L{start}-{end}");
        let fingerprint = crate::agent::context::determinism::sha256_hex(body);
        let entry = context
            .evidence_ledger
            .entry(key)
            .or_insert(EvidenceReceiptEntry {
                count: 0,
                first_step: step_id,
                last_step: step_id,
                output_id: None,
                branch_id: None,
                read_fingerprint: None,
            });
        if let (Some(previous), Some(branch)) = (&entry.output_id, &entry.branch_id) {
            if previous != output_id && entry.read_fingerprint.as_deref() == Some(&fingerprint) {
                notices.insert(id.to_owned(), format!(
                    "第 {} 步已保存相同的读取结果（第 {start}–{end} 行）。可用 get_step_output(output_id={previous:?}, branch_id={branch:?}) 回查。",
                    entry.last_step,
                ));
            }
        }
        entry.count = entry.count.saturating_add(1);
        entry.last_step = step_id;
        entry.output_id = Some(output_id.to_owned());
        entry.branch_id = Some(id.to_owned());
        entry.read_fingerprint = Some(fingerprint);
    }
    notices
}

fn share_budget(needs: &[usize], mut remaining: usize) -> Vec<usize> {
    let mut grants = vec![0; needs.len()];
    let mut order: Vec<usize> = (0..needs.len()).collect();
    order.sort_by_key(|&i| needs[i]);
    for (rank, i) in order.into_iter().enumerate() {
        let grant = needs[i].min(remaining / (needs.len() - rank));
        grants[i] = grant;
        remaining -= grant;
    }
    grants
}

fn prefix_within(text: &str, tokens: usize) -> &str {
    if conservative_token_estimate(text) <= tokens {
        return text;
    }
    let boundaries: Vec<_> = text.char_indices().map(|(index, _)| index).collect();
    let end =
        boundaries.partition_point(|&index| conservative_token_estimate(&text[..index]) <= tokens);
    &text[..boundaries[end - 1]]
}

struct Page<'a> {
    data: &'a Value,
    body: &'a str,
    offset: u64,
    total: u64,
}

impl<'a> Page<'a> {
    fn parse(data: &'a Value) -> Option<Self> {
        data["output_id"].as_str()?;
        let body = data["raw_output"].as_str().or_else(|| {
            (data["read_scope"]["kind"] == "single_output")
                .then(|| data["content"].as_str())
                .flatten()
        })?;
        let offset = data["offset"].as_u64()?;
        let total = data["total_chars"].as_u64()?;
        let end = offset.saturating_add(body.chars().count() as u64);
        if data["next_offset"].as_u64()? != end || data["has_more"].as_bool()? != (end < total) {
            return None;
        }
        Some(Self {
            data,
            body,
            offset,
            total,
        })
    }

    fn locator(&self, shown_chars: u64) -> String {
        let next = self.offset.saturating_add(shown_chars);
        json!({
            "output_id": self.data["output_id"],
            "selection": self.data["selection"],
            "source": self.data["source"],
            "scope_notice": self.data["scope_notice"],
            "projected": self.data["projected"],
            "offset": self.offset,
            "next_offset": next,
            "has_more": next < self.total,
            "total_chars": self.total,
        })
        .to_string()
    }
}

fn is_page_tool(tool: &str) -> bool {
    matches!(
        tool,
        "get_step_output" | "read_step_output" | "get_output" | "task_store"
    )
}

pub(super) fn single_page(tool: &str, output: &str, cap: usize, output_id: &str) -> Option<String> {
    if !is_page_tool(tool) || conservative_token_estimate(output) <= cap {
        return None;
    }
    let data: Value = serde_json::from_str(output).ok()?;
    let page = Page::parse(&data)?;
    let header = format!("【执行输出 · 回查正文】\noutput_id: {output_id}\n");
    let reserved = conservative_token_estimate(&header)
        + conservative_token_estimate(&page.locator(page.body.chars().count() as u64))
        + 8;
    if reserved >= cap {
        return None;
    }
    let body = prefix_within(page.body, cap - reserved);
    Some(format!(
        "{header}{}\n{body}",
        page.locator(body.chars().count() as u64)
    ))
}

pub(super) fn parallel(
    nodes: &[Value],
    cap: usize,
    output_id: &str,
    notices: &BTreeMap<String, String>,
    lookup_scope: &str,
) -> Option<String> {
    let header = format!(
        "{PARALLEL_LABEL}\noutput_id: {output_id}\n{} 个分支。各段正文为连续前缀。回查本批原文可用 get_step_output 或 search_step_output，传本批 output_id 和原始 branch_id。回查页按标注的原始回执和 next_offset 续读。\n{lookup_scope}",
        nodes.len()
    );
    let mut branches = Vec::with_capacity(nodes.len());
    let mut overhead = conservative_token_estimate(&header);
    let page_data: Vec<_> = nodes
        .iter()
        .map(|node| {
            if !node["tool"].as_str().is_some_and(is_page_tool) {
                return None;
            }
            if let Some(text) = node["data"]["output"].as_str() {
                return serde_json::from_str(text).ok().map(Cow::Owned);
            }
            Some(Cow::Borrowed(&node["data"]))
        })
        .collect();
    for (index, (node, data)) in nodes.iter().zip(&page_data).enumerate() {
        let id = node["id"].as_str().unwrap_or_default();
        let tool = node["tool"].as_str().unwrap_or("unknown");
        let target = node["data"]["query"]
            .as_str()
            .or_else(|| {
                if tool != "local_file" {
                    return None;
                }
                node["data"]["output"]
                    .as_str()?
                    .lines()
                    .next()?
                    .strip_prefix("📄 文件: ")?
                    .rsplit_once(" | 共 ")
                    .map(|(path, _)| path)
            })
            .map(|target| target.chars().take(120).collect::<String>());
        let mut label = format!(
            "[分支 {}] {}",
            index + 1,
            json!({"branch_id": node["id"], "tool": tool, "ok": node["ok"], "timed_out": node["timed_out"], "target":target})
        );
        if let Some(notice) = notices.get(id) {
            label.push('\n');
            label.push_str(notice);
        }
        let page = data.as_deref().and_then(Page::parse);
        let body = if let Some(page) = &page {
            overhead +=
                conservative_token_estimate(&page.locator(page.body.chars().count() as u64));
            Cow::Borrowed(page.body)
        } else {
            branch_text(node).0
        };
        overhead += conservative_token_estimate(&label) + 32;
        branches.push((label, page, body));
    }
    if branches.is_empty() || overhead >= cap {
        return None;
    }
    let needs: Vec<_> = branches
        .iter()
        .map(|(_, _, body)| {
            conservative_token_estimate(body).min(ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP)
        })
        .collect();
    let grants = share_budget(&needs, cap - overhead);
    let mut text = header;
    for ((label, page, body), grant) in branches.iter().zip(grants) {
        let shown = prefix_within(body, grant);
        text.push_str(&label);
        text.push('\n');
        if let Some(page) = page {
            text.push_str(&page.locator(shown.chars().count() as u64));
            text.push('\n');
        } else if shown.len() < body.len() {
            text.push_str("[分支正文已节选，可回查原文]\n");
        }
        text.push_str(shown);
        text.push_str("\n\n");
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_only_unclaimed_budget_among_longer_branches() {
        assert_eq!(
            share_budget(&[4000, 1000, 3000, 4000], 12000),
            [4000, 1000, 3000, 4000]
        );
        assert_eq!(share_budget(&[4000; 4], 12000), [3000; 4]);
        assert_eq!(share_budget(&[1000, 4000, 4000], 6000), [1000, 2500, 2500]);
    }

    #[test]
    fn large_window_keeps_five_full_branch_allowances() {
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        context.context_window_tokens = 1_000_000;
        let instruction =
            serde_json::from_value(json!({"parallel":vec![json!({"tool":"local_file"});5]}))
                .unwrap();
        let branches: Vec<_> = (0..5)
            .map(|i| {
                json!({
                    "id":format!("branch-{i}"),"tool":"local_file","ok":true,
                    "data":{"output":"天气数据".repeat(2100)}
                })
            })
            .collect();
        let report = format!(
            "【计划执行结果报告】\n{}{}\n```\n",
            crate::agent::plan_tree::output::RESULTS_MARKER,
            serde_json::to_string_pretty(&branches).unwrap()
        );
        context.add_step(&instruction, &report, "batch", 1);
        let observation = context
            .turns_history
            .last()
            .unwrap()
            .content
            .as_str()
            .unwrap();
        for branch in observation.split("[分支 ").skip(1) {
            let (_, body) = branch.split_once("可回查原文]\n").unwrap();
            let estimate = conservative_token_estimate(body.trim_end());
            assert!((3995..=4000).contains(&estimate));
        }
        assert_eq!(observation.matches("[分支 ").count(), 5);
        assert!(conservative_token_estimate(observation) > 20_000);
    }
}
