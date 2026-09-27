//! axum HTTP 层：会话/任务/事件流/追踪/工件等 handler 与路由表。
//! 从 channel.rs 原样抽出，行为不变。

use super::*;
use rusqlite::OptionalExtension;

mod session_ops;
mod run_spawn;
mod data_queries;
mod run_control;
pub(crate) mod views;
mod upload;
mod router;

pub(crate) use session_ops::*;
pub(crate) use run_spawn::*;
pub(crate) use data_queries::*;
pub(crate) use run_control::*;
pub(crate) use views::*;
pub(crate) use upload::*;
pub(crate) use router::*;
fn db_conn<'a>(
    app: &'a AppHandle,
) -> Result<std::sync::MutexGuard<'a, rusqlite::Connection>, (StatusCode, Json<Value>)> {
    let state: tauri::State<'a, DbState> = app.state::<DbState>();
    let db: &'a DbState = state.inner();
    db.0.lock().map_err(internal_error)
}

fn bad_request(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn not_found(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message })))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

// ---------------------------------------------------------------- 网页控制台
// ZCode 风格的浏览器控制台：工作区/临时任务选择 → 会话列表 → 执行流实时视图。
// 模块化管理（HTML 骨架 / CSS 样式 / JS 逻辑独立分拆在 mobile_web/ 目录下，编译期零开销嵌入二进制）
pub(crate) const CHANNEL_WEB_PAGE: &str = concat!(
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover, maximum-scale=1, user-scalable=no">
<meta name="color-scheme" content="dark">
<title>free_api 远程控制</title>
<style>
"#,
    include_str!("mobile_web/style.css"),
    r#"
</style>
</head>
<body>
"#,
    include_str!("mobile_web/template.html"),
    r#"
<script>
"#,
    include_str!("mobile_web/app.js"),
    r#"
</script>
</body>
</html>"#
);

/// trace 文本字段在网页详情端点的截断上限（步骤可能带全量工具输出）
pub(crate) const TRACE_FIELD_TRUNCATE: usize = 6_000;

#[derive(Deserialize)]
pub(crate) struct PreviewQuery {
    token: Option<String>,
}

pub(crate) static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// 桥 watchdog 抑制标志：页面「停止桥」置位（用户主动关闭，不自动复活），
/// 手动「启动桥」复位。自动启动/看门狗均尊重该标志。
pub(crate) static BRIDGE_WATCHDOG_SUPPRESSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

