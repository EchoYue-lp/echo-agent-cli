use crate::tauri::state::TauriState;
use echo_agent::prelude::ToolParameters;
use echo_agent_app_core::browser::{BrowserAction, BrowserExtensionStatus};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Command;
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
pub async fn browser_click_at(
    state: State<'_, TauriState>,
    conversation_id: String,
    x: f64,
    y: f64,
) -> Result<(), String> {
    execute(
        &state,
        conversation_id,
        BrowserAction::ClickAt,
        HashMap::from([
            ("x".to_string(), json!(x)),
            ("y".to_string(), json!(y)),
            ("effect".to_string(), Value::String("none".to_string())),
        ]),
    )
    .await
}

#[tauri::command]
pub async fn browser_scroll(
    state: State<'_, TauriState>,
    conversation_id: String,
    delta_x: f64,
    delta_y: f64,
) -> Result<(), String> {
    execute(
        &state,
        conversation_id,
        BrowserAction::Scroll,
        HashMap::from([
            ("deltaX".to_string(), json!(delta_x)),
            ("deltaY".to_string(), json!(delta_y)),
        ]),
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

#[tauri::command]
pub async fn chrome_setup_status(
    state: State<'_, TauriState>,
) -> Result<BrowserExtensionStatus, String> {
    Ok(state.browser_runtime.extension_status().await)
}

#[tauri::command]
pub fn chrome_open_extensions_page() -> Result<(), String> {
    open_playwright_extension_page()
}

#[tauri::command]
pub async fn browser_set_backend(
    state: State<'_, TauriState>,
    conversation_id: String,
    backend: String,
) -> Result<(), String> {
    let params = HashMap::from([("backend".to_string(), Value::String(backend))]);
    execute(&state, conversation_id, BrowserAction::Backend, params).await
}

fn open_playwright_extension_page() -> Result<(), String> {
    const PLAYWRIGHT_EXTENSION_URL: &str = "https://chromewebstore.google.com/detail/playwright-extension/mmlmfjhmonkocbjadbfplnigmagldckm";

    #[cfg(target_os = "macos")]
    let child = Command::new("open").arg(PLAYWRIGHT_EXTENSION_URL).spawn();
    #[cfg(target_os = "linux")]
    let child = Command::new("xdg-open")
        .arg(PLAYWRIGHT_EXTENSION_URL)
        .spawn();
    #[cfg(target_os = "windows")]
    let child = Command::new("cmd")
        .args(["/C", "start", "", PLAYWRIGHT_EXTENSION_URL])
        .spawn();

    child
        .map(|_| ())
        .map_err(|error| format!("failed to open Playwright Extension page: {error}"))
}
