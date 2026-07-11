use crate::tauri::state::TauriState;
use echo_agent::prelude::ToolParameters;
use echo_agent_app_core::browser::BrowserAction;
use serde_json::Value;
use std::collections::HashMap;
use tauri::State;

async fn execute(
    state: &TauriState,
    conversation_id: String,
    action: BrowserAction,
    params: ToolParameters,
) -> Result<(), String> {
    state
        .browser_runtime
        .execute_main(conversation_id, action, params, None)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn browser_navigate(
    state: State<'_, TauriState>,
    conversation_id: String,
    url: String,
) -> Result<(), String> {
    execute(
        &state,
        conversation_id,
        BrowserAction::Navigate,
        HashMap::from([("url".to_string(), Value::String(url))]),
    )
    .await
}

#[tauri::command]
pub async fn browser_back(
    state: State<'_, TauriState>,
    conversation_id: String,
) -> Result<(), String> {
    execute(&state, conversation_id, BrowserAction::Back, HashMap::new()).await
}

#[tauri::command]
pub async fn browser_reload(
    state: State<'_, TauriState>,
    conversation_id: String,
) -> Result<(), String> {
    execute(
        &state,
        conversation_id,
        BrowserAction::Reload,
        HashMap::new(),
    )
    .await
}

#[tauri::command]
pub async fn browser_screenshot(
    state: State<'_, TauriState>,
    conversation_id: String,
) -> Result<(), String> {
    execute(
        &state,
        conversation_id,
        BrowserAction::Screenshot,
        HashMap::new(),
    )
    .await
}

#[tauri::command]
pub async fn browser_tabs(
    state: State<'_, TauriState>,
    conversation_id: String,
    action: String,
    index: Option<u64>,
    url: Option<String>,
) -> Result<(), String> {
    let mut params = HashMap::from([("action".to_string(), Value::String(action))]);
    if let Some(index) = index {
        params.insert("index".to_string(), Value::Number(index.into()));
    }
    if let Some(url) = url {
        params.insert("url".to_string(), Value::String(url));
    }
    execute(&state, conversation_id, BrowserAction::Tabs, params).await
}

#[tauri::command]
pub async fn browser_stop(state: State<'_, TauriState>) -> Result<(), String> {
    state.browser_runtime.interrupt().await;
    Ok(())
}
