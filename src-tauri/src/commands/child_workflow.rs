use crate::agent::child_task::defaults::ChildWorkflowDefaults;
use tauri::Manager;

pub(crate) fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|p| p.join("child-workflow-defaults.json"))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn get_child_workflow_defaults(app: tauri::AppHandle) -> Result<ChildWorkflowDefaults, String> {
    ChildWorkflowDefaults::load(&path(&app)?)
}
#[tauri::command]
pub fn save_child_workflow_defaults(
    app: tauri::AppHandle,
    config: ChildWorkflowDefaults,
) -> Result<(), String> {
    config.save(&path(&app)?)
}
