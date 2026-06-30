//! Agent runtime bootstrap — shared initialization for TUI and GUI entry points.
//!
//! This module consolidates the common agent initialization logic that was
//! previously duplicated in `main.rs` (TUI) and `desktop.rs` (GUI).
//!
//! # Usage
//!
//! ```rust,ignore
//! use echo_agent_app_core::runtime::AgentRuntime;
//!
//! let (agent_handle, hitl_dispatcher) =
//!     AgentRuntime::bootstrap(&app_config, params).await?;
//! ```

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::evolution::ReviewIntegration;
use crate::hitl::HitlDispatcher;
use crate::infra::{self, AgentCreateParams};
use crate::state::AppState;
use echo_agent::evolution::ReviewConfig;
use echo_agent::intent::{
    KeywordClassifier, LlmIntentClassifier, SkillDescription, TriggerSupervisor,
};

/// Shared agent runtime context.
///
/// Created once at application startup and lives for the entire process lifetime.
pub struct AgentRuntime {
    pub agent_handle: AgentHandle,
    pub hitl_dispatcher: Arc<HitlDispatcher>,
    pub unified_memory: crate::unified_memory::UnifiedMemory,
    pub app_config: AppConfig,
    pub keyword_classifier: KeywordClassifier,
    /// Hook bridge for forwarding task lifecycle events to the central HookRegistry.
    pub task_hook_bridge: Option<Arc<echo_agent::hooks_bridge::BridgedTaskHooks>>,
    /// Hook bridge for forwarding subagent lifecycle events to the central HookRegistry.
    pub subagent_hook_bridge: Option<echo_agent::hooks_bridge::SubagentHookBridge>,
    /// Shared `RuntimeStateStore` produced during bootstrap. Surfaced on the
    /// runtime so `init_pool` (and any future product paths) can inject the
    /// same instance into pooled agents — bypasses the previous `extract_from`
    /// path which only saw a `None` value because the primary agent never had
    /// a state store wired in.
    pub state_store: Option<Arc<dyn echo_agent::state::RuntimeStateStore>>,
    /// Memory review integration for staleness scoring, conflict detection,
    /// and garbage collection. Created in bootstrap when a `Store` is available.
    /// Used by `/memory-review` command and session-end review hooks.
    pub review_integration: Option<Arc<ReviewIntegration>>,
}

impl AgentRuntime {
    /// Bootstrap the agent runtime.
    ///
    /// This is the single source of truth for agent initialization. Both TUI and
    /// GUI entry points call this method instead of duplicating the setup logic.
    ///
    /// # Steps performed
    /// 1. Create `ReactAgent` via `infra::create_agent`
    /// 2. Load MCP configuration
    /// 3. Configure auto-compression
    /// 4. Wrap in `AgentHandle`
    /// 5. Wire HITL dispatcher
    /// 6. Load built-in skills
    /// 7. Load user hooks
    /// 8. Create hook bridges (task + subagent lifecycle)
    /// 9. Initialize unified memory
    /// 10. Load plugins (skills / hooks / MCP)
    /// 11. Register LSP tools
    /// 12. Fire startup hook
    pub async fn bootstrap(
        app_config: &AppConfig,
        mut params: AgentCreateParams,
    ) -> anyhow::Result<Self> {
        // ── 0a. Runtime state store (must be ready before agent is built so that
        //       conversation_id + state_store land on the AgentConfig together;
        //       otherwise `save_runtime_checkpoint` silently no-ops). ──
        let state_store = infra::create_runtime_state_store();
        if params.state_store.is_none() {
            params.state_store = state_store.clone();
        }

        // Default conversation_id for the *primary* agent. Both
        // `save_runtime_checkpoint` and `save_transcript_projection` early-return
        // when `conversation_id` is None. Use a fresh id per primary session to
        // avoid merging independent TUI/CLI runs into a shared "primary" row.
        if params.conversation_id.is_none() {
            params.conversation_id = Some(infra::default_primary_conversation_id());
        }

        // ── 0b. Unified memory — load instruction files (user.md / project.md /
        //       local.md) BEFORE building the agent so we can hand the assembled
        //       instruction suffix to `PromptAssembler::add_memory_context`.
        //       Dynamic long-term memories remain query-dependent and are recalled
        //       during each turn through the agent memory store. ──
        let unified_memory = crate::unified_memory::UnifiedMemory::load();
        let memory_suffix = unified_memory.system_prompt_context().to_prompt_suffix();
        if params.memory_context_suffix.is_none() && !memory_suffix.is_empty() {
            params.memory_context_suffix = Some(memory_suffix);
        }
        tracing::info!(
            has_memory_context = params.memory_context_suffix.is_some(),
            "Unified memory loaded"
        );

        // ── 1. Create Agent ──
        let mut agent = infra::create_agent(&params, app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // ── 2. Load MCP ──
        infra::load_mcp_config(&mut agent, None, app_config).await;

        // ── 3. Auto-compression ──
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::info!("Auto context compression configured");
        }

        let agent_handle = AgentHandle::new(agent);

        // ── 3a. Register delegate_readonly tool ──
        {
            use crate::tasks::task_runtime::delegate_readonly_tool::register_delegate_readonly_on_handle;
            register_delegate_readonly_on_handle(&agent_handle, params.task_runtime_store.clone())
                .await;
        }

        // ── NOTE: ExecutePlanTool + the task-management tools are NOT registered
        // here. The TaskRuntimeStore doesn't exist yet at primary-agent build
        // time (GUI: AppState creates it later; TUI: built in main.rs after
        // bootstrap), so BOTH entry points call `register_task_tools_on_agent`
        // (in app-core `tasks/task_runtime/register.rs`) post-hoc once the store
        // is ready. TUI/GUI functional parity (AGENTS.md).
        // ── 4. HITL dispatcher ──
        let hitl_dispatcher = {
            let dispatcher = Arc::new(HitlDispatcher::new());
            let repl_provider = Arc::new(crate::hitl::ReplHumanLoopProvider::new());
            dispatcher.register("repl", repl_provider).await;
            agent_handle
                .write_async(|a| {
                    let d = dispatcher.clone();
                    Box::pin(async move {
                        a.set_human_loop_provider(d);
                        a.build_permission_service();
                    })
                })
                .await;
            tracing::info!("HITL dispatcher + PermissionService wired to agent");
            dispatcher
        };

        // ── 5. Built-in skills ──
        {
            let builtin_skills_dir =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
            if builtin_skills_dir.is_dir() {
                agent_handle
                    .write_async(|a| {
                        Box::pin(async move {
                            match a.load_skills_from_dir(&builtin_skills_dir).await {
                                Ok(names) => {
                                    tracing::info!(count = names.len(), skills = ?names, "Built-in skills loaded");
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to load built-in skills: {e}");
                                }
                            }
                        })
                    })
                    .await;
            }
        }

        // ── 5b. Methodology baseline injection ──
        // Inject core methodology skill bodies (brainstorming / debugging /
        // verification / planning) directly into the system prompt so they
        // are always active without requiring explicit activate_skill calls.
        {
            let echo_home =
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                    .join(".echo-agent");
            let enabled_config_path = echo_home.join("enabled-skills.json");
            let enabled_config = crate::skills_hub::EnabledSkillsConfig::load(&enabled_config_path)
                .unwrap_or_default();
            // 收集 baseline 名为 owned Vec<String>,move 进 async 闭包(闭包要 'static,
            // 不能借用会在块结束 drop 的 enabled_config)。
            let baseline_names: Vec<String> = enabled_config
                .enabled_baseline_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            tracing::info!(
                count = baseline_names.len(),
                skills = ?baseline_names,
                "Methodology baseline skills loaded from enabled-skills.json"
            );
            if !baseline_names.is_empty() {
                agent_handle
                    .write_async(|a| {
                        Box::pin(async move {
                            // 用 public API 读当前有效 system_prompt(优先 runtime
                            // override,否则 config.system_prompt)。bootstrap 阶段
                            // mutable_system_prompt 还是 None,返回 config 值。
                            // 避免访问 pub(crate) 私有字段 system_prompt。
                            let mut sp = a.current_system_prompt();
                            // baseline_names 已 move 进闭包,此处借用安全。
                            let refs: Vec<&str> =
                                baseline_names.iter().map(|s| s.as_str()).collect();
                            a.skill_registry()
                                .inject_methodology_baseline(&mut sp, &refs);
                            a.set_system_prompt(sp).await;
                        })
                    })
                    .await;
                tracing::info!("Methodology baseline injected into system prompt");
            }
        }

        // ── 6. User hooks ──
        infra::load_user_hooks(&agent_handle, app_config).await;
        let hooks_load = crate::hooks_config::load_hooks_files();
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

        // ── 7. Hook bridges ──
        let task_hook_bridge = agent_handle.read(|a| a.create_task_hook_bridge()).await;
        let subagent_hook_bridge = agent_handle.read(|a| a.create_subagent_hook_bridge()).await;
        tracing::info!("Hook bridges created");

        // ── 8. Unified memory — already loaded in step 0b (instruction-only).
        //       Dynamic agent-learned memories are managed by the layered
        //       MemoryLayerManager, not UnifiedMemory. ──

        // ── 8b. Review integration — create when Store is available so
        //       /memory-review and session-end hooks can access it. ──
        let review_integration = agent_handle
            .read(|a| a.store().cloned())
            .await
            .map(|store| {
                let echo_agent_dir =
                    crate::evolution::review_integration::discover_echo_agent_dir();
                Arc::new(ReviewIntegration::new(
                    ReviewConfig::default(),
                    echo_agent_dir,
                    store,
                ))
            });
        if review_integration.is_some() {
            tracing::info!("ReviewIntegration created for session");
        }
        if let Some(review_integration) = &review_integration {
            let layer_manager = Arc::new(
                review_integration
                    .create_layer_manager()
                    .with_write_observer(review_integration.clone()),
            );
            agent_handle
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_layer_manager(layer_manager);
                    })
                })
                .await;
            tracing::info!("Layered memory tools installed on primary agent");
        }

        // ── 9. Plugins ──
        load_plugins(&agent_handle).await;

        // ── 10. LSP tools ──
        register_lsp_tools(&agent_handle).await;

        // ── 11. Startup hook ──
        infra::fire_startup_hook(&agent_handle).await;

        // ── 12. ChainedClassifier (Keyword → LLM) + IntentRouter ──
        let mut keyword_classifier = KeywordClassifier::new();
        let mut skill_descriptions = Vec::new();
        {
            let descriptors = agent_handle.read(|a| a.skill_descriptors()).await;
            for desc in &descriptors {
                let triggers: Vec<&str> = desc.triggers.iter().map(|s| s.as_str()).collect();
                keyword_classifier.add_skill_keywords(&desc.name, &triggers);
                // Build SkillDescription for LLM classifier
                skill_descriptions.push(SkillDescription {
                    name: desc.name.clone(),
                    description: desc.description.clone(),
                    example_triggers: desc.triggers.iter().take(3).cloned().collect(),
                });
            }
            tracing::info!(
                skill_count = descriptors.len(),
                "KeywordClassifier populated from skill descriptors"
            );
        }

        // ── 12. TriggerSupervisor (Keyword + LLM + Hook fusion) + IntentRouter ──
        // Build LLM classifier as fallback if LLM client is available,
        // otherwise TriggerSupervisor operates keyword-only.
        let hook_cache = agent_handle.read(|a| a.hook_activation_cache()).await;
        let supervisor = {
            let llm_classifier = agent_handle
                .read(|a| a.llm_client().cloned())
                .await
                .map(|llm| LlmIntentClassifier::new(llm, skill_descriptions));
            let has_llm = llm_classifier.is_some();
            let sv = TriggerSupervisor::new(keyword_classifier.clone(), llm_classifier, hook_cache);
            tracing::info!(
                has_llm = has_llm,
                "TriggerSupervisor: Keyword → {} → Hook slot (fusion)",
                if has_llm {
                    "LlmIntent"
                } else {
                    "Hook-only fallback"
                }
            );
            sv
        };

        {
            use echo_agent::intent::{IntentRouter, IntentRouterConfig};
            let router = IntentRouter::new(
                Box::new(supervisor),
                IntentRouterConfig {
                    confidence_threshold: 0.7,
                    enable_direct_answer: true,
                    enable_skill_routing: true,
                    enable_workflow_routing: false,
                },
            );
            agent_handle
                .write_async(|a| {
                    Box::pin(async move {
                        a.set_intent_router(router);
                    })
                })
                .await;
            tracing::info!("IntentRouter wired with TriggerSupervisor");
        }

        Ok(Self {
            agent_handle,
            hitl_dispatcher,
            unified_memory,
            app_config: app_config.clone(),
            keyword_classifier,
            task_hook_bridge: Some(Arc::new(task_hook_bridge)),
            subagent_hook_bridge: Some(subagent_hook_bridge),
            state_store,
            review_integration,
        })
    }

    /// Convenience: build `AppState` from the runtime context.
    ///
    /// GUI entry uses this to create the Tauri-managed application state.
    pub fn into_app_state(
        self,
        conversation_store: Option<Arc<dyn echo_agent::memory::ConversationStore>>,
    ) -> AppState {
        let state = AppState::from_shared(
            self.agent_handle.clone(),
            self.hitl_dispatcher.clone(),
            conversation_store,
            self.app_config.clone(),
        )
        .with_review_integration(self.review_integration.clone());
        // Note: task_service and scheduler are started separately by the caller
        // because they need a Store which may be created differently per entry.
        state
    }

    /// Initialize an `AgentPool` from this runtime for multi-conversation
    /// parallel execution.
    ///
    /// Extracts shared resources from the primary agent and creates a pool
    /// that can spin up isolated agent instances on demand.
    pub async fn init_pool(
        &self,
        config: crate::agent_pool::PoolConfig,
    ) -> Arc<crate::agent_pool::AgentPool> {
        let pool = crate::agent_pool::AgentPool::from_runtime(self, config).await;
        let pool = Arc::new(pool);
        pool.spawn_cleanup_monitor().await;
        pool
    }

    /// Trigger reflection on a completed task or session.
    ///
    /// Convenience wrapper around [`checkpoint_reflection`](Self::checkpoint_reflection)
    /// that can be called from slash commands (`/reflect`) or session exit hooks.
    pub async fn reflect_on_session(&self) {
        self.checkpoint_reflection("session", "Interactive session completed", "Session ended")
            .await;
    }

    /// Checkpoint Reflection: reflect on a completed skill execution and write
    /// learnings back to project memory.
    ///
    /// This is a lightweight, non-blocking reflection that runs after a skill
    /// successfully completes. It uses ~200 tokens to summarize key learnings.
    pub async fn checkpoint_reflection(&self, skill_name: &str, task_summary: &str, result: &str) {
        // Build a concise prompt for the LLM
        let prompt = format!(
            "Reflect on the following completed task and summarize key learnings \
             in 1-2 sentences.\n\n\
             Skill: {skill_name}\n\
             Task: {task_summary}\n\
             Result: {result}\n\n\
             Rules:\n\
             - Focus on reusable insights (data quirks, tool behavior, user preferences)\n\
             - Be specific\n\
             - Do not include sensitive information\n\
             - Max 200 tokens\n\n\
             Reflection:"
        );

        // Clone the LLM client Arc so we can use it outside the read lock
        let llm_client = self
            .agent_handle
            .read(|agent| agent.llm_client().cloned())
            .await;

        let Some(llm) = llm_client else {
            tracing::debug!("No LLM client available, skipping checkpoint reflection");
            return;
        };

        // Call LLM to generate reflection (lightweight, max 300 tokens, 2s timeout)
        let reflection = {
            let messages = vec![echo_agent::prelude::Message::user(prompt)];
            let options = echo_agent::prelude::SimpleChatOptions::default().with_max_tokens(300);
            let llm_call = llm.chat_simple_with_options(messages, options);

            // Enforce 2-second wall-clock timeout
            match tokio::time::timeout(std::time::Duration::from_secs(2), llm_call).await {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "LLM reflection failed, using fallback");
                    format!("Completed {skill_name}: {task_summary}")
                }
                Err(_) => {
                    tracing::warn!("LLM reflection timed out (>2s), using fallback");
                    format!("Completed {skill_name}: {task_summary}")
                }
            }
        };

        // Write to project memory
        let memory_dir = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".echo-agent")
            .join("memory");
        let _ = std::fs::create_dir_all(&memory_dir);
        let memory_file = memory_dir.join("PROJECT.md");

        let entry = format!("\n## [{skill_name}] {task_summary}\n{reflection}\n");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&memory_file)
        {
            Ok(mut file) => {
                use std::io::Write;
                if let Err(e) = file.write_all(entry.as_bytes()) {
                    tracing::warn!(path = %memory_file.display(), error = %e, "Failed to write checkpoint reflection");
                } else {
                    tracing::info!(path = %memory_file.display(), skill = skill_name, "Checkpoint reflection written");
                }
            }
            Err(e) => {
                tracing::warn!(path = %memory_file.display(), error = %e, "Failed to open project memory for writing");
            }
        }
    }
}

async fn load_plugins(agent_handle: &AgentHandle) {
    use echo_agent::plugin::PluginRegistry;

    let mut plugin_registry = PluginRegistry::new(None);

    if let Err(e) = plugin_registry.scan_all() {
        tracing::warn!("Failed to scan plugins: {e}");
        return;
    }

    let plugin_count = plugin_registry.count();
    let enabled_count = plugin_registry.list_enabled().len();

    if plugin_count == 0 {
        return;
    }

    tracing::info!("Discovered {plugin_count} plugins ({enabled_count} enabled)");

    match plugin_registry.resolve_dependencies() {
        Ok(ordered_ids) => {
            let mut skills_to_load: Vec<std::path::PathBuf> = Vec::new();
            let mut hooks_to_register: Vec<(
                String,
                String,
                echo_agent::skills::hooks::HooksDefinition,
            )> = Vec::new();
            let mut mcp_files_to_load: Vec<std::path::PathBuf> = Vec::new();

            for plugin_id in &ordered_ids {
                let entry_info = plugin_registry
                    .get(plugin_id)
                    .map(|entry| (entry.enabled, entry.root.display().to_string()));

                let Some((enabled, source_dir)) = entry_info else {
                    continue;
                };

                if !enabled {
                    continue;
                }

                if let Ok(resolved) = plugin_registry.resolve_components(plugin_id) {
                    for skill_dir in &resolved.skill_dirs {
                        skills_to_load.push(skill_dir.clone());
                    }

                    if let Some(ref hooks_file) = resolved.hooks_file
                        && let Ok(content) = std::fs::read_to_string(hooks_file)
                        && let Ok(def) = serde_yaml::from_str::<
                            echo_agent::skills::hooks::HooksDefinition,
                        >(&content)
                    {
                        hooks_to_register.push((plugin_id.clone(), source_dir.clone(), def));
                    }

                    if let Some(ref mcp_file) = resolved.mcp_config_file {
                        mcp_files_to_load.push(mcp_file.clone());
                    }
                }
            }

            if !skills_to_load.is_empty() {
                let count = skills_to_load.len();
                agent_handle
                    .write_async(|a| {
                        Box::pin(async move {
                            for dir in &skills_to_load {
                                let _ = a.load_skills_from_dir(dir).await;
                            }
                        })
                    })
                    .await;
                tracing::info!("Wired {count} skill directories from plugins");
            }

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
                            }
                        })
                    })
                    .await;
                tracing::info!("Wired {count} hook definitions from plugins");
            }

            if !mcp_files_to_load.is_empty() {
                let count = mcp_files_to_load.len();
                agent_handle
                    .write_async(|a| {
                        Box::pin(async move {
                            for mcp_file in &mcp_files_to_load {
                                let _ = a.load_mcp_from_file(mcp_file).await;
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

async fn register_lsp_tools(agent_handle: &AgentHandle) {
    use echo_agent::lsp::{LspConfig, LspManager};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let mut lsp_manager = LspManager::new();
    let mut lsp_configured = false;

    // Try loading project-level .lsp.yaml
    let project_lsp = std::env::current_dir().ok().and_then(|cwd| {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join(".lsp.yaml");
            if candidate.exists() {
                return Some(candidate);
            }
            dir = dir.parent()?;
        }
    });

    if let Some(ref lsp_path) = project_lsp
        && let Ok(config) = LspConfig::from_file(lsp_path)
    {
        lsp_manager.load_config(&config);
        lsp_configured = true;
        tracing::info!(path = %lsp_path.display(), languages = config.servers.len(), "LSP config loaded (project)");
    }

    // Try loading global ~/.echo-agent/.lsp.yaml
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
    if let Some(ref home_dir) = home {
        let global_lsp = home_dir.join(".echo-agent").join(".lsp.yaml");
        if global_lsp.exists()
            && let Ok(config) = LspConfig::from_file(&global_lsp)
        {
            lsp_manager.load_config(&config);
            lsp_configured = true;
            tracing::info!(path = %global_lsp.display(), languages = config.servers.len(), "LSP config loaded (global)");
        }
    }

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
                        LspDiagnosticsTool, LspFindReferencesTool, LspGotoDefinitionTool,
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
        tracing::info!("LSP tools registered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::intent::IntentClassifier;

    fn make_test_classifier() -> KeywordClassifier {
        let mut c = KeywordClassifier::new();
        c.add_skill_keywords("coding", &["写代码", "编程", "调试", "debug", "实现"]);
        c.add_skill_keywords("paper-search", &["论文检索", "arxiv", "文献检索", "找论文"]);
        c.add_skill_keywords(
            "evidence-medicine",
            &["医学文献", "pubmed", "临床试验", "循证"],
        );
        c
    }

    #[test]
    fn test_classifier_routes_coding_query() {
        let c = make_test_classifier();
        let intent = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(c.classify("帮我写代码实现排序", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "coding"),
            "Should route to coding, got {:?}",
            intent
        );
    }

    #[test]
    fn test_classifier_routes_research_query() {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let intent = rt.block_on(c.classify("帮我搜索 arxiv 上的论文", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "paper-search"),
            "arxiv should match paper-search, got {:?}",
            intent
        );
    }

    #[test]
    fn test_classifier_routes_medical_query() {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let intent = rt.block_on(c.classify("搜索 pubmed 上关于骨质疏松的文献", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "evidence-medicine"),
            "PubMed should route to evidence-medicine, got {:?}",
            intent
        );
    }

    #[test]
    fn test_classifier_no_match_returns_fallback() {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let intent = rt.block_on(c.classify("今天天气怎么样", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::Fallback),
            "Weather should be Fallback, got {:?}",
            intent
        );
    }

    #[test]
    fn test_classifier_empty_returns_fallback() {
        let c = KeywordClassifier::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let intent = rt.block_on(c.classify("帮我写代码", &[]));
        assert!(matches!(intent, echo_agent::intent::Intent::Fallback));
    }

    #[test]
    fn test_classifier_word_boundary_no_false_positive() {
        let mut c = KeywordClassifier::new();
        c.add_skill_keywords("coding", &["bug"]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let intent = rt.block_on(c.classify("I am debugging the code", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::Fallback),
            "'debugging' should not trigger 'bug', got {:?}",
            intent
        );
        let intent = rt.block_on(c.classify("there is a bug in my code", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { .. }),
            "Standalone 'bug' should trigger coding, got {:?}",
            intent
        );
    }
}
