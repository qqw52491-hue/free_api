//! channel 路由表。

use super::*;

pub(crate) fn channel_router(state: Arc<ChannelState>) -> Router {
    Router::new()
        .route("/", get(handler_web_home))
        .route("/web", get(handler_web_home))
        .route("/favicon.ico", get(|| async { axum::http::StatusCode::NO_CONTENT }))
        // 自托管更新分发（updater 客户端不带 token，路由必须免鉴权；
        // 只读 app 数据目录 updates/ 下文件，文件名白名单化防穿越）
        .route("/update/latest.json", get(handler_update_manifest))
        .route("/update/files/{name}", get(handler_update_file))
        // 远端构建机发布端点（X-Channel-Token 鉴权）——Windows/Linux 机
        // 在本地 build 完把包+manifest 键推上来，本机即成分发源
        .route("/update/publish/files/{name}", post(handler_update_publish_file))
        .route("/update/publish/manifest", post(handler_update_publish_manifest))
        .route("/channel/projects", get(handler_projects))
        .route("/channel/sessions", get(handler_sessions))
        .route("/channel/session", post(handler_session))
        .route("/channel/session/{session_id}", delete(handler_delete_session))
        .route("/channel/task", post(handler_task))
        .route(
            "/channel/session/{session_id}/traces",
            get(handler_session_traces),
        )
        .route("/channel/trace/{trace_id}", get(handler_trace_detail))
        .route("/channel/runs", get(handler_run_list))
        .route("/channel/run/{run_id}", get(handler_run_status))
        .route("/channel/evals", get(handler_eval_list))
        .route("/channel/eval/{eval_id}", get(handler_eval_detail))
        .route("/channel/run/{run_id}/answer", post(handler_run_answer))
        .route("/channel/run/{run_id}/goal", post(handler_run_goal))
        .route("/channel/run/{run_id}/supplement", post(handler_run_supplement))
        .route("/channel/run/{run_id}/cancel", post(handler_run_cancel))
        .route("/channel/config", get(handler_config_get).put(handler_config_put))
        .route("/channel/models", get(handler_models))
        .route("/channel/role-cards", get(handler_role_cards))
        .route("/channel/events", get(handler_events))
        .route(
            "/channel/artifact/{artifact_id}/raw",
            get(handler_artifact_raw),
        )
        .route(
            "/channel/artifact/{artifact_id}/preview",
            get(handler_artifact_preview),
        )
        .route(
            "/channel/session/{session_id}/attachment",
            post(handler_session_attachment),
        )
        .route(
            "/channel/session/{session_id}/attachment-upload",
            post(handler_session_attachment_upload),
        )
        .route(
            "/channel/session/{session_id}/artifacts",
            get(handler_session_artifacts),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_UPLOAD_BYTES + 64 * 1024,
        ))
        .with_state(state)
}

// ---------------------------------------------------------------- 全局共享配置
// 页面保存配置走 Tauri 命令，而 axum 处理器读的是 ChannelState 里的快照；
// 两边必须共享同一份 Arc<RwLock<ChannelConfig>>，否则保存后运行中的服务仍在用
// 应用启动时加载的旧值（表现为「未配置默认模型」）。
