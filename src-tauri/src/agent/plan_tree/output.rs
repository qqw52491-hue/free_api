//! 批量回执的只读投影：复用原有分支 id，不新增观察、执行或持久化状态。
use serde_json::{json, Value};
use std::borrow::Cow;

pub(crate) const RESULTS_MARKER: &str = "详细返回数据 (JSON):\n```json\n";

#[derive(Debug)]
pub(crate) struct SelectedBranch {
    pub text: String,
    pub selection: Value,
}

fn leaves<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
    if let Some(items) = value.as_array() {
        for item in items { leaves(item, found); }
    } else if value.get("ok").is_some_and(Value::is_boolean)
        && value.get("id").is_some_and(Value::is_string)
        && value.get("tool").is_some_and(Value::is_string)
    {
        found.push(value);
    }
}

fn branch_text(node: &Value) -> (Cow<'_, str>, &'static str) {
    if let Some(output) = node.get("data").and_then(|v| v.get("output")).and_then(Value::as_str) {
        return (Cow::Borrowed(output), "data.output");
    }
    // JSON 数据、失败和超时保留完整节点结构，不把 error 误投影为成功正文。
    (Cow::Owned(serde_json::to_string_pretty(node).unwrap_or_else(|_| node.to_string())), "node")
}

/// 目录位于长正文之前，仅作定位线索；完整结果仍原样保存在后面的 JSON 中。
pub(crate) fn branch_directory(results: &Value) -> String {
    let mut nodes = Vec::new();
    leaves(results, &mut nodes);
    if nodes.is_empty() { return String::new(); }
    let mut directory = String::from("分支目录（按 branch_id 单独读取；目录仅为节选，完整结果见后方 JSON）：\n");
    for node in nodes.iter().take(20) {
        let body = node.get("data").and_then(|data| data.get("output")).and_then(Value::as_str)
            .or_else(|| node.get("error").and_then(Value::as_str)).unwrap_or("结构化 JSON 结果");
        let head: String = body.chars().take(240).collect();
        let preview: String = head.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(180).collect();
        let entry = json!({"branch_id": node["id"], "tool": node["tool"], "ok": node["ok"],
            "timed_out": node["timed_out"], "preview": preview});
        directory.push_str(&format!("- {}\n", entry));
    }
    if nodes.len() > 20 {
        directory.push_str(&format!("另有 {} 个分支未列出，可从完整回执定位。\n", nodes.len() - 20));
    }
    directory.push_str("读取方式：get_step_output(output_id=本批回执句柄, branch_id=目录中的原始 ID)；offset 从该分支开头计，省略 branch_id 仍读整批。\n\n");
    directory
}

/// 仅解析已知批量回执外壳；不会递归解析正文中的 JSON 或从标题/URL 猜分支。
/// 重复 ID、未知 ID 和旧格式无法定位时明确报错，不能静默退回另一篇正文。
pub(crate) fn select_branch(persisted: &str, request: &Value) -> Result<Option<SelectedBranch>, String> {
    let Some(value) = request.get("branch_id") else { return Ok(None); };
    let id = value.as_str().filter(|s| !s.trim().is_empty())
        .ok_or("branch_id 必须为非空字符串，填写回执目录中的原始分支 ID。")?;
    if !persisted.trim_start().starts_with("【计划执行结果报告】") {
        return Err("该回执不是可定位分支的批量结果；省略 branch_id 可按原方式读取完整回执。".into());
    }
    let payload = persisted.split_once(RESULTS_MARKER).and_then(|(_, rest)| rest.split_once("\n```"))
        .map(|(body, _)| body).ok_or("批量回执缺少完整结果 JSON；省略 branch_id 可读取已保存原文。")?;
    let results: Value = serde_json::from_str(payload)
        .map_err(|_| "批量结果 JSON 无法解析；省略 branch_id 可读取已保存原文。")?;
    let mut nodes = Vec::new();
    leaves(&results, &mut nodes);
    let mut matches = nodes.into_iter().filter(|node| node["id"].as_str() == Some(id));
    let node = matches.next().ok_or_else(|| format!("本回执没有 branch_id={id:?} 的可定位分支；请核对目录或省略 branch_id 读取整批。"))?;
    if matches.next().is_some() {
        return Err(format!("本回执中 branch_id={id:?} 不唯一，未选择正文；请读取完整回执核对。"));
    }
    let raw = request.get("raw").and_then(Value::as_bool).unwrap_or(false)
        || request.get("projection").and_then(Value::as_str) == Some("raw");
    let (text, view) = if raw {
        (Cow::Owned(serde_json::to_string_pretty(node).unwrap_or_else(|_| node.to_string())), "node")
    } else { branch_text(node) };
    Ok(Some(SelectedBranch { text: text.into_owned(), selection: json!({
        "branch_id": node["id"], "tool": node["tool"], "ok": node["ok"],
        "timed_out": node["timed_out"], "elapsed_ms": node["elapsed_ms"], "view": view,
        "notice": "分支字段来自该 output_id 的已保存结果，不是新的执行或独立输出句柄。"
    }) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::plan_tree::{PlanExecutionReport, NodeResult};
    use crate::agent::review_team::tests::TestDb;
    use crate::agent::task_store;

    fn report(results: Value) -> String {
        PlanExecutionReport { success: true, total_steps: 2, successful_steps: 2,
            failed_steps: 0, timed_out_steps: 0, elapsed_total_ms: 20,
            status: "completed".into(), error_summary: None, results }.to_llm_feedback()
    }
    fn bodies() -> (String, String, String) {
        let first = format!("Title: FIRST-ONLY\nURL Source: https://example.test/first\n{}", "第一篇正文\n".repeat(9000));
        let second = format!("Title: SECOND-ONLY\nURL Source: https://example.test/second\n{}", "第二篇🎉证据\n".repeat(100));
        let raw = report(json!([
            NodeResult::success(Some("$1".into()), Some("web_fetch".into()), json!({"output":first}), 10),
            NodeResult::success(Some("$2".into()), Some("web_fetch".into()), json!({"output":second}), 11)
        ]));
        (first, second, raw)
    }
    fn save(db: &crate::db::DbState, raw: &str) -> String {
        crate::agent::run_trace::save_step_output_with_meta(db, "session", 3, "parallel", "fetch", "succeeded", raw).unwrap()
    }
    fn read(db: &crate::db::DbState, action: &str, request: Value) -> Value {
        let result = task_store::dispatch(db, "session", action, &request);
        assert!(result.success, "{}", result.stderr);
        serde_json::from_str(&result.stdout).unwrap()
    }

    #[test]
    fn branch_directory_precedes_long_body_and_native_read_pages_only_the_selected_article() {
        let data = TestDb::new();
        let (_, second, raw) = bodies();
        let directory = raw.split_once(RESULTS_MARKER).unwrap().0;
        assert!(directory.contains("SECOND-ONLY") && directory.contains("$2"));
        assert!(directory.chars().count() < 1800, "目录不重复堆入长正文");
        let oid = save(&data.db, &raw);
        let full = read(&data.db, "get_step_output", json!({"output_id":oid}));
        assert_eq!(full["raw_output"], raw.chars().take(3000).collect::<String>());
        assert!(full.get("selection").is_none());
        for action in ["get_step_output", "get_output"] {
            let selected = read(&data.db, action, json!({"output_id":oid,"branch_id":"$2","limit":80}));
            assert_eq!(selected["raw_output"], second.chars().take(80).collect::<String>());
            assert_eq!(selected["total_chars"], second.chars().count());
            assert_eq!(selected["next_offset"], 80);
            assert_eq!(selected["source"], full["source"]);
            assert_eq!(selected["selection"]["branch_id"], "$2");
            let next = read(&data.db, action, json!({"output_id":oid,"branch_id":"$2","offset":80,"limit":60}));
            assert_eq!(next["raw_output"], second.chars().skip(80).take(60).collect::<String>());
        }
        // 顶层原生别名实际经过 dispatcher，同样读取该分支。
        let inst = serde_json::from_value(json!({"tool":"get_step_output","params":{"output_id":oid,"branch_id":"$2","limit":80}})).unwrap();
        let result = crate::agent::dispatcher::run_agent_step(&data.db,"session",&inst,
            &crate::agent::mcp::PluginRegistry::new(),None);
        assert!(result.success, "{}", result.stderr);
        assert_eq!(serde_json::from_str::<Value>(&result.stdout).unwrap()["raw_output"], second.chars().take(80).collect::<String>());
        assert_eq!(crate::agent::run_trace::get_step_output(&data.db,"session",&oid).unwrap(),raw);
    }

    #[test]
    fn branch_search_and_reviewer_read_keep_scope_and_original_observation_source() {
        let data = TestDb::new();
        let (_, second, raw) = bodies();
        let oid = save(&data.db, &raw);
        for action in ["search_step_output", "grep_step_output", "search_output"] {
            let out = read(&data.db,action,json!({"output_id":oid,"branch_id":"$2","pattern":"FIRST-ONLY"}));
            assert_eq!(out["total_matches"],0);
            let found = read(&data.db,action,json!({"output_id":oid,"branch_id":"$2","pattern":"SECOND-ONLY","context_lines":1}));
            assert_eq!(found["total_matches"],1);
            assert_eq!(found["matches"][0]["line_number"],1);
            assert!(found["matches"][0]["snippet"].as_str().unwrap().contains("https://example.test/second"));
        }
        let boundary = chrono::Utc::now() + chrono::Duration::seconds(1);
        let request = json!({"output_id":oid,"branch_id":"$2","limit":70});
        let review: Value = serde_json::from_str(&crate::agent::reviewer_gatekeeper::read_review_step_output(
            &data.db,"session",&request,boundary).unwrap()).unwrap();
        assert_eq!(review["content"],second.chars().take(70).collect::<String>());
        assert_eq!(review["source"]["relation"],"historical_reference");
        let search: Value = serde_json::from_str(&crate::agent::reviewer_gatekeeper::search_review_step_output(
            &data.db,"session",&json!({"output_id":oid,"branch_id":"$2","pattern":"SECOND-ONLY"}),boundary).unwrap()).unwrap();
        assert_eq!(search["search_scope"],"selected_branch");
        assert_eq!(search["source"],review["source"]);
        assert!(!task_store::dispatch(&data.db,"other-session","get_step_output",&request).success);
        assert!(crate::agent::reviewer_gatekeeper::read_review_step_output(&data.db,"other-session",&request,boundary).is_err());
    }

    #[test]
    fn old_reports_nested_arrays_raw_json_and_failure_nodes_remain_readable() {
        let raw = report(json!([
            [NodeResult::success(Some("data".into()), Some("query".into()),json!({"rows":[1,2]}),1)],
            NodeResult::failure(Some("failed".into()),Some("web_fetch".into()),"HTTP 500".into(),true,5)
        ]));
        // 旧记录没有目录，沿用同一个完整 JSON 外壳即可定位，不改写旧记录。
        let old = format!("【计划执行结果报告】状态: 旧记录\n{}{}",RESULTS_MARKER,raw.split_once(RESULTS_MARKER).unwrap().1);
        let selected = select_branch(&old,&json!({"branch_id":"data"})).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&selected.text).unwrap()["data"]["rows"],json!([1,2]));
        let failed = select_branch(&old,&json!({"branch_id":"failed"})).unwrap().unwrap();
        assert_eq!(failed.selection["ok"],false);
        assert_eq!(failed.selection["timed_out"],true);
        assert!(failed.text.contains("HTTP 500"));
        let (_,_,article) = bodies();
        for raw_flag in [json!({"branch_id":"$2","raw":true}), json!({"branch_id":"$2","projection":"raw"})] {
            let branch = select_branch(&article,&raw_flag).unwrap().unwrap();
            assert_eq!(serde_json::from_str::<Value>(&branch.text).unwrap()["id"],"$2");
        }
    }

    #[test]
    fn missing_ambiguous_and_invalid_ids_never_fall_back_to_a_different_body() {
        let node = json!({"id":"same","tool":"web_fetch","ok":true,"data":{"output":"body"}});
        let raw = report(json!([node,node]));
        assert!(select_branch(&raw,&json!({"branch_id":"same"})).unwrap_err().contains("不唯一"));
        for request in [json!({"branch_id":"missing"}),json!({"branch_id":""}),json!({"branch_id":1})] {
            assert!(select_branch(&raw,&request).is_err());
        }
        assert!(select_branch("普通正文",&json!({"branch_id":"same"})).is_err());
        assert!(select_branch("普通正文",&json!({})).unwrap().is_none());
        assert!(select_branch("【计划执行结果报告】\n损坏 JSON",&json!({"branch_id":"same"})).is_err());
        let embedded = report(json!([{"id":"real","tool":"read","ok":true,"data":{"output":node.to_string()}}]));
        assert!(select_branch(&embedded,&json!({"branch_id":"same"})).is_err());
    }
}
