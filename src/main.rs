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

use echo_agent_cli::agent_handle::AgentHandle;
use echo_agent_cli::cli;
use echo_agent_cli::config;
use echo_agent_cli::infra;

use clap::Parser;

// ── 主入口 ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // 解析命令行参数
    let args = cli::Args::parse();

    // 加载 YAML 配置文件
    let mut app_config = config::load_config(args.config.as_deref());
    config::apply_env_overrides(&mut app_config);

    // 处理其他子命令 (不需要创建 Agent 即可执行的命令)
    if let Some(ref cmd) = args.command {
        return cli::handle_subcommand(cmd).await;
    }

    // 初始化日志（使用配置中的级别）
    infra::init_logging(&app_config.logging.level);

    // 创建 Agent + 加载 MCP 配置（统一路径，消除重复）
    let params = echo_agent_cli::infra::AgentCreateParams {
        model: args.model.clone(),
        mode: args.mode.clone(),
        system_prompt: args.system_prompt.clone(),
        project: args.project.clone(),
    };
    let mut agent = infra::create_agent(&params, &app_config);
    infra::load_mcp_config(&mut agent, args.mcp_config.as_deref(), &app_config).await;

    // Configure auto-compression if token_limit is set
    if app_config.has_compressor() {
        app_config.apply_compressor(&agent).await;
        tracing::info!(
            token_limit = app_config.agent.token_limit,
            strategy = %app_config.agent.compress_strategy,
            window = app_config.agent.compress_window,
            "Auto context compression configured"
        );
    }

    let agent_handle = AgentHandle::new(agent);

    // ── Wire HITL dispatcher to agent (all modes) ──
    let hitl_dispatcher = {
        use echo_agent_app_core::hitl::HitlDispatcher;
        use std::sync::Arc;
        let dispatcher = Arc::new(HitlDispatcher::new());
        // Register REPL provider for human-in-the-loop
        let repl_provider = Arc::new(echo_agent_app_core::hitl::ReplHumanLoopProvider::new());
        dispatcher.register("repl", repl_provider).await;
        agent_handle
            .write_async(|a| {
                let d = dispatcher.clone();
                Box::pin(async move {
                    a.set_human_loop_provider(d);
                })
            })
            .await;
        tracing::info!("HITL dispatcher wired to agent");
        dispatcher // Keep dispatcher reference for AppState
    };

    // Load user hooks from YAML config
    infra::load_user_hooks(&agent_handle, &app_config).await;

    // Also load hooks from hooks.yaml files (global + project-local)
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
        tracing::info!("Hooks loaded from hooks.yaml files");
    }

    // Create hook bridges so task/subagent lifecycle events fire in the central HookRegistry.
    // These must stay alive for the duration of the application.
    let task_hook_bridge = agent_handle
        .read(|a| a.create_task_hook_bridge())
        .await;
    let subagent_hook_bridge = agent_handle
        .read(|a| a.create_subagent_hook_bridge())
        .await;
    tracing::info!("Hook bridges created (task + subagent lifecycle events → HookRegistry)");

    // Initialize unified memory API — must stay alive for the application lifetime
    let unified_memory = {
        use echo_agent_app_core::unified_memory::UnifiedMemory;
        let mem = UnifiedMemory::load();
        tracing::info!(
            user = mem.get_instructions(echo_agent_app_core::unified_memory::InstructionTier::User).is_some(),
            project = mem.get_instructions(echo_agent_app_core::unified_memory::InstructionTier::Project).is_some(),
            local = mem.get_instructions(echo_agent_app_core::unified_memory::InstructionTier::Local).is_some(),
            "Unified memory loaded (instructions)"
        );
        mem
    };

    // Load plugins and wire components
    {
        use echo_agent::plugin::{PluginRegistry, PluginScope};
        let project_root = args.project.as_ref().map(|p| std::path::PathBuf::from(p));
        let mut plugin_registry = PluginRegistry::new(project_root);

        if let Err(e) = plugin_registry.scan_all() {
            tracing::warn!("Failed to scan plugins: {e}");
        } else {
            let plugin_count = plugin_registry.count();
            let enabled_count = plugin_registry.list_enabled().len();

            if plugin_count > 0 {
                tracing::info!("Discovered {plugin_count} plugins ({enabled_count} enabled)");

                // Resolve dependencies and load in order
                match plugin_registry.resolve_dependencies() {
                    Ok(ordered_ids) => {
                        let mut skills_to_load: Vec<std::path::PathBuf> = Vec::new();
                        let mut hooks_to_register: Vec<(String, String, echo_agent::skills::hooks::HooksDefinition)> = Vec::new();
                        let mut mcp_files_to_load: Vec<std::path::PathBuf> = Vec::new();

                        for plugin_id in &ordered_ids {
                            // First pass: collect entry info without mutable borrow
                            let entry_info = plugin_registry.get(plugin_id).map(|entry| {
                                (entry.enabled, entry.root.display().to_string())
                            });

                            let Some((enabled, source_dir)) = entry_info else {
                                continue;
                            };

                            if !enabled {
                                continue;
                            }

                            // Second pass: resolve components (needs &mut)
                            if let Ok(resolved) = plugin_registry.resolve_components(plugin_id) {
                                tracing::info!(
                                    plugin = %plugin_id,
                                    skills = resolved.skill_dirs.len(),
                                    agents = resolved.agent_files.len(),
                                    hooks = resolved.hooks_file.is_some(),
                                    mcp = resolved.mcp_config_file.is_some(),
                                    "Plugin components resolved"
                                );

                                // Collect skill directories for loading
                                for skill_dir in &resolved.skill_dirs {
                                    skills_to_load.push(skill_dir.clone());
                                }

                                // Collect hooks files for registration
                                if let Some(ref hooks_file) = resolved.hooks_file {
                                    if let Ok(content) = std::fs::read_to_string(hooks_file) {
                                        match serde_yaml_ng::from_str::<echo_agent::skills::hooks::HooksDefinition>(&content) {
                                            Ok(def) => {
                                                hooks_to_register.push((
                                                    plugin_id.clone(),
                                                    source_dir.clone(),
                                                    def,
                                                ));
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    plugin = %plugin_id,
                                                    path = %hooks_file.display(),
                                                    "Failed to parse plugin hooks YAML: {e}"
                                                );
                                            }
                                        }
                                    }
                                }

                                // Collect MCP config files for loading
                                if let Some(ref mcp_file) = resolved.mcp_config_file {
                                    mcp_files_to_load.push(mcp_file.clone());
                                }
                            }
                        }

                        // Wire skills into agent
                        if !skills_to_load.is_empty() {
                            let count = skills_to_load.len();
                            agent_handle
                                .write_async(|a| {
                                    Box::pin(async move {
                                        for dir in &skills_to_load {
                                            match a.load_skills_from_dir(dir).await {
                                                Ok(names) => {
                                                    tracing::info!(
                                                        dir = %dir.display(),
                                                        skills = names.len(),
                                                        "Plugin skills loaded"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        dir = %dir.display(),
                                                        "Failed to load plugin skills: {e}"
                                                    );
                                                }
                                            }
                                        }
                                    })
                                })
                                .await;
                            tracing::info!("Wired {count} skill directories from plugins");
                        }

                        // Wire hooks into agent
                        if !hooks_to_register.is_empty() {
                            let count = hooks_to_register.len();
                            agent_handle
                                .write_async(|a| {
                                    Box::pin(async move {
                                        let mut registry = a.hook_registry().write().await;
                                        for (plugin_name, source_dir, def) in &hooks_to_register {
                                            registry.register(
                                                &format!("plugin:{plugin_name}"),
                                                source_dir,
                                                def.clone(),
                                            );
                                            tracing::info!(
                                                plugin = %plugin_name,
                                                "Plugin hooks registered"
                                            );
                                        }
                                    })
                                })
                                .await;
                            tracing::info!("Wired {count} hook definitions from plugins");
                        }

                        // Wire MCP servers into agent
                        if !mcp_files_to_load.is_empty() {
                            let count = mcp_files_to_load.len();
                            agent_handle
                                .write_async(|a| {
                                    Box::pin(async move {
                                        for mcp_file in &mcp_files_to_load {
                                            match a.load_mcp_from_file(mcp_file).await {
                                                Ok(tools) => {
                                                    tracing::info!(
                                                        file = %mcp_file.display(),
                                                        tools = tools.len(),
                                                        "Plugin MCP servers connected"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        file = %mcp_file.display(),
                                                        "Failed to load plugin MCP config: {e}"
                                                    );
                                                }
                                            }
                                        }
                                    })
                                })
                                .await;
                            tracing::info!("Wired {count} MCP config files from plugins");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to resolve plugin dependencies: {e}");
                    }
                }
            }
        }
    }

    // ── LSP tool registration ──
    // Create an LspManager, load config from .lsp.yaml, and register LSP tools.
    {
        use echo_agent::lsp::{LspConfig, LspManager};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mut lsp_manager = LspManager::new();
        let mut lsp_configured = false;

        // Try loading project-level .lsp.yaml
        let project_lsp = std::env::current_dir()
            .ok()
            .and_then(|cwd| {
                let mut dir = cwd.as_path();
                loop {
                    let candidate = dir.join(".lsp.yaml");
                    if candidate.exists() {
                        return Some(candidate);
                    }
                    dir = dir.parent()?;
                }
            });

        if let Some(ref lsp_path) = project_lsp {
            match LspConfig::from_file(lsp_path) {
                Ok(config) => {
                    lsp_manager.load_config(&config);
                    lsp_configured = true;
                    tracing::info!(
                        path = %lsp_path.display(),
                        languages = config.servers.len(),
                        "LSP config loaded (project)"
                    );
                }
                Err(e) => {
                    tracing::warn!(path = %lsp_path.display(), "Failed to load LSP config: {e}");
                }
            }
        }

        // Try loading global ~/.echo-agent/.lsp.yaml
        let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
        if let Some(ref home_dir) = home {
            let global_lsp = home_dir.join(".echo-agent").join(".lsp.yaml");
            if global_lsp.exists() {
                match LspConfig::from_file(&global_lsp) {
                    Ok(config) => {
                        lsp_manager.load_config(&config);
                        lsp_configured = true;
                        tracing::info!(
                            path = %global_lsp.display(),
                            languages = config.servers.len(),
                            "LSP config loaded (global)"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(path = %global_lsp.display(), "Failed to load global LSP config: {e}");
                    }
                }
            }
        }

        // Set project root for LSP workspace
        if let Ok(cwd) = std::env::current_dir() {
            lsp_manager.set_project_root(&cwd);
        }

        if lsp_configured {
            let shared_lsp = Arc::new(RwLock::new(lsp_manager));
            agent_handle
                .write_async(|a| {
                    let shared_lsp = shared_lsp.clone();
                    Box::pin(async move {
                        use echo_agent::tools::lsp::{
                            LspDiagnosticsTool, LspGotoDefinitionTool, LspFindReferencesTool,
                            LspHoverTool, LspStatusTool,
                        };
                        a.add_tool(Box::new(LspDiagnosticsTool::new(shared_lsp.clone())));
                        a.add_tool(Box::new(LspGotoDefinitionTool::new(shared_lsp.clone())));
                        a.add_tool(Box::new(LspFindReferencesTool::new(shared_lsp.clone())));
                        a.add_tool(Box::new(LspHoverTool::new(shared_lsp.clone())));
                        a.add_tool(Box::new(LspStatusTool::new(shared_lsp)));
                    })
                })
                .await;
            tracing::info!("LSP tools registered (diagnostics, goto_definition, find_references, hover, status)");
        }
    }

    // Fire SessionStart("startup") hook — after hooks are loaded so they can react
    infra::fire_startup_hook(&agent_handle).await;

    // Spawn config file watcher (fires ConfigChange hooks + reloads hooks on change)
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if let Some(config_path) =
        echo_agent_cli::config_watcher::resolve_config_path(args.config.as_deref())
    {
        echo_agent_cli::config_watcher::spawn_config_watcher(
            config_path,
            agent_handle.clone(),
            cancel_token.clone(),
        );
    }

    // ── Background task store (all modes) ──
    // Create a SQLite store for background task persistence.
    // Passed to mode functions so they can start BackgroundTaskService.
    let task_store: std::sync::Arc<dyn echo_agent::memory::Store> = {
        let db_path = echo_agent_app_core::persistence::Persistence::base_dir()
            .join("tasks.db");
        match echo_agent::memory::SqliteStore::new(&db_path) {
            Ok(store) => std::sync::Arc::new(store),
            Err(e) => {
                tracing::warn!("Failed to create SQLite store for tasks: {e}");
                // Fallback: use FileStore
                let file_path = echo_agent_app_core::persistence::Persistence::base_dir()
                    .join("tasks_store");
                match echo_agent::memory::FileStore::new(&file_path) {
                    Ok(store) => std::sync::Arc::new(store),
                    Err(e2) => {
                        tracing::error!("Failed to create FileStore for tasks: {e2}");
                        std::sync::Arc::new(echo_agent::memory::InMemoryStore::new())
                    }
                }
            }
        }
    };

    
        let run_web = args.web || (!args.cli && !args.channels);
    let run_cli = args.cli;
    let run_channels = args.channels;

    if run_channels {
        #[cfg(feature = "channels")]
        {
            let channels_handle = tokio::spawn(cli::run_channels_mode(&app_config));

            if run_web && run_cli {
                cli::run_both_modes(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
            } else if run_cli {
                cli::run_cli_mode(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
            } else if run_web {
                cli::run_web_mode(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
            } else {
                // 仅 channels 模式，等待 channels 或 Ctrl+C
                channels_handle.await??;
                return Ok(());
            }
            // channels 会在后台运行，主模式退出后自动结束
        }
        #[cfg(not(feature = "channels"))]
        {
            tracing::error!(
                "--channels 需要启用 channels feature: cargo build --features channels"
            );
        }
    } else if run_web && run_cli {
        cli::run_both_modes(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
    } else if run_cli {
        // 仅 CLI 模式
        cli::run_cli_mode(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
    } else {
        // 仅 Web 模式
        cli::run_web_mode(agent_handle, hitl_dispatcher.clone(), &args, &app_config, task_store.clone()).await?;
    }

    // Keep hook bridges and unified memory alive until shutdown
    drop(task_hook_bridge);
    drop(subagent_hook_bridge);
    drop(unified_memory);

    Ok(())
}

// ── 单元测试 ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::prelude::*;
    use echo_agent_cli::config;

    #[test]
    fn test_create_agent_config() {
        let args = cli::Args {
            web: false,
            cli: false,
            port: 3000,
            host: "127.0.0.1".to_string(),
            model: Some("test-model".to_string()),
            system_prompt: Some("test prompt".to_string()),
            mode: "general".to_string(),
            project: None,
            mcp_config: None,
            config: None,
            no_color: false,
            channels: false,
            output: "text".to_string(),
            verbose: false,
            command: None,
        };

        let params = infra::AgentCreateParams {
            model: args.model.clone(),
            mode: args.mode.clone(),
            system_prompt: args.system_prompt.clone(),
            project: args.project.clone(),
        };
        let app_config = config::AppConfig::default();
        let agent = infra::create_agent(&params, &app_config);
        assert_eq!(agent.model_name(), "test-model");
    }

    #[test]
    fn test_args_default() {
        let args = cli::Args::parse_from(["echo-agent-cli"]);
        assert!(!args.web);
        assert!(!args.cli);
        assert_eq!(args.port, 3000);
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_args_cli_mode() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--cli"]);
        assert!(args.cli);
        assert!(!args.web);
    }

    #[test]
    fn test_args_both_modes() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--web", "--cli"]);
        assert!(args.web);
        assert!(args.cli);
    }

    #[test]
    fn test_args_custom_port() {
        let args = cli::Args::parse_from(["echo-agent-cli", "--port", "8080"]);
        assert_eq!(args.port, 8080);
    }
}
