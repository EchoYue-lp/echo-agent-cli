//! Echo Agent — Tauri desktop entry point.
//!
//! The Tauri window is a thin shell around the Axum web server.
//! The frontend (React) talks to the server via HTTP, not IPC.

use clap::Parser;
use echo_agent_cli::{
    agent_handle::AgentHandle, cli, config, config_watcher, infra, state::AppState,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let args = cli::Args::parse_from(["echo-agent-tauri"]);
    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

    infra::init_logging(&app_config.logging.level);

    // ── Create agent + MCP ──
    let params = infra::AgentCreateParams {
        model: args.model.clone(),
        mode: args.mode.clone(),
        system_prompt: args.system_prompt.clone(),
        project: args.project.clone(),
    };
    let mut agent = infra::create_agent(&params, &app_config);
    infra::load_mcp_config(&mut agent, args.mcp_config.as_deref(), &app_config).await;

    // Configure auto-compression
    if app_config.has_compressor() {
        app_config.apply_compressor(&agent).await;
    }

    let agent_handle = AgentHandle::new(agent);

    // ── HITL dispatcher ──
    let hitl_dispatcher = {
        use echo_agent_app_core::hitl::HitlDispatcher;
        let dispatcher = Arc::new(HitlDispatcher::new());
        let repl_provider = Arc::new(echo_agent_app_core::hitl::ReplHumanLoopProvider::new());
        dispatcher.register("repl", repl_provider).await;
        agent_handle
            .write_async(|a| {
                let d = dispatcher.clone();
                Box::pin(async move { a.set_human_loop_provider(d) })
            })
            .await;
        dispatcher // Keep dispatcher reference for AppState
    };

    // ── Load hooks ──
    infra::load_user_hooks(&agent_handle, &app_config).await;
    let hooks_load = echo_agent_app_core::hooks_config::load_hooks_files();
    if !hooks_load.definition.is_empty() {
        let hooks_def = hooks_load.definition;
        agent_handle
            .write_async(|a| {
                Box::pin(async move {
                    let mut registry = a.hook_registry().write().await;
                    registry.clear_user_hooks();
                    registry.register_user_hooks(hooks_def);
                })
            })
            .await;
    }

    infra::fire_startup_hook(&agent_handle).await;

    // ── Config watcher ──
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if let Some(config_path) = config_watcher::resolve_config_path(args.config.as_deref()) {
        config_watcher::spawn_config_watcher(
            config_path,
            agent_handle.clone(),
            cancel_token.clone(),
        );
    }

    // ── Task store ──
    let task_store: Arc<dyn echo_agent::memory::Store> = {
        let db_path = echo_agent_app_core::persistence::Persistence::base_dir().join("tasks.db");
        match echo_agent::memory::SqliteStore::new(&db_path) {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(echo_agent::memory::InMemoryStore::new()),
        }
    };

    // ── Start Axum HTTP server (background) ──
    let conversation_store = infra::create_conversation_store();
    infra::inject_conversation_store(&agent_handle, &conversation_store);

    let mut state_inner = AppState::from_shared(
        agent_handle.clone(),
        hitl_dispatcher,
        conversation_store,
        app_config.clone(),
    );
    state_inner.start_task_service(task_store.clone()).await;
    state_inner.start_scheduler_with_store(Some(task_store));
    let state = Arc::new(state_inner);

    infra::spawn_mcp_health_check(state.clone(), cancel_token.clone());

    if let Err(e) = echo_agent_cli::metrics::init_metrics() {
        tracing::warn!("Failed to initialize metrics: {}", e);
    }

    echo_agent_cli::ws::handler::cleanup_stale_uploads().await;

    let app = cli::build_router(state.clone()).await;
    // Tauri desktop mode always binds to localhost for security
    let port = app_config.server.port;
    let addr = format!("127.0.0.1:{}", port);

    infra::print_web_startup_info(&addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let web_cancel = cancel_token.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { web_cancel.cancelled().await })
            .await
            .expect("Axum server error");
    });

    // ── Launch Tauri window ──
    echo_agent_cli::tauri::build_tauri_app()
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");

    // Tauri window closed → cancel background tasks
    cancel_token.cancel();

    Ok(())
}
