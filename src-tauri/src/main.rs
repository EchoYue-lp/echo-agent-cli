//! Echo Agent Tauri 桌面应用入口
//!
//! 独立 binary，通过 `cargo run --bin echo-agent-tauri` 启动。
//! 同时启动 Web 服务供前端 Vite proxy 转发 API 请求。

use echo_agent_cli::cli;
use echo_agent_cli::config;
use echo_agent_cli::agent_handle::AgentHandle;
use echo_agent_cli::infra;
use echo_agent_cli::persistence::Persistence;
use echo_agent_cli::state;

use clap::Parser;
use std::sync::Arc;

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

    // Clone agent for the web server (AgentHandle is cheap Clone via Arc)
    let web_agent = agent_handle.clone();
    let web_config = app_config.clone();

    // Spawn the Web API server in background so the frontend can use HTTP/WS
    let server_agent = web_agent.clone();
    let server_config = web_config.clone();
    tokio::spawn(async move {
        let conversation_store = infra::create_conversation_store();
        infra::inject_conversation_store(&server_agent, &conversation_store);

        let state = Arc::new({
            let mut s = state::AppState::from_shared(
                server_agent.clone(),
                conversation_store,
                server_config.clone(),
            );
            s.start_scheduler();
            s
        });

        let app = cli::router::build_router(state).await;
        let addr = format!("{}:{}", server_config.server.host, server_config.server.port);
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to bind web server: {e}");
                return;
            }
        };

        tracing::info!("Tauri: Web API server listening on http://{addr}");
        let _ = axum::serve(listener, app).await;
    });

    // Build and run the Tauri application (blocks until window closes)
    let persistence = Persistence::new();
    let builder = echo_agent_cli::tauri::build_tauri(
        agent_handle,
        persistence,
        app_config,
    );

    builder
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri error: {}", e))
}
