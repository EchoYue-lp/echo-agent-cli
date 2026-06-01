//! 基础设施函数
//!
//! 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::memory::ConversationStore;
use echo_agent::memory::SqliteConversationStore;
use echo_agent::prelude::*;

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::project::prompt::PromptAssembler;

/// Agent creation parameters (extracted from CLI args or config).
pub struct AgentCreateParams {
    pub model: Option<String>,
    pub mode: String,
    pub system_prompt: Option<String>,
    pub project: Option<String>,
}

/// 创建 Agent 实例
///
/// Uses `ReactAgentBuilder` with `.mode()` and `.mode_engine()` to
/// leverage the framework's mode auto-configuration (system prompt,
/// recommended tools, display name) instead of manual prompt/tool wiring.
pub fn create_agent(params: &AgentCreateParams, app_config: &AppConfig) -> ReactAgent {
    let model = params.model.as_deref().unwrap_or(&app_config.model.name);

    // Parse mode with bilingual support via LocalizedModeEngine
    let agent_mode = LocalizedModeEngine::from_str(&params.mode)
        .or_else(|| AgentMode::from_name(&params.mode))
        .unwrap_or_else(|| {
            let valid = ["general", "coding", "research", "data", "writing"];
            tracing::warn!(
                "Unknown mode '{}', falling back to 'general'. Valid modes: {}",
                params.mode,
                valid.join(", ")
            );
            AgentMode::General
        });

    let base_system_prompt = params
        .system_prompt
        .as_deref()
        .unwrap_or(&app_config.agent.system_prompt);

    // Load project context if available
    let project_ctx = if let Some(ref project_dir) = params.project {
        let project_root = std::path::Path::new(project_dir);
        if project_root.exists() {
            Some(crate::project::context::load_project_context(project_root))
        } else {
            tracing::warn!("项目目录不存在: {}", project_dir);
            None
        }
    } else if let Some(project_root) = crate::project::context::discover_project_root(None) {
        let ctx = crate::project::context::load_project_context(&project_root);
        if !ctx.instructions.is_empty() {
            Some(ctx)
        } else {
            None
        }
    } else {
        None
    };

    // Use PromptAssembler for modular, budget-aware prompt construction
    // (project context, mode-specific prompt, etc.)
    let model_window = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        8000 // default context window estimate
    };
    let assembler = PromptAssembler::default_for_mode(
        &agent_mode,
        base_system_prompt,
        project_ctx.as_ref(),
        model_window,
    );
    let system_prompt = assembler.assemble_no_vars();

    // Determine config values from AppConfig
    let token_limit = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        usize::MAX
    };

    let mode_engine = Arc::new(LocalizedModeEngine::with_chinese());

    // Use ReactAgentBuilder with mode and mode_engine for framework-level
    // auto-configuration (Chinese prompts, recommended tools, allowed_tools)
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(&app_config.agent.name)
        .system_prompt(&system_prompt)
        .mode(agent_mode)
        .mode_engine(mode_engine)
        .enable_tools()
        .enable_memory()
        .enable_human_in_loop()
        .enable_cot()
        .max_iterations(app_config.agent.max_iterations)
        .token_limit(token_limit)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: app_config.agent.tool_timeout_ms,
            ..Default::default()
        });

    // Initialize JSONL run store for trace persistence (before build)
    if let Ok(home) = std::env::var("HOME") {
        let run_dir = std::path::PathBuf::from(home)
            .join(".echo-agent")
            .join("runs");
        match JsonlRunStore::new(&run_dir) {
            Ok(store) => {
                builder = builder.with_run_store(Arc::new(store));
            }
            Err(e) => {
                tracing::warn!("Failed to initialize run store: {e}");
            }
        }
    }

    let mut agent = match builder.build() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to build agent: {e}");
            eprintln!("Error: Failed to initialize agent: {e}");
            eprintln!("Please check your configuration and try again.");
            std::process::exit(1);
        }
    };

    // Register default hooks
    register_default_hooks(&mut agent);

    agent
}

/// Register sensible default hooks for the CLI agent.
///
/// Register default hooks that should always be present.
///
/// Currently a placeholder — hooks are registered via hooks.yaml files
/// and the plugin system. This function can be extended to add
/// built-in hooks that should always be present.
///
/// The hook system uses YAML configuration files:
/// - `~/.echo-agent/hooks.yaml` (global hooks)
/// - `.echo-agent/hooks.yaml` (project-specific hooks)
///
/// Hooks can be defined for various events:
/// - SessionStart, SessionEnd
/// - PreToolUse, PostToolUse
/// - Stop, StopFailure
/// - And more (see echo_agent::skills::hooks::HookEvent)
fn register_default_hooks(agent: &mut ReactAgent) {
    tracing::info!(
        agent = %agent.model_name(),
        "Agent created, ready to register hooks from config/plugins"
    );
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
    let default_paths =
        [
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
pub fn inject_conversation_store(agent: &AgentHandle, store: &Option<Arc<dyn ConversationStore>>) {
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

/// Print a warning if the server binds to a non-localhost address.
///
/// Echo Agent CLI is designed as a single-user local application. Binding to
/// 0.0.0.0 or a public IP exposes the agent to the network.
pub fn warn_non_localhost_bind(host: &str, addr: &str, auth_enabled: bool) {
    // Check if host is a non-localhost address
    let is_localhost = host == "127.0.0.1" || host == "localhost" || host == "::1";
    if !is_localhost {
        tracing::warn!(
            "⚠️  Server binding to non-localhost address: http://{}",
            addr
        );
        tracing::warn!("   Echo Agent CLI is designed for single-user local use.");
        if !auth_enabled {
            tracing::warn!(
                "   Authentication is DISABLED — anyone on the network can access the agent."
            );
            tracing::warn!("   Enable JWT auth in config or set ECHO_AUTH_ENABLED=true.");
        }
        tracing::warn!(
            "   For remote access, use a reverse proxy with TLS and enable authentication."
        );
    }
}

/// Initialize TraceAnalyzer in AppState from the agent's RunStore.
///
/// Call this after the agent has been created (and its `run_store` set)
/// and before the AppState is wrapped in `Arc`. This extracts the
/// `Arc<dyn RunStore>` from the agent and creates a `TraceAnalyzer`
/// that routes can use for observability queries.
pub async fn init_trace_analyzer(state: &crate::state::AppState) {
    let run_store = state
        .connection
        .agent
        .read_async(|a| Box::pin(async move { a.run_store.clone() }))
        .await;

    if let Some(store) = run_store {
        let analyzer = echo_agent::trace::TraceAnalyzer::new(store);
        *state.trace.analyzer.write().await = Some(analyzer);
        tracing::info!("TraceAnalyzer initialized from agent RunStore");
    } else {
        tracing::warn!("No RunStore available on agent — TraceAnalyzer disabled");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    Stderr,
    TuiFile,
}

pub fn init_logging_for_tui(level: &str) {
    init_logging_with_target(level, LogTarget::TuiFile);
}

pub fn init_logging(level: &str) {
    init_logging_with_target(level, LogTarget::Stderr);
}

/// 初始化日志系统（线程安全，仅执行一次）
///
/// When the `telemetry` feature is enabled, this delegates to
/// [`echo_agent::telemetry::init_telemetry`] which sets up OTLP tracing + metrics
/// configured via `OTEL_EXPORTER_OTLP_ENDPOINT` (defaults to `http://localhost:4317`).
pub fn init_logging_with_target(level: &str, target: LogTarget) {
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
                enable_console: target == LogTarget::Stderr,
            };
            // Use env filter matching the requested level
            // Note: We don't set RUST_LOG env var to avoid thread-safety issues
            // Instead, we rely on tracing_subscriber's EnvFilter::new() to parse the filter
            let _ = echo_agent::telemetry::init_telemetry(config);
            return;
        }

        #[cfg(not(feature = "telemetry"))]
        {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            let default_filter =
                format!("echo_agent_cli={level},echo_agent={level},tower_http=info");
            let env_filter = || {
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::filter::EnvFilter::new(&default_filter))
            };

            match target {
                LogTarget::TuiFile => {
                    if let Ok(file) = std::fs::File::create(tui_log_path()) {
                        let _ = tracing_subscriber::registry()
                            .with(env_filter())
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .with_writer(std::sync::Mutex::new(file))
                                    .with_ansi(false),
                            )
                            .try_init();
                    }
                }
                LogTarget::Stderr => {
                    let _ = tracing_subscriber::registry()
                        .with(env_filter())
                        .with(tracing_subscriber::fmt::layer())
                        .try_init();
                }
            }
        }
    });
}

fn tui_log_path() -> std::path::PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut current = cwd.as_path();
        loop {
            let state_dir = crate::workspace::layout::WorkspaceLayout::state_dir(current);
            if state_dir.exists()
                || crate::workspace::layout::WorkspaceLayout::manifest(current).exists()
                || crate::workspace::layout::WorkspaceLayout::legacy_manifest(current).exists()
            {
                let dir = state_dir.join("logs");
                let _ = std::fs::create_dir_all(&dir);
                return dir.join("tui.log");
            }

            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(home)
        .join(".echo-agent")
        .join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tui.log")
}

// ── Doctor 诊断 ──────────────────────────────────────────────────

/// 诊断结果
pub struct DoctorResult {
    pub issues: Vec<String>,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorConnectivity {
    Skip,
    Probe,
}

fn provider_from_model(model: &str) -> &str {
    model
        .split_once(':')
        .map(|(provider, _)| provider)
        .unwrap_or_else(|| {
            let lower = model.to_ascii_lowercase();
            if lower.starts_with("gpt-")
                || lower.starts_with("o1")
                || lower.starts_with("o3")
                || lower.starts_with("o4")
            {
                "openai"
            } else if lower.starts_with("claude-") {
                "anthropic"
            } else if lower.starts_with("deepseek-") {
                "deepseek"
            } else if lower.starts_with("qwen-") || lower.starts_with("qwen3") {
                "qwen"
            } else if lower.starts_with("glm-") {
                "zhipu"
            } else if lower.starts_with("moonshot-") || lower.starts_with("kimi-") {
                "moonshot"
            } else {
                "qwen"
            }
        })
}

fn provider_required_keys(provider: &str) -> &'static [&'static str] {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "dashscope" | "qwen" | "aliyun" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "moonshot" | "kimi" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "zhipu" | "glm" => &["ZHIPU_API_KEY", "GLM_API_KEY"],
        "ollama" => &[],
        _ => &[],
    }
}

/// Send a minimal chat request to verify the model is reachable and responding.
async fn probe_model_connectivity(model: &str) -> echo_agent::error::Result<()> {
    use echo_agent::error::ReactError;
    use echo_agent::llm::core::types::Message;

    let config = echo_agent::llm::config::LlmConfig::from_model(model)?;

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ReactError::Other(format!("Failed to create HTTP client: {e}")))?;

    let messages = vec![Message::user("hi".to_string())];

    let response = echo_agent::llm::chat(
        std::sync::Arc::new(http_client),
        &config.model,
        &messages,
        Some(0.0),
        Some(1),
        Some(false),
        None,
        None,
        None,
    )
    .await?;

    if response.choices.is_empty() {
        return Err(ReactError::Other(
            "Model returned empty response".to_string(),
        ));
    }

    Ok(())
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor() -> DoctorResult {
    let mut config = crate::config::load_config(None);
    crate::config::apply_env_overrides(&mut config);
    run_base_doctor_for_model(&config.model.name)
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor_for_model(model: &str) -> DoctorResult {
    run_base_doctor_for_model_with_connectivity(model, DoctorConnectivity::Skip)
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor_for_model_with_connectivity(
    model: &str,
    connectivity: DoctorConnectivity,
) -> DoctorResult {
    let mut issues: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();

    let home = std::env::var("HOME").unwrap_or_default();

    let provider = provider_from_model(model);
    let required_keys = provider_required_keys(provider);
    if required_keys.is_empty() {
        checks.push(format!(
            "ℹ️  当前模型: {} (provider: {}, 无需或未知 API Key)",
            model, provider
        ));
    } else if required_keys.iter().any(|key| std::env::var(key).is_ok()) {
        checks.push(format!(
            "✅ 当前模型: {} (provider: {}, API Key: {})",
            model,
            provider,
            required_keys.join("/")
        ));
    } else {
        issues.push(format!(
            "❌ 当前模型 {} 需要设置 API Key: {}",
            model,
            required_keys.join(" 或 ")
        ));
    }

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
            checks.push(format!("✅ 已检测到 API Key: {} ({})", name, key));
            has_any_key = true;
        }
    }
    if !has_any_key {
        checks.push("ℹ️  未检测到其他 LLM API Key".to_string());
    }

    if connectivity == DoctorConnectivity::Probe {
        let probe_result = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(probe_model_connectivity(model)),
            Err(_) => Err(echo_agent::error::ReactError::Other(
                "Not running in a tokio runtime".to_string(),
            )),
        };
        match probe_result {
            Ok(()) => checks.push(format!("✅ 模型连通性: {} 可用", model)),
            Err(e) => issues.push(format!("❌ 模型连通性检查失败: {}", e)),
        }
    }

    let config_path = format!("{}/.echo-agent/config.yaml", home);
    if std::path::Path::new(&config_path).exists() {
        checks.push("✅ 配置文件: ~/.echo-agent/config.yaml".to_string());
    } else {
        issues.push("⚠️  未找到配置文件 ~/.echo-agent/config.yaml (使用默认配置)".to_string());
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
        issues.push(
            "⚠️  数据目录 ~/.echo-agent/ 不存在 (运行 echo-agent-cli onboard 初始化)".to_string(),
        );
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
            checks.push(format!(
                "✅ 项目指令: {} 个文件已加载",
                ctx.instructions.len()
            ));
        }
    } else {
        checks.push("ℹ️  未检测到项目目录 (可在项目根目录创建 AGENTS.md)".to_string());
    }

    DoctorResult { issues, checks }
}

/// Load user hooks from YAML config into the agent's hook registry.
pub async fn load_user_hooks(agent: &AgentHandle, app_config: &AppConfig) {
    let hooks_def = app_config.hooks.clone();
    if hooks_def.is_empty() {
        return;
    }
    let rule_count = hooks_def.rules.len();
    agent
        .write_async(|a| {
            Box::pin(async move {
                let mut registry = a.hook_registry().write().await;
                // Clear existing user hooks first to avoid duplicates on config reload
                registry.clear_user_hooks();
                registry.register_user_hooks(hooks_def);
            })
        })
        .await;
    tracing::info!(count = rule_count, "User hooks loaded from config");
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
