//! 技能管理命令

use super::super::state::TauriState;
use tauri::State;

#[tauri::command]
pub async fn list_skills(state: State<'_, TauriState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.agent.inner().read().await;
    let skills: Vec<serde_json::Value> = guard
        .skill_names()
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name.to_string(),
                "description": "",
            })
        })
        .collect();
    Ok(skills)
}

#[tauri::command]
pub async fn load_skills(
    _state: State<'_, TauriState>,
    _dir_path: String,
) -> Result<Vec<String>, String> {
    // Skill loading via direct agent API not available on this branch.
    // Use Web API or configure skills in echo-agent.yaml.
    Err("Skill loading via IPC not yet available — use Web API".into())
}
