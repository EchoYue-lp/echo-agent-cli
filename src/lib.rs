pub mod cli;
pub mod logging;
pub mod task_run_control;

/// Configure EKO's process-wide data roots before any store is opened.
/// `EKO_DATA_DIR` is primarily used by subprocess tests and local development;
/// production keeps the branded `~/.eko` default.
pub fn configure_data_root() -> anyhow::Result<()> {
    if let Some(root) = std::env::var_os("EKO_DATA_DIR") {
        let root = std::path::PathBuf::from(root);
        if !root.is_absolute() {
            anyhow::bail!("EKO_DATA_DIR must be an absolute path: {}", root.display());
        }
        echo_agent_app_core::api::data_root::configure(root.clone()).map_err(|current| {
            anyhow::anyhow!(
                "EKO data root was already initialized at {}",
                current.display()
            )
        })?;
        return Ok(());
    }
    echo_agent_app_core::api::data_root::configure(
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".eko"),
    )
    .map_err(|current| {
        anyhow::anyhow!(
            "EKO data root was already initialized at {}",
            current.display()
        )
    })?;
    Ok(())
}

#[cfg(feature = "gui")]
// `#[tauri::command]` expands to dispatch glue containing `unreachable!()`.
// Keep the exception at the generated IPC boundary; application/core code
// remains covered by the workspace's strict panic lint gate.
#[allow(clippy::unreachable)]
pub mod tauri;

#[cfg(feature = "tui")]
pub mod tui;

// Re-export from app-core
pub use echo_agent_app_core::api::{
    AppState, agent_handle, config_watcher, error, infra, output, profiles, project, scheduler,
    skills_hub, state, tasks, types, webhook,
};
