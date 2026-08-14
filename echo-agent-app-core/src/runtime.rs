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
//! let mcp_config_path = echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(
//!     None,
//!     &app_config,
//! );
//! let runtime = AgentRuntime::bootstrap(&app_config, params, mcp_config_path).await?;
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::evolution::ReviewIntegration;
use crate::hitl::HitlDispatcher;
use crate::infra::{self, AgentCreateParams};
use crate::state::AppState;
use echo_agent::config::AppConfig;
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
    pub app_config: AppConfig,
    pub keyword_classifier: KeywordClassifier,
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
    /// Application-owned Playwright MCP runtime shared by every agent surface.
    pub browser_runtime: Arc<crate::browser::BrowserRuntime>,
    /// Static EKO prompt-module budget report captured at agent build time.
    pub prompt_assembly: crate::project::prompt::PromptAssembly,
    /// Process-level shared plugin runtime used by every interaction surface.
    pub plugin_runtime: Arc<crate::plugin_runtime::PluginRuntimeService>,
    /// Canonical durable user MCP configuration shared with application state.
    pub mcp_config_runtime: Arc<crate::mcp_config_runtime::McpConfigRuntime>,
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
        mcp_config_path: PathBuf,
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

        // ── 0b. Resolve and parse the canonical MCP source before starting
        // background resources. A malformed existing file aborts bootstrap so
        // it cannot later be overwritten by an empty in-memory snapshot.
        let mcp_config_snapshot = crate::mcp_config_runtime::load_mcp_config_snapshot(
            &mcp_config_path,
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "canonical MCP config {} cannot be loaded: {error}",
                mcp_config_path.display()
            )
        })?;
        let mcp_config_runtime = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
            mcp_config_path.clone(),
            mcp_config_snapshot.clone(),
        ));

        let browser_runtime = match params.browser_runtime.clone() {
            Some(runtime) => runtime,
            None => {
                crate::browser::BrowserRuntime::start(crate::browser::BrowserConfig::from_env())
                    .await
            }
        };
        params.browser_runtime = Some(browser_runtime.clone());

        // ── 1. Create Agent ──
        let created = infra::create_agent_with_diagnostics(&params, app_config)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut agent = created.agent;
        let prompt_assembly = created.prompt_assembly;

        // ── 2. Connect the same snapshot exposed to application state. ──
        tracing::info!(path = %mcp_config_path.display(), "Canonical MCP config selected");
        match agent.load_mcp_config(mcp_config_snapshot).await {
            Ok(clients) => tracing::info!(count = clients.len(), "MCP user servers connected"),
            Err(error) => tracing::warn!(%error, "MCP user config connection failed"),
        }

        // ── 3. Auto-compression ──
        if app_config.has_compressor() {
            app_config.apply_compressor(&agent).await;
            tracing::info!("Auto context compression configured");
        }

        let agent_handle = AgentHandle::new(agent);

        // ── NOTE: ExecuteTaskTool + the task-management tools are NOT registered
        // here. The TaskRuntimeStore doesn't exist yet at primary-agent build
        // time (GUI: AppState creates it later; TUI: built in main.rs after
        // bootstrap), so BOTH entry points call `register_task_tools_on_agent`
        // (in app-core `tasks/task_runtime/register.rs`) post-hoc once the store
        // is ready. TUI/GUI functional parity (AGENTS.md).
        // Chat 可用 agent_tool 做单个临时子任务;Auto/Task 的委派统一进入正式 TaskRuntime。
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
        browser_runtime
            .set_default_approval_provider(hitl_dispatcher.clone())
            .await;

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
            let enabled_config_path = echo_agent::paths::user_data_path("enabled-skills.json");
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
        // Single merged load: echo-agent.yaml inline + ~/.eko/hooks.yaml +
        // .eko/hooks.yaml are merged into one HooksDefinition by
        // HookConfigLoader (P0-1), then registered once. The previous code
        // loaded inline and file sources separately, each calling
        // clear_user_hooks(), so the second load wiped the first — a silent
        // bug where echo-agent.yaml inline hooks disappeared whenever any
        // hooks.yaml file existed.
        infra::load_user_hooks(&agent_handle, app_config).await;

        // ── 8b. Review integration — create when Store is available so
        //       /memory-review and session-end hooks can access it. ──
        // The `echo_agent_dir` MUST be the same root the memory store was
        // built from (see infra::create_agent), so hot-layer `MEMORY.md` and
        // warm-layer `store.json` land in the same project directory and never
        // diverge. We resolve it from `params.working_dir` (workspace root) —
        // identical to the store path resolution done in `create_agent`.
        let (_, review_echo_agent_dir) =
            infra::resolve_memory_store_paths(params.working_dir.as_deref());
        let review_integration = agent_handle
            .read(|a| a.store().cloned())
            .await
            .map(|store| {
                Arc::new(ReviewIntegration::new(
                    ReviewConfig::default(),
                    review_echo_agent_dir.clone(),
                    store,
                ))
            });
        if review_integration.is_some() {
            tracing::info!("ReviewIntegration created for session");
        }
        if let Some(review_integration) = &review_integration {
            let evolution_observer = crate::evolution::evolution_hook_observer(&agent_handle).await;
            review_integration.set_evolution_observer(evolution_observer);
            let layer_manager = Arc::new(review_integration.create_layer_manager());
            let trigger_sink = review_integration.clone();
            let skill_policy = review_integration.clone();
            let skill_curator = review_integration.curator();
            let workspace_skills = review_echo_agent_dir.join("skills");
            agent_handle
                .write_async(|a| {
                    Box::pin(async move {
                        a.install_memory_layer_manager(layer_manager);
                        a.set_memory_trigger_sink(Some(trigger_sink));
                        a.set_skill_load_policy(Some(skill_policy));
                        a.set_skill_curator(Some(skill_curator));
                        if workspace_skills.is_dir()
                            && let Err(error) = a.load_skills_from_dir(workspace_skills).await
                        {
                            tracing::warn!(%error, "Failed to load workspace-curated skills");
                        }
                    })
                })
                .await;
            tracing::info!("Layered memory, evidence sink, and skill policy installed");
        }

        // ── 9. LSP runtime ──
        // Plugins and built-in project discovery share this single manager;
        // plugin reload atomically replaces its contents while every LSP tool
        // keeps the same Arc handle.
        let lsp_runtime = register_lsp_tools(&agent_handle).await;

        // ── 10. Plugins ──
        // Discovery, initial wiring, and later live mutations all go through
        // one runtime owner. This avoids bootstrap/reload double registration.
        let plugin_runtime = crate::plugin_runtime::PluginRuntimeService::new(
            agent_handle.clone(),
            lsp_runtime,
            mcp_config_runtime.ownership(),
        )
        .await;

        // ── 11. File-backed research library ──
        agent_handle
            .write(|agent| {
                crate::research_connectors::install_auto_ingest_tools(agent);
                agent.add_tool(Box::new(crate::research_tool::ResearchLibraryTool));
            })
            .await;

        // ── 12. Startup hook ──
        infra::fire_startup_hook(&agent_handle).await;

        // ── 13. ChainedClassifier (Keyword → LLM) + IntentRouter ──
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
        let available_skill_names = skill_descriptions
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<Vec<_>>();

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
                    classification_timeout_ms: 5_000,
                },
            )
            .with_available_skills(available_skill_names);
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
            app_config: app_config.clone(),
            keyword_classifier,
            state_store,
            review_integration,
            browser_runtime,
            prompt_assembly,
            plugin_runtime,
            mcp_config_runtime,
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
            self.mcp_config_runtime.clone(),
        )
        .with_review_integration(self.review_integration.clone())
        .with_prompt_assembly(self.prompt_assembly.clone())
        .with_plugin_runtime(Some(self.plugin_runtime.clone()));
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
        task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> Arc<crate::agent_pool::AgentPool> {
        let pool =
            crate::agent_pool::AgentPool::from_runtime(self, config, task_runtime_store).await;
        Arc::new(pool)
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
        let Some(review_integration) = self.review_integration.as_ref() else {
            tracing::warn!("Review integration unavailable; skipping checkpoint reflection");
            return;
        };
        let memory_generation = match review_integration.lease_generation() {
            Ok(generation) => generation,
            Err(error) => {
                tracing::warn!(%error, "Checkpoint reflection unavailable during workspace transition");
                return;
            }
        };

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

        // Write to the workspace root pinned before the LLM call.
        let memory_dir = memory_generation.echo_agent_dir().join("memory");
        if let Err(error) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(path = %memory_dir.display(), %error, "Failed to create project memory directory");
            return;
        }
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

async fn register_lsp_tools(agent_handle: &AgentHandle) -> crate::plugin_runtime::PluginLspRuntime {
    use echo_agent::lsp::{LspConfig, LspManager};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let project_root = agent_handle
        .read(|agent| agent.working_dir())
        .await
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut config = LspConfig::discover(&project_root);
    if !config.servers.is_empty() {
        tracing::info!(
            root = %project_root.display(),
            languages = config.servers.len(),
            "LSP servers auto-discovered"
        );
    }

    // Global preferences override discovery defaults.
    {
        let global_lsp = echo_agent::paths::user_data_path(".lsp.yaml");
        if global_lsp.is_file() {
            match LspConfig::from_file(&global_lsp) {
                Ok(global) => config.merge(global),
                Err(error) => {
                    tracing::warn!(path = %global_lsp.display(), %error, "Failed to load global LSP config")
                }
            }
        }
    }

    // The nearest project config has final precedence.
    let project_lsp = {
        let mut dir = project_root.as_path();
        loop {
            let candidate = dir.join(".lsp.yaml");
            if candidate.is_file() {
                break Some(candidate);
            }
            let Some(parent) = dir.parent() else {
                break None;
            };
            dir = parent;
        }
    };

    if let Some(ref lsp_path) = project_lsp {
        match LspConfig::from_file(lsp_path) {
            Ok(project) => {
                let language_count = project.servers.len();
                config.merge(project);
                tracing::info!(path = %lsp_path.display(), languages = language_count, "LSP config loaded (project)");
            }
            Err(error) => {
                tracing::warn!(path = %lsp_path.display(), %error, "Failed to load project LSP config");
            }
        }
    }

    let mut lsp_manager = LspManager::new();
    lsp_manager.load_config(&config);
    lsp_manager.set_project_root(&project_root);
    let languages: Vec<String> = lsp_manager
        .configured_languages()
        .into_iter()
        .map(str::to_string)
        .collect();
    for language in languages {
        if let Err(error) = lsp_manager.start_server(&language).await {
            tracing::warn!(%language, %error, "LSP server unavailable");
        }
    }

    let shared_lsp = Arc::new(RwLock::new(lsp_manager));
    agent_handle
        .write_async(|a| {
            let shared_lsp = shared_lsp.clone();
            Box::pin(async move {
                use echo_agent::tools::lsp::{
                    LspDiagnosticsTool, LspFindReferencesTool, LspGotoDefinitionTool, LspHoverTool,
                    LspStatusTool,
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
    crate::plugin_runtime::PluginLspRuntime::new(shared_lsp, config, project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::intent::IntentClassifier;
    use echo_agent::skills::external::{SkillLoader, tool_matcher};

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
    fn test_classifier_routes_coding_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let intent =
            tokio::runtime::Runtime::new()?.block_on(c.classify("帮我写代码实现排序", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "coding"),
            "Should route to coding, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_routes_research_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("帮我搜索 arxiv 上的论文", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "paper-search"),
            "arxiv should match paper-search, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_routes_medical_query() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("搜索 pubmed 上关于骨质疏松的文献", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::SkillRequired { ref skill_name, .. } if skill_name == "evidence-medicine"),
            "PubMed should route to evidence-medicine, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_no_match_returns_fallback() -> anyhow::Result<()> {
        let c = make_test_classifier();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("今天天气怎么样", &[]));
        assert!(
            matches!(intent, echo_agent::intent::Intent::Fallback),
            "Weather should be Fallback, got {:?}",
            intent
        );
        Ok(())
    }

    #[test]
    fn test_classifier_empty_returns_fallback() -> anyhow::Result<()> {
        let c = KeywordClassifier::new();
        let rt = tokio::runtime::Runtime::new()?;
        let intent = rt.block_on(c.classify("帮我写代码", &[]));
        assert!(matches!(intent, echo_agent::intent::Intent::Fallback));
        Ok(())
    }

    #[test]
    fn test_classifier_word_boundary_no_false_positive() -> anyhow::Result<()> {
        let mut c = KeywordClassifier::new();
        c.add_skill_keywords("coding", &["bug"]);
        let rt = tokio::runtime::Runtime::new()?;
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
        Ok(())
    }

    #[tokio::test]
    async fn bundled_skill_allowlists_match_registered_tool_names() -> anyhow::Result<()> {
        let agent = echo_agent::agent::ReactAgent::new(echo_agent::agent::AgentConfig::standard(
            "test-model",
            "skill-audit",
            "test",
        ));
        let mut tool_names = agent.tool_names();
        tool_names.extend(
            ["task_create", "task_update", "task_list", "task_execute"]
                .into_iter()
                .map(str::to_string),
        );

        let skill_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills");
        let mut loader = SkillLoader::new();
        let descriptors = loader.discover_from_dir(skill_root).await?;
        assert!(
            !descriptors.is_empty(),
            "bundled skills were not discovered"
        );

        for descriptor in descriptors {
            for matcher in descriptor.allowed_tools {
                assert!(
                    tool_names
                        .iter()
                        .any(|tool_name| tool_matcher(&matcher, tool_name)),
                    "Skill '{}' allowed-tools entry '{}' matches no registered tool",
                    descriptor.name,
                    matcher
                );
            }
        }
        Ok(())
    }
}
