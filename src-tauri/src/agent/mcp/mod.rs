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
