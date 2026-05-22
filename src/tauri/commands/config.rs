//! 配置相关 Tauri 命令

use super::super::state::TauriState;
use tauri::State;

#[tauri::command]
pub async fn get_config(state: State<'_, TauriState>) -> Result<serde_json::Value, String> {
    let guard = state.agent.inner().read().await;
    let model = guard
        .llm_config()
        .map(|c| c.model.clone())
        .unwrap_or_default();
    Ok(serde_json::json!({
        "model": model,
        "tool_count": guard.tool_names().len(),
        "mcp_servers": guard.mcp_server_names(),
        "skills": guard.skill_names(),
    }))
}

#[tauri::command]
pub async fn update_config(
    state: State<'_, TauriState>,
    model: Option<String>,
    max_iterations: Option<usize>,
) -> Result<(), String> {
    let mut guard = state.agent.inner().write().await;
    if let Some(m) = model {
        if let Some(cfg) = guard.llm_config().cloned() {
            let mut new_cfg = cfg;
            new_cfg.model = m;
            guard.set_llm_config(new_cfg);
        }
    }
    if let Some(n) = max_iterations {
        guard.set_max_iterations(n);
    }
    Ok(())
}
