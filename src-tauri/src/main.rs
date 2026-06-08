//! EchoCoWork — Tauri desktop entry point.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    echo_agent_cli::tauri::desktop::run_desktop_entry().await
}
