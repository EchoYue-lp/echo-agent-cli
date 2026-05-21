//! Echo Agent Tauri 桌面应用入口
//!
//! 独立 binary，通过 `cargo run --bin echo-agent-tauri` 启动。

use echo_agent_cli::cli;
use echo_agent_cli::config;
use echo_agent_cli::agent_handle::AgentHandle;
use echo_agent_cli::infra;
use echo_agent_cli::persistence::Persistence;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(64 * 1024 * 1024)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = cli::Args::parse();

    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

    infra::init_logging(&app_config.logging.level);

    let mut agent = infra::create_agent(&args, &app_config);
    infra::load_mcp_config(&mut agent, args.mcp_config.as_deref(), &app_config).await;

    if app_config.has_compressor() {
        app_config.apply_compressor(&agent).await;
    }

    let agent_handle = AgentHandle::new(agent);
    infra::load_user_hooks(&agent_handle, &app_config).await;
    infra::fire_startup_hook(&agent_handle).await;

    let persistence = Persistence::new();

    // Build and run the Tauri application
    let builder = echo_agent_cli::tauri::build_tauri(
        agent_handle,
        persistence,
        app_config,
    );

    builder
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri error: {}", e))
}
