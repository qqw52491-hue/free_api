use crate::agent::tools::role_manager;
use crate::db::DbState;
use serde_json::Value;
use tauri::State;

/// 角色卡列表（含禁用项，供管理面板展示）
#[tauri::command]
pub async fn list_role_cards(state: State<'_, DbState>) -> Result<Vec<role_manager::RoleCard>, String> {
    role_manager::list_cards(state.inner(), true)
}

/// 新建或整卡更新角色卡
#[tauri::command]
pub async fn upsert_role_card(state: State<'_, DbState>, card: Value) -> Result<role_manager::RoleCard, String> {
    role_manager::upsert_card(state.inner(), &card)
}

/// 启用/禁用切换
#[tauri::command]
pub async fn toggle_role_card(state: State<'_, DbState>, name: String) -> Result<String, String> {
    role_manager::toggle_card(state.inner(), &Value::String(name))
}

/// 删除角色卡
#[tauri::command]
pub async fn delete_role_card(state: State<'_, DbState>, name: String) -> Result<String, String> {
    role_manager::delete_card(state.inner(), &Value::String(name))
}
