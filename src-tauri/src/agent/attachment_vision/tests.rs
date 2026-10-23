use super::*;
use crate::agent::{
    checkpoint::{load_checkpoint, save_checkpoint, AgentCheckpoint},
    mcp::PluginRegistry,
    review_team::tests::TestDb,
    router::{AgentRouter, RoutingTier},
};
use std::sync::{Arc, Mutex};
use tauri::Manager;

struct Fixture {
    data: TestDb,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(frames: Vec<Value>) -> Self {
        let data = TestDb::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let app = axum::Router::new().route(
            "/{*path}",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let index = {
                    let mut requests = captured.lock().unwrap();
                    requests.push(body);
                    requests.len() - 1
                };
                let frame = frames[index.min(frames.len() - 1)].clone();
                async move {
                    if frame["fixture_delay"] == true {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        format!("data: {frame}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        {
            let conn = data.db.0.lock().unwrap();
            conn.execute("INSERT INTO platforms (id,name,base_url,api_key,created_at) VALUES ('mock','mock',?1,'test-only','now')", [format!("http://{address}/v1")]).unwrap();
            for (id, name) in [
                ("vision", "observer"),
                ("pro", "planner"),
                ("flash", "executor"),
            ] {
                conn.execute("INSERT INTO models (id,platform_id,name,display_name,max_tokens,created_at) VALUES (?1,'mock',?2,?2,8192,'now')",rusqlite::params![id,name]).unwrap();
            }
        }
        Self {
            data,
            requests,
            server,
        }
    }
    fn routing(&self) -> ModelRoutingConfig {
        ModelRoutingConfig {
            pro: "pro".into(),
            flash: "flash".into(),
            vision: "vision".into(),
            ..Default::default()
        }
    }
    async fn prepare(&self, context: &mut SandwichContext) -> Result<bool, String> {
        let (_sender, mut rx) = watch::channel(0);
        prepare(
            &self.data.db,
            context,
            &self.routing(),
            "fixture",
            0,
            &self.data.root.join("images"),
            &mut rx,
        )
        .await
    }
}
fn frame(text: &str, reason: &str) -> Value {
    json!({"choices":[{"delta":{"content":text},"finish_reason":reason}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})
}
fn context() -> SandwichContext {
    let mut context = SandwichContext::new("sys".into(), "排查点击模型选择框闪两下".into());
    context.add_image_feedback(
        "原始附件 a.png，用户问题的参考资料",
        "data:image/png;base64,QUJD",
    );
    context.add_image_feedback(
        "原始附件 b.png，用户问题的参考资料",
        "data:image/png;base64,REVG",
    );
    let mut cp = AgentCheckpoint::new("fixture".into(), &context);
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.route = "technical_plan".into();
    cp.triage_verdict = Some(triage);
    context.plan_gate = Some(cp.plan_gate_projection());
    context
}
fn image_count(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .flat_map(|m| m.content.as_array().into_iter().flatten())
        .filter(|p| p["type"] == "image_url")
        .count()
}

#[tokio::test]
async fn observed_attachments_restore_planning_and_survive_checkpoint_recovery() {
    let instruction =
        json!({"tool":"local_file","command":{"action":"read","path":"evidence.txt"}}).to_string();
    let f = Fixture::new(vec![
        frame(
            "可见输入区底部模型按钮和另一张路由面板；截图不能证明点击动画次数。",
            "stop",
        ),
        frame(&instruction, "stop"),
    ])
    .await;
    std::fs::write(f.data.root.join("evidence.txt"), "fixture evidence").unwrap();
    let mut context = context();
    let original = context.turns_history.clone();
    assert_eq!(
        AgentRouter::route_for_step(&context, &f.routing(), 0).tier,
        RoutingTier::Vision
    );
    assert!(f.prepare(&mut context).await.unwrap());
    assert_eq!(
        serde_json::to_value(&context.turns_history).unwrap(),
        serde_json::to_value(&original).unwrap(),
        "原始图像与来源不得被摘要改写"
    );
    assert_eq!(image_count(&context.assemble_messages()), 0);
    assert_eq!(
        AgentRouter::route_for_step(&context, &f.routing(), 1).tier,
        RoutingTier::Pro
    );
    let projected = context
        .assemble_messages()
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        projected.matches("可见输入区底部模型按钮").count(),
        1,
        "同批观察不因两张图重复注入"
    );
    assert!(projected.contains("原始附件 a.png") && projected.contains("原始附件 b.png"));
    let mut cp = AgentCheckpoint::new("fixture".into(), &context);
    save_checkpoint(&f.data.db, &mut cp).unwrap();
    let restored = load_checkpoint(&f.data.db, "fixture").unwrap().unwrap();
    let mut resumed = SandwichContext::new("sys".into(), "继续".into());
    restored.restore_context(&mut resumed);
    resumed.plan_gate = context.plan_gate.clone();
    assert!(!f.prepare(&mut resumed).await.unwrap());
    assert_eq!(image_count(&resumed.assemble_messages()), 0);
    let selected = AgentRouter::route_for_step(&resumed, &f.routing(), 2);
    assert_eq!(selected.model_id, "pro");
    let app = tauri::test::mock_app();
    app.manage(f.data.db.clone());
    let (_sender, mut rx) = watch::channel(0);
    let (inst, _, _, _) = crate::agent::utils::call_llm(
        &resumed,
        &app.state::<DbState>(),
        selected.model_id,
        "Pro",
        None,
        2,
        "fixture",
        false,
        0,
        "auto",
        false,
        &mut rx,
    )
    .await
    .unwrap();
    let result = crate::agent::dispatcher::run_agent_step(
        &f.data.db,
        "fixture",
        &inst,
        &PluginRegistry::new(),
        Some(&f.data.root),
    );
    assert!(result.success && result.stdout.contains("fixture evidence"));
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "observer");
    assert_eq!(requests[1]["model"], "planner");
    assert!(requests[0]["messages"].to_string().contains("image_url"));
    assert!(!requests[1]["messages"].to_string().contains("image_url"));
    assert_eq!(requests[0]["max_tokens"], 8192);
    assert_eq!(requests[1]["max_tokens"], 8192);
}

#[tokio::test]
async fn failed_or_truncated_observation_keeps_images_and_does_not_repeat_on_restore() {
    for response in [frame("partial", "length"), frame("", "stop")] {
        let f = Fixture::new(vec![response]).await;
        let mut context = context();
        assert!(f.prepare(&mut context).await.unwrap());
        assert!(history_requires_vision(&context));
        assert_eq!(image_count(&context.assemble_messages()), 2);
        assert!(context
            .assemble_messages()
            .iter()
            .any(|m| m.content.to_string().contains("图片观察未完成")));
        let mut cp = AgentCheckpoint::new("fixture".into(), &context);
        save_checkpoint(&f.data.db, &mut cp).unwrap();
        let cp = load_checkpoint(&f.data.db, "fixture").unwrap().unwrap();
        let mut restored = SandwichContext::new("sys".into(), "继续".into());
        cp.restore_context(&mut restored);
        assert!(!f.prepare(&mut restored).await.unwrap());
        assert_eq!(f.requests.lock().unwrap().len(), 1);
        assert_eq!(
            AgentRouter::route_for_step(&restored, &f.routing(), 5).tier,
            RoutingTier::Vision
        );
    }
}

#[tokio::test]
async fn original_copy_can_be_viewed_explicitly_without_pinning_later_calls() {
    let f = Fixture::new(vec![frame("图片观察正文", "stop")]).await;
    let mut context = context();
    f.prepare(&mut context).await.unwrap();
    let path = context
        .image_observations
        .values()
        .next()
        .unwrap()
        .references[0]
        .clone();
    assert_eq!(std::fs::read(&path).unwrap(), b"ABC");
    let pending = crate::agent::tools::view_image::PendingImageQueue::default();
    let result = crate::agent::tools::view_image::dispatch(
        &json!({"path":path}),
        Some(&f.data.root),
        "fixture",
        &pending,
    );
    assert!(result.success, "{}", result.stderr);
    crate::agent::tools::view_image::transfer_pending_images(
        &pending,
        &mut context.pending_view_images,
    )
    .unwrap();
    assert_eq!(
        AgentRouter::route_for_step(&context, &f.routing(), 6).tier,
        RoutingTier::Vision
    );
    assert_eq!(image_count(&context.assemble_messages()), 1);
    context.pending_view_images.clear();
    assert_eq!(
        AgentRouter::route_for_step(&context, &f.routing(), 7).tier,
        RoutingTier::Pro
    );
    assert_eq!(f.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn new_image_and_legacy_checkpoint_are_observed_without_reusing_wrong_sources() {
    let f = Fixture::new(vec![frame("观察一", "stop"), frame("观察二", "stop")]).await;
    let mut context = context();
    // 旧检查点无回执字段，恢复后仍保留全部原图，尚未观察时仍选视觉档。
    let cp = AgentCheckpoint::new("legacy".into(), &context);
    let mut value = serde_json::to_value(cp).unwrap();
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("image_observations");
    let cp: AgentCheckpoint = serde_json::from_value(value).unwrap();
    cp.restore_context(&mut context);
    assert!(history_requires_vision(&context));
    f.prepare(&mut context).await.unwrap();
    context.add_image_feedback("新的 c.png", "data:image/png;base64,R0hJ");
    assert!(history_requires_vision(&context));
    f.prepare(&mut context).await.unwrap();
    assert!(!history_requires_vision(&context));
    assert_eq!(f.requests.lock().unwrap().len(), 2);
    let requests = f.requests.lock().unwrap();
    let second = requests[1]["messages"].to_string();
    assert!(second.contains("R0hJ"));
    assert!(!second.contains("QUJD"));
}

#[tokio::test]
async fn interrupted_observation_keeps_originals_and_has_no_completed_receipt() {
    let f = Fixture::new(vec![json!({"fixture_delay":true})]).await;
    let mut context = context();
    let (sender, mut rx) = watch::channel(0);
    let routing = f.routing();
    let directory = f.data.root.join("images");
    let stop = async {
        while f.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        sender.send_modify(|g| *g += 1);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(
            prepare(
                &f.data.db,
                &mut context,
                &routing,
                "fixture",
                0,
                &directory,
                &mut rx
            ),
            stop
        )
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), llm::STREAM_INTERRUPTED_ERROR);
    assert!(context.image_observations.is_empty());
    assert_eq!(image_count(&context.assemble_messages()), 2);
}

#[tokio::test]
async fn observations_survive_real_compaction_and_stay_scoped_to_task() {
    let f = Fixture::new(vec![frame(
        "可见按钮在输入区下方，无法从截图确认动画次数。",
        "stop",
    )])
    .await;
    let mut context = context();
    context.context_window_tokens = 8000;
    for i in 0..20 {
        context.turns_history.push(ChatMessage {
            role: "assistant".into(),
            content: json!(format!("步骤{i}")),
        });
        context.turns_history.push(ChatMessage {
            role: "user".into(),
            content: json!("普通历史文字，无工具引用。".repeat(200)),
        });
    }
    // 未完成首次观察前，不能先按 base64 长度把图片归档掉。
    let before = context.turns_history.len();
    context.maybe_compact_history();
    assert_eq!(context.turns_history.len(), before);
    f.prepare(&mut context).await.unwrap();
    context.maybe_compact_history();
    assert!(context.turns_history.len() < before);
    assert!(!context
        .turns_history
        .iter()
        .any(|message| source_key(&context, message).is_some()));
    let projected = context
        .assemble_messages()
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(projected.contains("可见按钮在输入区下方") && projected.contains("原图回看副本"));
    assert!(!history_requires_vision(&context));
    assert!(!f.prepare(&mut context).await.unwrap());
    let cp = AgentCheckpoint::new("fixture".into(), &context);
    let mut restored = SandwichContext::new("sys".into(), "继续".into());
    cp.restore_context(&mut restored);
    assert!(restored
        .assemble_messages()
        .iter()
        .any(|m| m.content.to_string().contains("可见按钮在输入区下方")));
    // 另一独立任务不能仅因误带回执映射而得到无关图片观察。
    let mut unrelated = SandwichContext::new("sys".into(), "其他任务".into());
    unrelated.image_observations = context.image_observations.clone();
    assert!(project_history(&unrelated).is_empty());
}

#[tokio::test]
async fn completed_chat_history_inherits_observations_without_reobserving_old_images() {
    let f = Fixture::new(vec![frame("旧附件的可见事实", "stop")]).await;
    let mut old = context();
    f.prepare(&mut old).await.unwrap();
    let cp = AgentCheckpoint::new("fixture".into(), &old);
    let mut fresh = SandwichContext::new("sys".into(), "下一问题".into());
    fresh.turns_history.push(ChatMessage {
        role: "user".into(),
        content: json!("新用户问题"),
    });
    cp.data.inherit_filling_into(&mut fresh);
    assert!(!history_requires_vision(&fresh));
    assert!(!f.prepare(&mut fresh).await.unwrap());
    assert_eq!(
        image_count(&fresh.turns_history),
        2,
        "跨任务继承仍不改写原始历史"
    );
    assert_eq!(image_count(&fresh.assemble_messages()), 0);
    assert_eq!(f.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn history_estimate_counts_sent_projection_and_config_failure_keeps_originals() {
    let f = Fixture::new(vec![frame("简短可见事实", "stop")]).await;
    let mut context = context();
    use base64::Engine;
    context.add_image_feedback(
        "大附件",
        &format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(vec![3u8; 100_000])
        ),
    );
    f.prepare(&mut context).await.unwrap();
    assert!(
        context.history_estimated_tokens() < 5000,
        "已移出请求的 base64 不应继续触发压缩"
    );
    let prefix = serde_json::to_value(project_history(&context)).unwrap();
    context.turns_history.push(ChatMessage {
        role: "user".into(),
        content: json!("后续信息"),
    });
    let projected = serde_json::to_value(project_history(&context)).unwrap();
    assert_eq!(
        &projected.as_array().unwrap()[..prefix.as_array().unwrap().len()],
        prefix.as_array().unwrap().as_slice()
    );
    let mut missing = self::context();
    let (_sender, mut rx) = watch::channel(0);
    let mut routing = f.routing();
    routing.vision = "deleted-model".into();
    assert!(prepare(
        &f.data.db,
        &mut missing,
        &routing,
        "fixture",
        0,
        &f.data.root.join("images"),
        &mut rx
    )
    .await
    .unwrap());
    assert_eq!(image_count(&missing.assemble_messages()), 2);
    assert!(!prepare(
        &f.data.db,
        &mut missing,
        &routing,
        "fixture",
        0,
        &f.data.root.join("images"),
        &mut rx
    )
    .await
    .unwrap());
    assert_eq!(
        f.requests.lock().unwrap().len(),
        1,
        "配置失败未实际调用，不应创建虚假调用"
    );
}
