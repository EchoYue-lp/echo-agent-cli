//! 基础设施函数
//!
//! 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::memory::ConversationStore;
use echo_agent::memory::SqliteConversationStore;
use echo_agent::prelude::*;

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;

/// 创建 Agent 实例
pub fn create_agent(args: &crate::cli::Args, app_config: &AppConfig) -> ReactAgent {
    let model = args.model.as_deref().unwrap_or(&app_config.model.name);

    let agent_mode = crate::project::modes::AgentMode::from_str(&args.mode)
        .unwrap_or(crate::project::modes::AgentMode::General);

    let base_system_prompt = args
        .system_prompt
        .as_deref()
        .unwrap_or(&app_config.agent.system_prompt);

    let system_prompt = if base_system_prompt != app_config.agent.system_prompt {
        base_system_prompt.to_string()
    } else {
        agent_mode.system_prompt().to_string()
    };

    let system_prompt = if let Some(ref project_dir) = args.project {
        let project_root = std::path::Path::new(project_dir);
        if project_root.exists() {
            let ctx = crate::project::context::load_project_context(project_root);
            crate::project::context::build_system_prompt_with_context(&system_prompt, &ctx)
        } else {
            tracing::warn!("项目目录不存在: {}", project_dir);
            system_prompt
        }
    } else if let Some(project_root) = crate::project::context::discover_project_root(None) {
        let ctx = crate::project::context::load_project_context(&project_root);
        if !ctx.instructions.is_empty() {
            crate::project::context::build_system_prompt_with_context(&system_prompt, &ctx)
        } else {
            system_prompt
        }
    } else {
        system_prompt
    };

    // Use to_agent_config() which includes token_limit from YAML config
    let mut config = app_config.to_agent_config();
    // Override model if CLI arg is provided
    if args.model.is_some() {
        config = config.model_name(model);
    }
    // Override system_prompt with the resolved one (includes project context)
    config = config.system_prompt(&system_prompt);

    ReactAgent::new(config)
}

/// 加载 MCP 配置并连接服务端
pub async fn load_mcp_config(
    agent: &mut ReactAgent,
    mcp_cli_override: Option<&str>,
    app_config: &AppConfig,
) {
    // 优先级: CLI --mcp-config > YAML mcp.config_path > 环境变量 > 默认路径
    let config_path = mcp_cli_override
        .map(std::path::PathBuf::from)
        .or_else(|| {
            app_config
                .mcp
                .config_path
                .as_ref()
                .map(std::path::PathBuf::from)
        })
        .or_else(|| {
            std::env::var("MCP_CONFIG_PATH")
                .ok()
                .map(std::path::PathBuf::from)
        });

    // 默认路径（仅从用户目录加载，不从 CWD 加载以防止仓库注入）
    let default_paths = [
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
            .join(".echo-agent/mcp.json"),
    ];

    let config_path = config_path.or_else(|| default_paths.iter().find(|p| p.exists()).cloned());

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

/// 启动 MCP 后台健康检查任务
pub fn spawn_mcp_health_check(
    state: Arc<crate::state::AppState>,
    cancel: echo_agent::agent::CancellationToken,
) {
    tokio::spawn(async move {
        // 首次检查延迟 5 秒，等待 MCP 连接初始化完成
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("MCP health check task stopped");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                    state.run_mcp_health_check().await;
                }
            }
        }
    });
}

/// 创建对话持久化 Store（SQLite），失败时返回 None（禁用持久化）
pub fn create_conversation_store() -> Option<Arc<dyn ConversationStore>> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = std::path::PathBuf::from(home)
        .join(".echo-agent")
        .join("conversations.db");

    match SqliteConversationStore::new(&db_path) {
        Ok(store) => {
            tracing::info!("ConversationStore (SQLite) 初始化: {}", db_path.display());
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("ConversationStore 初始化失败: {e}, 禁用对话持久化");
            None
        }
    }
}

/// 注入 ConversationStore 到 Agent（可选，仅在 store 可用时注入）
pub fn inject_conversation_store(
    agent: &AgentHandle,
    store: &Option<Arc<dyn ConversationStore>>,
) {
    if let Some(store) = store {
        agent.try_write(|a| a.set_conversation_store(store.clone()));
    }
}

/// 优雅关闭信号
pub async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!("failed to install Ctrl+C handler: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!("failed to install SIGTERM handler: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("收到 Ctrl+C 信号，正在关闭..."),
        _ = terminate => tracing::info!("收到 SIGTERM 信号，正在关闭..."),
    }
}

/// 打印 Web 模式启动信息
pub fn print_web_startup_info(addr: &str) {
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
pub fn print_both_startup_info(addr: &str) {
    tracing::info!("🚀 Echo Agent CLI (Web + CLI 模式)");
    tracing::info!("✅ Web 服务: http://{}", addr);
    tracing::info!("✅ CLI 交互: 已启动");
    tracing::info!("💡 输入 /help 查看命令帮助");
}

/// 初始化日志系统（线程安全，仅执行一次）
///
/// When the `telemetry` feature is enabled, this delegates to
/// [`echo_agent::telemetry::init_telemetry`] which sets up OTLP tracing + metrics
/// configured via `OTEL_EXPORTER_OTLP_ENDPOINT` (defaults to `http://localhost:4317`).
pub fn init_logging(level: &str) {
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();

    INIT.get_or_init(|| {
        #[cfg(feature = "telemetry")]
        {
            let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4317".to_string());
            let service_name =
                std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "echo-agent-cli".to_string());

            let config = echo_agent::telemetry::TelemetryConfig {
                otlp_endpoint,
                service_name,
                enable_console: true,
            };
            // Use env filter matching the requested level
            // SAFETY: called during init before any other threads start
            unsafe {
                std::env::set_var(
                    "RUST_LOG",
                    format!("echo_agent_cli={level},echo_agent={level},tower_http=info"),
                );
            }
            let _ = echo_agent::telemetry::init_telemetry(config);
            return;
        }

        #[cfg(not(feature = "telemetry"))]
        {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            let default_filter =
                format!("echo_agent_cli={level},echo_agent={level},tower_http=info");
            let _ = tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                        tracing_subscriber::filter::EnvFilter::new(&default_filter)
                    }),
                )
                .with(tracing_subscriber::fmt::layer())
                .try_init();
        }
    });
}

// ── Doctor 诊断 ──────────────────────────────────────────────────

/// 诊断结果
pub struct DoctorResult {
    pub issues: Vec<String>,
    pub checks: Vec<String>,
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor() -> DoctorResult {
    let mut issues: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();

    let home = std::env::var("HOME").unwrap_or_default();

    let api_keys = [
        ("DASHSCOPE_API_KEY", "阿里通义千问"),
        ("QWEN_API_KEY", "通义千问 (别名)"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("DEEPSEEK_API_KEY", "DeepSeek"),
        ("ZHIPU_API_KEY", "智谱 GLM"),
        ("MOONSHOT_API_KEY", "月之暗面 Kimi"),
    ];
    let mut has_any_key = false;
    for (key, name) in &api_keys {
        if std::env::var(key).is_ok() {
            checks.push(format!("✅ API Key: {} ({})", name, key));
            has_any_key = true;
        }
    }
    if !has_any_key {
        issues.push("❌ 未检测到任何 LLM API Key, 请设置环境变量 (如 DASHSCOPE_API_KEY)".to_string());
    }

    let config_path = format!("{}/.echo-agent/echo-agent.yaml", home);
    if std::path::Path::new(&config_path).exists() {
        checks.push("✅ 配置文件: ~/.echo-agent/echo-agent.yaml".to_string());
    } else {
        issues.push("⚠️  未找到配置文件 ~/.echo-agent/echo-agent.yaml (使用默认配置)".to_string());
    }

    let mcp_path = format!("{}/.echo-agent/mcp.json", home);
    if std::path::Path::new(&mcp_path).exists() {
        checks.push("✅ MCP 配置: ~/.echo-agent/mcp.json".to_string());
    } else {
        checks.push("ℹ️  未找到 MCP 配置 (如需工具扩展可创建 ~/.echo-agent/mcp.json)".to_string());
    }

    let data_dir = format!("{}/.echo-agent", home);
    if std::path::Path::new(&data_dir).exists() {
        checks.push("✅ 数据目录: ~/.echo-agent/".to_string());
    } else {
        issues.push("⚠️  数据目录 ~/.echo-agent/ 不存在 (运行 echo-agent-cli onboard 初始化)".to_string());
    }

    let db_path = format!("{}/.echo-agent/conversations.db", home);
    if std::path::Path::new(&db_path).exists() {
        checks.push("✅ 对话数据库: ~/.echo-agent/conversations.db".to_string());
    } else {
        checks.push("ℹ️  对话数据库尚未创建 (首次对话后自动创建)".to_string());
    }

    if let Some(root) = crate::project::context::discover_project_root(None) {
        let ctx = crate::project::context::load_project_context(&root);
        if ctx.instructions.is_empty() {
            checks.push("ℹ️  项目目录已检测到, 但未找到指令文件 (AGENTS.md 等)".to_string());
        } else {
            checks.push(format!("✅ 项目指令: {} 个文件已加载", ctx.instructions.len()));
        }
    } else {
        checks.push("ℹ️  未检测到项目目录 (可在项目根目录创建 AGENTS.md)".to_string());
    }

    DoctorResult { issues, checks }
}

/// Load user hooks from YAML config into the agent's hook registry.
pub async fn load_user_hooks(agent: &AgentHandle, app_config: &AppConfig) {
    if app_config.hooks.is_empty() {
        return;
    }
    let hooks_def = app_config.hooks.clone();
    agent.write_async(|a| {
        Box::pin(async move {
            let mut registry = a.hook_registry().write().await;
            registry.register_user_hooks(hooks_def);
        })
    }).await;
    tracing::info!("User hooks loaded from config");
}

/// Fire SessionStart("startup") hook after hooks are loaded.
///
/// This is called once when the agent first starts up, after all hooks
/// (both skill hooks and user hooks) have been registered, so that
/// registered hooks can react to the startup event.
pub async fn fire_startup_hook(agent: &AgentHandle) {
    agent.read_async(|a| Box::pin(async move {
        let result = a.fire_lifecycle_hook(
            echo_agent::skills::hooks::HookEvent::SessionStart,
            Some("startup"),
        ).await;
        if result.block {
            tracing::warn!(reason = ?result.block_reason, "SessionStart hook blocked agent startup");
        }
    })).await;
    tracing::info!("SessionStart(\"startup\") hook fired");
}

/// 打印诊断结果
pub fn print_doctor_result(result: &DoctorResult) {
    println!();
    println!("╭─────────────────────────────────────────────────────────────╮");
    println!("│                    🏥 Echo Agent 诊断                        │");
    println!("╰─────────────────────────────────────────────────────────────╯");

    if !result.issues.is_empty() {
        println!("\n  ⚠️  问题:");
        for issue in &result.issues {
            println!("    {}", issue);
        }
    }

    println!("\n  检查项:");
    for check in &result.checks {
        println!("    {}", check);
    }

    if result.issues.is_empty() {
        println!("\n  ✅ 所有检查通过, Agent 运行正常");
    } else {
        println!("\n  发现 {} 个问题需要关注", result.issues.len());
    }
    println!();
}
