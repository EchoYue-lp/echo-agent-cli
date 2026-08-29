/// Register sensible default hooks for the CLI agent.
///
/// Register default hooks that should always be present.
///
/// Currently a placeholder — hooks are registered via hooks.yaml files
/// and the plugin system. This function can be extended to add
/// built-in hooks that should always be present.
///
/// The hook system uses YAML configuration files:
/// - `~/.eko/hooks.yaml` (global hooks)
/// - `.eko/hooks.yaml` (project-specific hooks)
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

/// 启动 MCP 后台健康检查任务
pub fn spawn_mcp_health_check(
    state: Arc<crate::state::AppState>,
    cancel: echo_agent::agent::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // 首次检查延迟 5 秒，等待 MCP 连接初始化完成
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("MCP health check task stopped before first pass");
                return;
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
        }
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
    })
}

/// Spawn Dreaming after boot settles, then repeat it on a daily cadence.
///
/// Replaces the old "every-N-writes triggers a full review" model with a
/// recall-frequency-driven pass: promote high-recall memories (incl. Archived,
/// revived first) to the hot layer (MEMORY.md → system prompt stable prefix)
/// and batch-demote stale low-recall ones to Archived. Uses the shared
/// `ReviewIntegration`'s layer manager (same store the agent recalls from, so
/// revives/demotes land in the unified `["agent","memories"]` namespace).
/// Each completed pass settles the generation's shared hot-memory projection.
/// Best-effort errors are logged and the next pass still runs.
pub fn spawn_dreaming_task(
    review_integration: Arc<crate::evolution::ReviewIntegration>,
    cancel: echo_agent::agent::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Initial delay so boot-time activity isn't interrupted.
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("Dreaming task stopped before first pass");
                return;
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {}
        }
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Dreaming task stopped");
                    break;
                }
                _ = interval.tick() => {
                    let pass = run_dreaming_pass(&review_integration);
                    tokio::pin!(pass);
                    let result = tokio::select! {
                        _ = cancel.cancelled() => {
                            tracing::info!("Dreaming task stopped during active pass");
                            break;
                        }
                        result = &mut pass => result,
                    };
                    match result {
                        Ok(report) => {
                            tracing::info!(
                                scanned = report.scanned,
                                promoted = report.promoted,
                                revived = report.revived,
                                demoted = report.demoted,
                                "Dreaming pass completed"
                            );
                        }
                        Err(e) => tracing::warn!(error = %e, "Dreaming pass failed"),
                    }
                }
            }
        }
    })
}

async fn run_dreaming_pass(
    review_integration: &crate::evolution::ReviewIntegration,
) -> anyhow::Result<echo_agent::evolution::DreamingReport> {
    // The lease covers both framework writes and canonical projection
    // settlement. A workspace transition therefore either observes the complete
    // old generation or returns Busy before publishing the new generation.
    let generation_lease = review_integration
        .lease_generation()
        .map_err(anyhow::Error::from)?;
    let layer_manager = generation_lease.layer_manager()?;
    let dreaming = echo_agent::evolution::Dreaming::new(
        layer_manager,
        echo_agent::evolution::DreamingConfig::default(),
    );
    let report = dreaming.run().await.map_err(anyhow::Error::from)?;
    let projection = generation_lease.settle_hot_memory_projection().await;
    if let Some(error) = projection.error {
        tracing::warn!(%error, "Dreaming hot-memory projection remains pending");
    }
    Ok(report)
}

/// 创建对话持久化 Store（文件），失败时返回 None（禁用持久化）
pub fn create_conversation_store() -> Option<Arc<dyn ConversationStore>> {
    let base = crate::data_root::user_data_dir();

    match echo_agent::memory::FileConversationStore::new(&base) {
        Ok(store) => {
            tracing::info!(
                "ConversationStore (file) 初始化: {}/conversations",
                base.display()
            );
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

/// 创建运行时状态 Store（文件），失败时返回 None（禁用 checkpoint）
///
/// Persists `AgentCheckpoint`s (full messages + plan + active_skills + blocked_reason)
/// and the TaskNode DAG so a conversation can be resumed across process restarts.
/// Distinct from [`create_conversation_store`], which only stores user-visible
/// transcript projections.
pub fn create_runtime_state_store() -> Option<Arc<dyn RuntimeStateStore>> {
    create_runtime_state_store_in(crate::data_root::user_data_dir())
}

/// 创建指定 base dir 下的运行时状态 Store（U1c：文件后端，无 SQLite）。
pub fn create_runtime_state_store_in(
    base_dir: impl AsRef<std::path::Path>,
) -> Option<Arc<dyn RuntimeStateStore>> {
    match echo_agent::state::FileRuntimeStateStore::new(&base_dir) {
        Ok(store) => {
            tracing::info!(
                "RuntimeStateStore (file) 初始化: {}/runtime_state",
                base_dir.as_ref().display()
            );
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("RuntimeStateStore 初始化失败: {e}, 禁用运行时 checkpoint");
            None
        }
    }
}

/// 动态记忆 store 的全局默认路径：`~/.eko/store.json`。
///
/// 当无 workspace/project 时使用（CLI 在非项目目录启动、GUI 未进入 workspace）。
/// 与历史行为一致——框架默认就是这里。返回 (store_path, echo_agent_dir)：
/// `echo_agent_dir` 是 hot 层 MEMORY.md 的落点（`.eko/`），与 store 同根。
pub fn global_memory_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    let echo_agent_dir = crate::data_root::user_data_dir();
    let store_path = echo_agent_dir.join("store.json");
    (store_path, echo_agent_dir)
}

/// 解析当前应当使用的 memory store 路径与 echo_agent_dir。
///
/// 优先级（与 hot 层 MEMORY.md 的 discover 逻辑一致）：
/// 1. 给定 `workspace_root` → `{root}/.eko/memory/store.json`，echo_agent_dir = `{root}/.eko`
/// 2. 从 `cwd` 向上发现项目根（含 `.git`/`.eko`）→ `{root}/.eko/memory/store.json`
/// 3. 回退全局 `~/.eko/store.json`
///
/// `workspace_root` 用于已切换 workspace 的场景；CLI/TUI 启动时传 None 走 cwd discover。
pub fn resolve_memory_store_paths(
    workspace_root: Option<&std::path::Path>,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use crate::workspace::layout::WorkspaceLayout;

    // (1) 显式 workspace 根优先
    if let Some(root) = workspace_root
        && root.exists()
    {
        let store_path = WorkspaceLayout::memory_store(root);
        let echo_agent_dir = WorkspaceLayout::state_dir(root); // {root}/.eko
        return (store_path, echo_agent_dir);
    }

    // (2) 从 cwd 向上找项目根（与 discover_echo_agent_dir 同语义）
    if let Ok(cwd) = std::env::current_dir()
        && let Some(root) = crate::utils::find_project_root(&cwd)
    {
        let store_path = WorkspaceLayout::memory_store(&root);
        let echo_agent_dir = WorkspaceLayout::state_dir(&root); // {root}/.eko
        return (store_path, echo_agent_dir);
    }

    // (3) 全局兜底
    global_memory_paths()
}

/// 在指定路径创建 memory store（FileStore）。
///
/// 调用方负责保证 `store_path` 的父目录存在（`create_memory_store_for_workspace`
/// 会建目录；此函数只建文件）。失败时返回 None（框架随后会禁用记忆）。
pub fn create_memory_store_at(
    store_path: &std::path::Path,
) -> Option<Arc<dyn echo_agent::memory::Store>> {
    if let Some(parent) = store_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            path = %store_path.display(),
            error = %e,
            "Failed to create memory store dir; memory disabled"
        );
        return None;
    }
    match echo_agent::memory::FileStore::new(store_path) {
        Ok(store) => {
            tracing::info!(
                path = %store_path.display(),
                "Memory store (file) 初始化"
            );
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!(
                path = %store_path.display(),
                error = %e,
                "FileStore 初始化失败，禁用动态记忆"
            );
            None
        }
    }
}

/// 为 workspace/project 根创建 memory store（物理隔离）。
///
/// 落点：`{root}/.eko/memory/store.json`。workspace 切换时调用以重载 store。
pub fn create_memory_store_for_workspace(
    workspace_root: &std::path::Path,
) -> Option<Arc<dyn echo_agent::memory::Store>> {
    let store_path = crate::workspace::layout::WorkspaceLayout::memory_store(workspace_root);
    create_memory_store_at(&store_path)
}

/// 全局兜底 memory store（`~/.eko/store.json`）。
///
/// 用于无 workspace 时的 bootstrap，以及 exit_workspace 后的重置。
pub fn create_global_memory_store() -> Option<Arc<dyn echo_agent::memory::Store>> {
    let (store_path, _) = global_memory_paths();
    create_memory_store_at(&store_path)
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
