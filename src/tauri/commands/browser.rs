use crate::tauri::state::TauriState;
use echo_agent_app_core::api::browser::BrowserExtensionStatus;
use std::process::Command;
use tauri::State;

#[tauri::command]
pub async fn chrome_setup_status(
    state: State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<BrowserExtensionStatus, String> {
    let control = state
        .app_state
        .workspace_control_for_scope(&workspace_id)
        .await
        .map_err(|error| error.to_string())?;
    control
        .validate_generation(&workspace_generation)
        .map_err(|error| error.to_string())?;
    let runtime = control.runtime().clone();
    state
        .app_state
        .extension_control
        .browser_status_scoped(&state.app_state, Some(&runtime))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn chrome_open_extensions_page() -> Result<(), String> {
    open_playwright_extension_page()
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
