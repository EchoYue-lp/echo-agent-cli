//! EKO — Tauri desktop entry point.

fn main() -> anyhow::Result<()> {
    if echo_agent_cli::chrome_native_host::is_native_host_invocation() {
        return echo_agent_cli::chrome_native_host::run().map_err(anyhow::Error::msg);
    }
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(echo_agent_cli::tauri::desktop::run_desktop_entry())
}
