use crate::tauri::state::TauriState;
use echo_agent::prelude::ToolParameters;
use echo_agent_app_core::browser::BrowserAction;
use echo_agent_app_core::browser::chrome::CHROME_NATIVE_HOST_NAME;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use tauri::{AppHandle, Manager, State};

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeSetupStatus {
    pub enabled: bool,
    pub connected: bool,
    pub extension_origin: Option<String>,
    pub endpoint_file: String,
    pub startup_error: Option<String>,
    pub native_host_installed: bool,
    pub extension_path: Option<String>,
}

#[tauri::command]
pub async fn chrome_setup_status(
    app: AppHandle,
    state: State<'_, TauriState>,
) -> Result<ChromeSetupStatus, String> {
    let status = state.browser_runtime.chrome_status().await;
    let native_host_installed = chrome_native_host_manifest_path()
        .map(|path| path.is_file())
        .unwrap_or(false);
    Ok(ChromeSetupStatus {
        enabled: status.enabled,
        connected: status.connected,
        extension_origin: status.extension_origin,
        endpoint_file: status.endpoint_file.to_string_lossy().into_owned(),
        startup_error: status.startup_error,
        native_host_installed,
        extension_path: chrome_extension_dir(&app).map(|path| path.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub fn chrome_open_extensions_page() -> Result<(), String> {
    open_chrome_extensions_page()
}

#[tauri::command]
pub fn chrome_open_extension_dir(app: AppHandle) -> Result<(), String> {
    let extension_dir = chrome_extension_dir(&app)
        .ok_or_else(|| "bundled Chrome extension directory is unavailable".to_string())?;
    open_path(&extension_dir)
}

#[tauri::command]
pub async fn chrome_install_native_host(extension_id: String) -> Result<String, String> {
    validate_extension_id(&extension_id)?;
    let host_binary = chrome_native_host_binary()?;
    if !host_binary.is_file() {
        return Err(format!(
            "Chrome native host binary is missing at {}",
            host_binary.display()
        ));
    }
    let manifest_path = chrome_native_host_manifest_path()?;
    let parent = manifest_path
        .parent()
        .ok_or_else(|| "native host manifest path has no parent".to_string())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| error.to_string())?;
    let manifest = json!({
        "name": CHROME_NATIVE_HOST_NAME,
        "description": "EKO authorized Chrome tab bridge",
        "path": host_binary.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")],
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    tokio::fs::write(&manifest_path, bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(manifest_path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn browser_set_backend(
    state: State<'_, TauriState>,
    conversation_id: String,
    backend: String,
    tab_id: Option<u64>,
) -> Result<(), String> {
    let mut params = HashMap::from([("backend".to_string(), Value::String(backend))]);
    if let Some(tab_id) = tab_id {
        params.insert("tabId".to_string(), Value::Number(tab_id.into()));
    }
    execute(&state, conversation_id, BrowserAction::Backend, params).await
}

fn validate_extension_id(value: &str) -> Result<(), String> {
    if value.chars().count() == 32
        && value
            .chars()
            .all(|character| ('a'..='p').contains(&character))
    {
        Ok(())
    } else {
        Err("Chrome extension id must contain 32 lowercase characters from a to p".to_string())
    }
}

fn chrome_native_host_binary() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|error| error.to_string())
}

fn chrome_extension_dir(app: &AppHandle) -> Option<PathBuf> {
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|directory| directory.join("chrome-extension"));
    if bundled.as_ref().is_some_and(|directory| directory.is_dir()) {
        return bundled;
    }

    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("chrome-extension");
    development.is_dir().then_some(development)
}

fn open_path(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let child = Command::new("open").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let child = Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let child = Command::new("explorer").arg(path).spawn();

    child
        .map(|_| ())
        .map_err(|error| format!("failed to open {}: {error}", path.display()))
}

fn open_chrome_extensions_page() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let child = Command::new("open")
        .args(["-a", "Google Chrome", "chrome://extensions"])
        .spawn();
    #[cfg(target_os = "linux")]
    let child = Command::new("google-chrome")
        .arg("chrome://extensions")
        .spawn();
    #[cfg(target_os = "windows")]
    let child = Command::new("cmd")
        .args(["/C", "start", "", "chrome://extensions"])
        .spawn();

    child
        .map(|_| ())
        .map_err(|error| format!("failed to open Chrome extensions: {error}"))
}

fn chrome_native_host_manifest_path() -> Result<std::path::PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "home directory is unavailable".to_string())?;
    #[cfg(target_os = "macos")]
    return Ok(home
        .join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome")
        .join("NativeMessagingHosts")
        .join(format!("{CHROME_NATIVE_HOST_NAME}.json")));
    #[cfg(target_os = "linux")]
    return Ok(home
        .join(".config")
        .join("google-chrome")
        .join("NativeMessagingHosts")
        .join(format!("{CHROME_NATIVE_HOST_NAME}.json")));
    #[cfg(target_os = "windows")]
    Err("Windows native host registration requires an installer registry entry".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_chrome_extension_ids() {
        assert!(validate_extension_id("abcdefghijklmnopabcdefghijklmnop").is_ok());
        assert!(validate_extension_id("abcdefghijklmnopabcdefghijklmnopq").is_err());
        assert!(validate_extension_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }
}
