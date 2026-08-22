pub mod client;
pub mod registry;
pub mod types;

pub use client::*;
pub use registry::*;
pub use types::*;

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    /// 复现生产事故形态的 mock 服务：启动慢、且在 initialize 握手完成前
    /// 按协议拒绝任何请求。目录预热曾在握手完成前发出 tools/list，
    /// 得到 JSON-RPC 错误后静默放弃，模型可见工具目录全程为空。
    const SLOW_INIT_MOCK: &str = r#"
import sys, json, time
time.sleep(0.3)
initialized = False
init_count = 0
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    rid, method = req.get("id"), req.get("method")
    if method == "initialize":
        init_count += 1
        initialized = True
        resp = {"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "slow-init-mock", "version": "0.0.1"},
        }}
    elif not initialized:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {
            "code": -32002, "message": "Server not initialized",
        }}
    elif method == "tools/list":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "read_text_file", "description": "read",
             "inputSchema": {"type": "object",
                 "properties": {"path": {"type": "string"}, "cwd": {"type": "string"}},
                 "required": ["path"]}},
            {"name": "ripgrep_search", "description": "search",
             "inputSchema": {"type": "object",
                 "properties": {"pattern": {"type": "string"}, "cwd": {"type": "string"}, "glob": {"type": "string"}},
                 "required": ["pattern"]}},
        ]}}
    elif method == "initialize_count":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"count": init_count}}
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "result": {}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    fn slow_init_config() -> PluginConfig {
        PluginConfig {
            name: "slow-init-mock".to_string(),
            command: "python3".to_string(),
            args: vec!["-c".to_string(), SLOW_INIT_MOCK.to_string()],
            url: None,
            headers: HashMap::new(),
            env: HashMap::new(),
            capability_card: None,
        }
    }

    #[tokio::test]
    pub async fn stdio_tools_list_waits_for_initialize_handshake() {
        let client = Arc::new(McpClient::spawn(slow_init_config()).expect("spawn mock"));

        // 不预握手，直接发起 tools/list —— 模拟目录预热与后台保活的赛跑。
        // 修复前：请求先于握手到达，服务端拒绝，工具目录为空；
        // 修复后：send 前先串行等待首次 initialize 握手完成。
        let tools = tokio::time::timeout(Duration::from_secs(20), client.list_tools())
            .await
            .expect("list_tools 不应超时")
            .expect("list_tools 应在握手后成功");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "read_text_file");

        // 并发立即调用也必须成功：第二个调用方等待同一次握手，而不是各自重发 initialize。
        let mut handles = Vec::new();
        for _ in 0..3 {
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                let tools = c.list_tools().await.expect("并发 list_tools 应成功");
                assert_eq!(tools.len(), 2);
            }));
        }
        for handle in handles {
            handle.await.expect("join");
        }

        // 握手全生命周期只发生一次（协议要求；重复 initialize 是违规）。
        let count = client
            .send_request("initialize_count", json!({}))
            .await
            .expect("查询握手计数");
        assert_eq!(
            count.get("count").and_then(|v| v.as_i64()),
            Some(1),
            "initialize 必须只握手一次，实际: {:?}",
            count
        );
    }

    #[tokio::test]
    pub async fn test_mcp_client_parallel_multiplexing() {
        let mock_cmd = r#"while IFS= read -r line; do id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2); echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"echo\":$id}}"; done"#;
        let config = PluginConfig {
            name: "test-echo".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), mock_cmd.to_string()],
            url: None,
            headers: HashMap::new(),
            env: HashMap::new(),
            capability_card: None,
        };

        let client = Arc::new(McpClient::spawn(config).expect("spawn mock"));

        let mut handles = Vec::new();
        for i in 0..10 {
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                let res = c.send_request("test_method", json!({ "req_idx": i })).await;
                assert!(res.is_ok(), "Request {} failed: {:?}", i, res);
                let val = res.unwrap();
                assert!(val.get("echo").is_some(), "No echo for {}", i);
            }));
        }

        for h in handles {
            h.await.expect("join handle");
        }
    }

    #[tokio::test]
    #[ignore = "requires live remote MCP server"]
    pub async fn test_remote_hub_real_connection() {
        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer 7a237f4951af018156c723862fb988a50555ae3dd39ab2f29ef2290109209a58".to_string(),
        );
        let config = PluginConfig {
            name: "remote-hub".to_string(),
            command: String::new(),
            args: Vec::new(),
            url: Some("https://mcp.wxhjy.dpdns.org/mcp".to_string()),
            headers,
            env: HashMap::new(),
            capability_card: None,
        };

        let client = McpClient::spawn(config).expect("spawn remote-hub");
        let init_res = client.initialize().await;
        println!("Init result: {:?}", init_res);
        assert!(init_res.is_ok());

        let tools_res = client.list_tools().await;
        println!(
            "Tools result count: {:?}",
            tools_res.as_ref().map(|t| t.len())
        );
        assert!(tools_res.is_ok());
    }
}
