use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

mod chat_sessions;
mod chat_messages;
mod chat_send;
mod chat_misc;
mod chat_artifacts;
pub(crate) mod attachment_storage;

pub use chat_sessions::*;
pub use chat_messages::*;
pub use chat_send::*;
pub use chat_misc::*;
pub use chat_artifacts::*;
pub use attachment_storage::*;


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_safe_artifact_path_for_session_tmp() {
        // 测试解析真实存在的 tmp 相对路径
        let res = resolve_safe_artifact_path("tmp/beijing_weather_5days.xlsx");
        if res.is_ok() {
            let path = res.unwrap();
            assert!(path.is_absolute());
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("beijing_weather_5days.xlsx"));
        }

        // 测试纯文件名
        let res_file = resolve_safe_artifact_path("beijing_weather_5days.xlsx");
        if res_file.is_ok() {
            let path = res_file.unwrap();
            assert!(path.is_absolute());
            assert!(path.exists());
        }

        // 测试不存在的虚拟路径
        let res_err = resolve_safe_artifact_path("tmp/absolutely_non_existent_file_12345.xyz");
        assert!(res_err.is_err());
        assert!(res_err.unwrap_err().contains("文件不存在"));
    }
}
