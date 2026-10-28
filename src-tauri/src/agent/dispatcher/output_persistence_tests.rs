use super::*;
use crate::db::{init_db, DbState};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Fixture {
    root: PathBuf,
    db: DbState,
    session: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "free-api-output-regression-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let text = format!(
            "起点\n{}\n原文末尾_SENTINEL_天气数据\n",
            "天气正文 abcdefghijklmnopqrstuvwxyz\n".repeat(700)
        );
        std::fs::write(root.join("weather.txt"), text).unwrap();
        Self {
            root,
            db: DbState(Arc::new(Mutex::new(
                init_db(&PathBuf::from(":memory:")).unwrap(),
            ))),
            session: format!("output-regression-{}", uuid::Uuid::new_v4()),
        }
    }
    fn execute(&self, raw: Value) -> DispatchResult {
        let instruction = serde_json::from_value(raw).unwrap();
        run_agent_step(
            &self.db,
            &self.session,
            &instruction,
            &PluginRegistry::new(),
            Some(&self.root),
        )
    }
    fn archive(&self, result: &mut DispatchResult) -> String {
        let display = if result.success {
            result.stdout.clone()
        } else {
            result.stderr.clone()
        };
        let durable = take_tool_durable_output(result, &display);
        crate::agent::run_trace::save_step_output_with_meta(
            &self.db,
            &self.session,
            1,
            "parallel",
            "保存完整结果",
            if result.success {
                "succeeded"
            } else {
                "failed"
            },
            &durable,
        )
        .unwrap()
    }
    fn read(&self, id: &str, offset: usize) -> Value {
        let r = crate::agent::task_store::dispatch(
            &self.db,
            &self.session,
            "get_step_output",
            &json!({"output_id":id,"offset":offset,"limit":3000}),
        );
        assert!(r.success, "{}", r.stderr);
        serde_json::from_str(&r.stdout).unwrap()
    }
    fn search(&self, id: &str) -> Value {
        let r = crate::agent::task_store::dispatch(
            &self.db,
            &self.session,
            "search_step_output",
            &json!({"output_id":id,"pattern":"SENTINEL_天气数据"}),
        );
        assert!(r.success, "{}", r.stderr);
        serde_json::from_str(&r.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn read_leaf() -> Value {
    json!({"tool":"local_file","command":{"action":"read","path":"weather.txt","start_line":1,"end_line":1000}})
}

#[test]
fn composite_file_views_keep_successes_before_failure_and_survive_mcp_archiving() {
    let f = Fixture::new();
    std::fs::write(f.root.join("edit.txt"), "old\n").unwrap();
    let mut result = f.execute(json!({"commands":[
        {"tool":"local_file","params":{"action":"read","path":"edit.txt"}},
        {"tool":"local_file","args":{"action":"edit","path":"edit.txt","target_content":"old","replacement_content":"new"}},
        {"tool":"local_file","params":{"action":"edit","path":"edit.txt","target_content":"missing","replacement_content":"bad"}},
        {"tool":"local_file","params":{"action":"write","path":"never.txt","content":"must not run"}}
    ]}));
    assert!(!result.success);
    let files = result.view.as_ref().unwrap()["tool_views"].as_array().unwrap().clone();
    assert_eq!(files.len(), 3);
    assert_eq!(files[1]["success"], true);
    assert_eq!(files[1]["view"]["removed_lines"], json!(["old"]));
    assert_eq!(files[1]["view"]["added_lines"], json!(["new"]));
    assert_eq!(files[2]["success"], false);
    assert!(!f.root.join("never.txt").exists());
    assert_eq!(std::fs::read_to_string(f.root.join("edit.txt")).unwrap(), "new\n");
    let mut mcp = Vec::new();
    append_mcp_raw_results(&mut mcp, Some(&mcp_raw_result_view("p", "t", &json!({}), &json!({"content":[]}))));
    result.view = composite_results_view(mcp, files.clone());
    let id = f.archive(&mut result);
    assert_eq!(result.view.as_ref().unwrap()["tool_views"], json!(files));
    let saved = f.read(&id, 0);
    assert!(saved.to_string().contains("old"));
    let round_trip: Value = serde_json::from_str(&serde_json::to_string(&result.view).unwrap()).unwrap();
    assert_eq!(round_trip["tool_views"][1]["view"]["kind"], "diff");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_parallel_file_views_preserve_individual_results() {
    let f = Fixture::new();
    for name in ["a.txt", "b.txt"] { std::fs::write(f.root.join(name), "old\n").unwrap(); }
    let mut result = f.execute(json!({"plan_tree":[
        {"parallel":[
            [{"tool":"local_file","params":{"action":"read","path":"a.txt"}},
             {"tool":"local_file","params":{"action":"edit","path":"a.txt","target_content":"old","replacement_content":"A"}}],
            [{"tool":"local_file","command":{"action":"read","path":"b.txt"}},
             {"tool":"local_file","command":{"action":"edit","path":"b.txt","target_content":"absent","replacement_content":"B"}}]
        ]}
    ]}));
    assert!(!result.success);
    let files = result.view.as_ref().unwrap()["tool_views"].as_array().unwrap();
    assert_eq!(files.len(), 4);
    let edits: Vec<_> = files.iter().filter(|v| v["params"]["action"] == "edit").collect();
    assert_eq!(edits.len(), 2);
    assert_eq!(edits.iter().filter(|v| v["success"] == true).count(), 1);
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "A\n");
    assert_eq!(std::fs::read_to_string(f.root.join("b.txt")).unwrap(), "old\n");
    let before = result.view.clone();
    f.archive(&mut result);
    assert_eq!(result.view, before);
}

#[test]
fn failed_tool_streams_remain_retrievable_even_without_stderr() {
    let f = Fixture::new();
    for stderr in ["", "执行失败的错误信息"] {
        let mut result = DispatchResult {
            stdout: "失败前已经返回的原始数据".into(),
            stderr: stderr.into(),
            success: false,
            route: "test_tool".into(),
            view: None,
        };
        let id = f.archive(&mut result);
        let saved = f.read(&id, 0);
        let text = saved["raw_output"].as_str().unwrap();
        assert!(text.contains("失败前已经返回的原始数据"));
        if !stderr.is_empty() {
            assert!(text.contains(stderr));
        }
        assert_eq!(saved["source"]["outcome"], "failed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_full_output_survives_persistence_pagination_and_search() {
    let f = Fixture::new();
    let mut result = f.execute(json!({"parallel":[read_leaf(),read_leaf()]}));
    assert!(result.success, "{}", result.stderr);
    let id = f.archive(&mut result);
    let first = f.read(&id, 0);
    assert!(first["has_more"].as_bool().unwrap(), "{}", first);
    assert!(first["total_chars"].as_u64().unwrap() > 6000);
    let later = f.read(&id, 3000);
    assert!(!later["raw_output"].as_str().unwrap().is_empty());
    assert!(f.search(&id)["total_matches"].as_u64().unwrap() > 0);
    let original = crate::agent::run_trace::get_step_output(&f.db, &f.session, &id).unwrap();
    let mut restored = String::new();
    for offset in (0..original.chars().count()).step_by(3000) {
        restored.push_str(f.read(&id, offset)["raw_output"].as_str().unwrap());
    }
    assert_eq!(restored, original);
    // The existing ingress budget still bounds model context after full persistence.
    let mut context = crate::agent::context::SandwichContext::new("sys".into(), "测试".into());
    let instruction =
        serde_json::from_value(json!({"parallel":[read_leaf(),read_leaf()]})).unwrap();
    context.add_step(&instruction, &original, &id);
    let model_view = context
        .turns_history
        .last()
        .unwrap()
        .content
        .as_str()
        .unwrap();
    assert!(model_view.contains(&id));
    assert!(model_view.contains("truncated: true"));
    assert!(
        crate::agent::context::budget::conservative_token_estimate(model_view)
            <= crate::agent::context::budget::ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_parallel_preserves_its_successful_branch_output() {
    let f = Fixture::new();
    let mut result = f.execute(json!({"parallel":[read_leaf(),{"tool":"local_file","command":{"action":"read","path":"not-found.txt"}}]}));
    assert!(!result.success);
    let id = f.archive(&mut result);
    assert!(f.search(&id)["total_matches"].as_u64().unwrap() > 0);
    assert_eq!(f.read(&id, 0)["source"]["outcome"], "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serial_tree_and_commands_keep_prior_success_on_failure_without_running_later_writes() {
    let f = Fixture::new();
    let missing = json!({"tool":"local_file","command":{"action":"read","path":"missing.txt"}});
    let later_write = json!({"tool":"local_file","command":{"action":"write","path":"must-not-exist.txt","content":"bad"}});
    for raw in [
        json!({"plan_tree":[read_leaf(),missing.clone(),later_write.clone()]}),
        json!({"commands":[read_leaf(),missing,later_write]}),
    ] {
        let mut result = f.execute(raw);
        assert!(!result.success);
        assert!(!f.root.join("must-not-exist.txt").exists());
        let id = f.archive(&mut result);
        assert!(f.search(&id)["total_matches"].as_u64().unwrap() > 0);
    }
}

const MCP_FIXTURE: &str = r#"
import sys,json
for line in sys.stdin:
    req=json.loads(line)
    if req.get('id') is None: continue
    method=req.get('method')
    if method=='initialize':
        out={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'output-fixture','version':'1'}}
    elif method=='tools/list':
        out={'tools':[{'name':'read_text','description':'fixture','inputSchema':{'type':'object','properties':{'fail':{'type':'boolean'}}}}]}
    elif method=='tools/call':
        out={'content':[{'type':'text','text':'MCP 原文\n'*1800+'MCP_END_SENTINEL'}, {'type':'image','mimeType':'image/png','data':'A'*10000}],
             'structuredContent':{'rows':[1,2,3]},'_meta':{'raw_proof':'MCP_RAW_SENTINEL'},'vendorExtra':{'keep':True},
             'isError':req.get('params',{}).get('arguments',{}).get('fail',False)}
    else: out={}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':out}),flush=True)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_mcp_transport_direct_mixed_parallel_and_serial_failures_retain_raw_evidence() {
    use crate::agent::mcp::{McpClient, PluginConfig, PluginSource};
    let f = Fixture::new();
    let client = Arc::new(
        McpClient::spawn(PluginConfig {
            name: "output-fixture".into(),
            command: "python3".into(),
            args: vec!["-c".into(), MCP_FIXTURE.into()],
            url: None,
            headers: Default::default(),
            env: Default::default(),
            capability_card: None,
        })
        .unwrap(),
    );
    client.list_tools().await.unwrap();
    let mut registry = PluginRegistry::new();
    registry
        .insert_with_source("output-fixture".into(), client, PluginSource::ManagedDb)
        .unwrap();
    let mcp = json!({"tool":"output-fixture/read_text","command":{}});
    let bad_mcp = json!({"tool":"output-fixture/read_text","command":{"fail":true}});
    for (raw, success, has_native) in [
        (mcp.clone(), true, false),
        (bad_mcp.clone(), false, false),
        (json!({"parallel":[read_leaf(),mcp]}), true, true),
        (
            json!({"parallel":[read_leaf(),bad_mcp.clone()]}),
            false,
            true,
        ),
        (
            json!({"plan_tree":[read_leaf(),bad_mcp.clone()]}),
            false,
            true,
        ),
        (json!({"commands":[read_leaf(),bad_mcp]}), false, true),
    ] {
        let db = f.db.clone();
        let sid = f.session.clone();
        let root = f.root.clone();
        let registry = registry.clone();
        let mut result = tokio::task::spawn_blocking(move || {
            let inst = serde_json::from_value(raw).unwrap();
            run_agent_step(&db, &sid, &inst, &registry, Some(&root))
        })
        .await
        .unwrap();
        assert_eq!(result.success, success, "{}", result.stderr);
        let id = f.archive(&mut result);
        let persisted = crate::agent::run_trace::get_step_output(&f.db, &f.session, &id).unwrap();
        let packet: Value = serde_json::from_str(&persisted).unwrap();
        assert_eq!(
            packet["items"][0]["result"]["_meta"]["raw_proof"],
            "MCP_RAW_SENTINEL"
        );
        assert_eq!(
            packet["items"][0]["result"]["structuredContent"]["rows"],
            json!([1, 2, 3])
        );
        assert_eq!(
            packet["items"][0]["result"]["content"][1]["data"]
                .as_str()
                .unwrap()
                .len(),
            10000
        );
        assert_eq!(packet["items"][0]["result"]["isError"], !success);
        let projected = project_mcp_durable_output(&persisted).unwrap();
        assert!(projected.contains("MCP_END_SENTINEL"));
        assert_eq!(projected.contains("SENTINEL_天气数据"), has_native);
        let raw_result = crate::agent::task_store::dispatch(
            &f.db,
            &f.session,
            "search_step_output",
            &json!({"output_id":id,"pattern":"MCP_RAW_SENTINEL","raw":true}),
        );
        assert!(raw_result.success);
        let raw_result: Value = serde_json::from_str(&raw_result.stdout).unwrap();
        assert!(raw_result["total_matches"].as_u64().unwrap() > 0);
    }
}
