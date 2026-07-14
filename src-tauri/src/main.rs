//! EKO — Tauri desktop entry point.

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(echo_agent_cli::tauri::desktop::run_desktop_entry())
}
