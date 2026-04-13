//! Echo Agent CLI - AI Agent 命令行与 Web 服务
//!
//! 提供两种交互模式：
//! - **Web 模式**: 启动 HTTP/WebSocket 服务，提供完整的 REST API
//! - **CLI 模式**: 启动交互式命令行界面，支持 REPL 对话
//!
//! # 快速开始
//!
//! ```bash
//! # 仅启动 Web 服务（默认）
//! echo-agent-cli
//!
//! # 仅启动 CLI 交互
//! echo-agent-cli --cli
//!
//! # 同时启动 Web 服务和 CLI 交互
//! echo-agent-cli --web --cli
//!
//! # 指定端口
//! echo-agent-cli --web --port 8080
//! ```
//!
//! # 命令行选项
//!
//! | 选项 | 说明 |
//! |------|------|
//! | `--web` | 启动 Web 服务 |
//! | `--cli` | 启动命令行交互 |
//! | `--port <PORT>` | Web 服务端口 |
//! | `--host <HOST>` | Web 服务地址 |
//! | `--model <MODEL>` | 使用的模型名称 |
//! | `--no-color` | 禁用彩色输出 |
//! | `-h, --help` | 显示帮助信息 |
//! | `-V, --version` | 显示版本信息 |

use axum::{
    routing::{delete, get, post},
    Router,
};
use clap::Parser;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use echo_agent::memory::conversation::ConversationStore;
use echo_agent::memory::SqliteConversationStore;
use echo_agent::prelude::*;

mod cli;
mod error;
mod persistence;
mod routes;
mod state;
mod types;
mod ws;

// ── 命令行参数 ─────────────────────────────────────────────────────

/// Echo Agent CLI - AI Agent 命令行与 Web 服务
#[derive(Parser, Debug)]
#[command(name = "echo-agent-cli")]
#[command(version = "1.0.0")]
#[command(about = "AI Agent 命令行与 Web 服务", long_about = None)]
struct Args {
    /// 启动 Web 服务
    #[arg(long, default_value_t = false)]
    web: bool,

    /// 启动命令行交互
    #[arg(long, short = 'i', default_value_t = false)]
    cli: bool,

    /// Web 服务端口
    #[arg(long, short = 'p', default_value = "3000")]
    port: u16,

    /// Web 服务地址
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// 模型名称
    #[arg(long, short = 'm', env = "MODEL_NAME", default_value = "qwen-plus")]
    model: String,

    /// 系统提示词
    #[arg(long, short = 's', env = "SYSTEM_PROMPT", default_value = "你是一个智能助手，可以帮助用户回答问题、执行任务。")]
    system_prompt: String,

    /// MCP 配置文件路径
    #[arg(long, env = "MCP_CONFIG_PATH")]
    mcp_config: Option<String>,

    /// 禁用彩色输出
    #[arg(long)]
    no_color: bool,
}

// ── 主入口 ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenv::dotenv().ok();

    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    init_logging();

    // 创建 Agent
    let agent = create_agent(&args);
    let agent_arc = Arc::new(Mutex::new(agent));

    // 决定运行模式
    let run_web = args.web || (!args.web && !args.cli);
    let run_cli = args.cli;

    // 同时运行 Web 和 CLI
    if run_web && run_cli {
        run_both_modes(agent_arc, &args).await?;
    } else if run_cli {
        // 仅 CLI 模式
        run_cli_mode(agent_arc).await?;
    } else {
        // 仅 Web 模式
        run_web_mode(agent_arc, &args).await?;
    }

    Ok(())
}

/// 初始化日志系统
fn init_logging() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "echo_agent_cli=info,echo_agent=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// 创建 Agent 实例
fn create_agent(args: &Args) -> ReactAgent {
    let config = AgentConfig::standard(&args.model, "cli-assistant", &args.system_prompt)
        .enable_tool(true)
        .enable_memory(true)
        .enable_human_in_loop(true)
        .max_iterations(10);

    ReactAgent::new(config)
}

/// 加载 MCP 配置并连接服务端
async fn load_mcp_config(agent: &mut ReactAgent) {
    // 1. 尝试从环境变量指定的路径加载
    let config_path = std::env::var("MCP_CONFIG_PATH")
        .ok()
        .map(|p| std::path::PathBuf::from(p));

    // 2. 尝试默认路径
    let default_paths = [
        std::path::PathBuf::from("mcp.json"),
        std::path::PathBuf::from(".echo-agent/mcp.json"),
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
            .join(".echo-agent/mcp.json"),
    ];

    let config_path = config_path.or_else(|| {
        default_paths.iter().find(|p| p.exists()).cloned()
    });

    if let Some(path) = config_path {
        tracing::info!("加载 MCP 配置: {}", path.display());
        match agent.load_mcp_from_file(&path).await {
            Ok(clients) => {
                tracing::info!("MCP 服务端连接成功: {} 个", clients.len());
            }
            Err(e) => {
                tracing::warn!("MCP 配置加载失败: {}", e);
            }
        }
    } else {
        tracing::info!("未找到 MCP 配置文件，跳过 MCP 连接");
    }
}

/// 创建对话持久化 Store（SQLite）
fn create_conversation_store() -> Arc<dyn ConversationStore> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = std::path::PathBuf::from(home)
        .join(".echo-agent")
        .join("conversations.db");

    match SqliteConversationStore::new(&db_path) {
        Ok(store) => {
            tracing::info!("ConversationStore (SQLite) 初始化: {}", db_path.display());
            Arc::new(store)
        }
        Err(e) => {
            tracing::error!("ConversationStore 初始化失败: {e}, 使用内存回退");
            // Fallback: we still need a store, panic is not ideal
            panic!("无法初始化 SQLite ConversationStore: {e}");
        }
    }
}

/// 注入 ConversationStore 到 Agent
fn inject_conversation_store(
    agent: &Arc<Mutex<ReactAgent>>,
    store: &Arc<dyn ConversationStore>,
) {
    if let Ok(mut agent) = agent.try_lock() {
        agent.set_conversation_store(store.clone());
    }
}

/// 运行 Web 模式
async fn run_web_mode(agent: Arc<Mutex<ReactAgent>>, args: &Args) -> anyhow::Result<()> {
    // 加载 MCP 配置
    {
        let mut agent = agent.lock().await;
        load_mcp_config(&mut agent).await;
    }

    let conversation_store = create_conversation_store();
    inject_conversation_store(&agent, &conversation_store);

    let state = Arc::new(state::AppState::from_shared(agent, conversation_store));
    let app = build_router(state);
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    print_web_startup_info(&addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// 运行 CLI 模式
async fn run_cli_mode(agent: Arc<Mutex<ReactAgent>>) -> anyhow::Result<()> {
    // 加载 MCP 配置
    {
        let mut agent = agent.lock().await;
        load_mcp_config(&mut agent).await;
    }

    let config = cli::ReplConfig::default();
    cli::run_repl(agent, config).await
}

/// 同时运行 Web 和 CLI 模式
async fn run_both_modes(agent: Arc<Mutex<ReactAgent>>, args: &Args) -> anyhow::Result<()> {
    // 加载 MCP 配置
    {
        let mut agent = agent.lock().await;
        load_mcp_config(&mut agent).await;
    }

    let conversation_store = create_conversation_store();
    inject_conversation_store(&agent, &conversation_store);

    let state = Arc::new(state::AppState::from_shared(agent.clone(), conversation_store));
    let app = build_router(state);
    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    print_both_startup_info(&addr);

    // 在后台启动 Web 服务
    let web_handle = tokio::spawn(async move {
        axum::serve(listener, app).await
    });

    // 在前台运行 CLI
    let config = cli::ReplConfig::default();
    cli::run_repl(agent, config).await?;

    // CLI 退出后，停止 Web 服务
    web_handle.abort();

    Ok(())
}

/// 构建路由
fn build_router(state: Arc<state::AppState>) -> Router {
    Router::new()
        // 静态文件服务（前端）
        .nest_service("/static", ServeDir::new("web-frontend/dist"))
        // ── 对话 API ─────────────────────────────────────────────
        .route("/api/chat", post(routes::chat::handle_chat))
        .route("/api/history", get(routes::history::get_history))
        .route("/api/history/export", get(routes::history::export_history))
        // ── 会话 API ─────────────────────────────────────────────
        .route("/api/session", get(routes::session::get_session))
        .route("/api/session/reset", post(routes::session::reset_session))
        // ── 工具 API ─────────────────────────────────────────────
        .route("/api/tools/:name/enable", post(routes::tools::enable_tool))
        .route("/api/tools/:name/disable", post(routes::tools::disable_tool))
        .route("/api/tools/:name", get(routes::tools::get_tool))
        .route("/api/tools", get(routes::tools::list_tools))
        // ── Skill API ─────────────────────────────────────────────
        .route("/api/skills/load", post(routes::skills::load_skills_from_dir))
        .route("/api/skills/:name", get(routes::skills::get_skill))
        .route("/api/skills", get(routes::skills::list_skills))
        // ── MCP API ─────────────────────────────────────────────
        .route("/api/mcp/connect", post(routes::mcp::connect_mcp_server))
        .route("/api/mcp/:name/disconnect", post(routes::mcp::disconnect_mcp_server))
        .route("/api/mcp/:name", get(routes::mcp::get_mcp_server))
        .route("/api/mcp", get(routes::mcp::list_mcp_servers))
        // ── 配置 API ─────────────────────────────────────────────
        .route("/api/config", get(routes::config::get_config).put(routes::config::update_config))
        // ── 上下文 API ─────────────────────────────────────────────
        .route("/api/context", get(routes::context::get_context))
        // ── 压缩 API ─────────────────────────────────────────────
        .route("/api/compress", post(routes::compress::compress))
        .route("/api/compress/stats", get(routes::compress::get_compression_stats))
        // ── 记忆 API ─────────────────────────────────────────────
        .route("/api/memory", post(routes::memory::add_memory).get(routes::memory::get_memory))
        .route("/api/memory/search", post(routes::memory::search_memory))
        .route("/api/memory/delete", post(routes::memory::delete_memory))
        .route("/api/memory/namespaces", get(routes::memory::list_namespaces))
        // ── 结构化输出 API ─────────────────────────────────────────────
        .route("/api/extract", post(routes::extract::extract))
        .route("/api/extract/validate", post(routes::extract::validate_schema))
        .route("/api/extract/examples", get(routes::extract::get_examples))
        // ── WebSocket 流式对话 ─────────────────────────────────────────────
        .route("/ws/chat", get(ws::handler::ws_chat_handler))
        // ── 权限 API ─────────────────────────────────────────────
        .route("/api/permissions/mode", get(routes::permissions::get_permission_mode).put(routes::permissions::set_permission_mode))
        .route("/api/permissions/rules", get(routes::permissions::list_permission_rules).post(routes::permissions::add_permission_rule))
        .route("/api/permissions/rules/:name", delete(routes::permissions::remove_permission_rule))
        // ── 工作流 API ─────────────────────────────────────────────
        .route("/api/workflow", get(routes::workflow::list_workflows).post(routes::workflow::create_workflow))
        .route("/api/workflow/:id", get(routes::workflow::get_workflow).delete(routes::workflow::delete_workflow))
        .route("/api/workflow/:id/execute", post(routes::workflow::execute_workflow))
        // ── 审计 API ─────────────────────────────────────────────
        .route("/api/audit/logs", get(routes::audit::get_audit_logs).delete(routes::audit::clear_audit_logs))
        .route("/api/audit/stats", get(routes::audit::get_audit_stats))
        // ── 沙箱 API ─────────────────────────────────────────────
        .route("/api/sandbox/status", get(routes::sandbox::get_sandbox_status))
        .route("/api/sandbox/config", get(routes::sandbox::get_sandbox_config).put(routes::sandbox::update_sandbox_config))
        .route("/api/sandbox/execute", post(routes::sandbox::execute_in_sandbox))
        // ── 会话快照 API ─────────────────────────────────────────────
        .route("/api/session/checkpoint", post(routes::session::create_checkpoint))
        .route("/api/session/checkpoints", get(routes::session::list_checkpoints))
        .route("/api/session/restore/:snapshot_id", post(routes::session::restore_checkpoint))
        // ── 对话历史持久化 API ─────────────────────────────────────────────
        .route("/api/conversations", get(routes::conversations::list_conversations).post(routes::conversations::save_conversation))
        .route("/api/conversations/:id", get(routes::conversations::get_conversation).put(routes::conversations::update_conversation).delete(routes::conversations::delete_conversation))
        .route("/api/conversations/:id/export", get(routes::conversations::export_conversation))
        .route("/api/conversations/:id/restore", post(routes::conversations::restore_conversation))
        // 中间件
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// 打印 Web 模式启动信息
fn print_web_startup_info(addr: &str) {
    tracing::info!("🚀 Echo Agent CLI (Web 模式)");
    tracing::info!("✅ 服务已启动: http://{}", addr);
    tracing::info!("📖 API 端点:");
    tracing::info!("   POST /api/chat          - 阻塞式对话");
    tracing::info!("   GET  /api/history       - 获取对话历史");
    tracing::info!("   POST /api/compress      - 触发上下文压缩");
    tracing::info!("   POST /api/memory        - 添加记忆");
    tracing::info!("   POST /api/memory/search - 搜索记忆");
    tracing::info!("   POST /api/extract       - 结构化输出");
    tracing::info!("   WS   /ws/chat           - WebSocket 流式对话");
}

/// 打印双模式启动信息
fn print_both_startup_info(addr: &str) {
    tracing::info!("🚀 Echo Agent CLI (Web + CLI 模式)");
    tracing::info!("✅ Web 服务: http://{}", addr);
    tracing::info!("✅ CLI 交互: 已启动");
    tracing::info!("💡 输入 /help 查看命令帮助");
}

// ── 单元测试 ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_agent_config() {
        let args = Args {
            web: false,
            cli: false,
            port: 3000,
            host: "0.0.0.0".to_string(),
            model: "test-model".to_string(),
            system_prompt: "test prompt".to_string(),
            mcp_config: None,
            no_color: false,
        };

        let agent = create_agent(&args);
        assert_eq!(agent.model_name(), "test-model");
    }

    #[test]
    fn test_args_default() {
        let args = Args::parse_from(["echo-agent-cli"]);
        assert!(!args.web);
        assert!(!args.cli);
        assert_eq!(args.port, 3000);
        assert_eq!(args.model, "qwen-plus");
    }

    #[test]
    fn test_args_cli_mode() {
        let args = Args::parse_from(["echo-agent-cli", "--cli"]);
        assert!(args.cli);
        assert!(!args.web);
    }

    #[test]
    fn test_args_both_modes() {
        let args = Args::parse_from(["echo-agent-cli", "--web", "--cli"]);
        assert!(args.web);
        assert!(args.cli);
    }

    #[test]
    fn test_args_custom_port() {
        let args = Args::parse_from(["echo-agent-cli", "--port", "8080"]);
        assert_eq!(args.port, 8080);
    }
}