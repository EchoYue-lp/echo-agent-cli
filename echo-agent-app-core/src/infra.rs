//! 基础设施函数
//!
//! 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::agent::subagent::{
    AgentFactory, FnAgentFactory, SubagentBuilder, SubagentPromptCompiler,
    SubagentSystemPromptInput,
};
use echo_agent::llm::{LlmApiProtocol, LlmClient, LlmConfig};
use echo_agent::memory::ConversationStore;
use echo_agent::prelude::*;
use echo_agent::state::RuntimeStateStore;
use futures::future::BoxFuture;

use crate::agent_handle::AgentHandle;
use crate::model_config;
use crate::project::prompt::PromptAssembler;
use echo_agent::config::AppConfig;

type Result<T, E = echo_agent::error::ReactError> = std::result::Result<T, E>;

/// Default context window size in tokens (396K).
const DEFAULT_CONTEXT_WINDOW: usize = 396_000;
pub(crate) const EKO_MODEL_CRITIC_OWNER: &str = "eko:model-generation";

/// Default max output tokens when not configured (sensible for 128K context models).
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// EKO product default for one tool result returned to the model. The generic
/// framework keeps 0 as opt-out so other consumers choose their own budget.
const DEFAULT_MAX_TOOL_OUTPUT_TOKENS: usize =
    crate::tool_exposure::MAX_MODEL_VISIBLE_TOOL_RESULT_TOKENS;
const TOOL_OUTPUT_ARTIFACT_THRESHOLD_BYTES: usize = 32 * 1024;
const TOOL_OUTPUT_ARTIFACT_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;
fn resolved_max_tool_output_tokens(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS
    }
}

/// Resolve the one context budget used by prompt assembly, the parent agent,
/// and every subagent that inherits the selected runtime model.
///
/// Product priority is explicit agent override, selected model context window,
/// then EKO's documented fallback.
pub fn effective_token_limit(
    app_config: &AppConfig,
    runtime: &model_config::ModelRuntimeConfig,
) -> usize {
    if app_config.agent.token_limit > 0 {
        return app_config.agent.token_limit;
    }
    runtime
        .context_window
        .and_then(|window| usize::try_from(window).ok())
        .filter(|window| *window > 0)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

#[derive(Clone)]
struct SubagentRuntimeGeneration {
    model: String,
    llm_config: Option<LlmConfig>,
    llm_client: Option<Arc<dyn LlmClient>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    /// Reasoning-depth knob carried with the generation. Inherited bindings
    /// track the parent's published thinking; Fixed bindings resolve the
    /// role's `thinking` frontmatter once at registration.
    thinking: Option<echo_agent::llm::ThinkingConfig>,
}

#[derive(Clone)]
enum SubagentModelBinding {
    Inherit(Arc<tokio::sync::RwLock<SubagentRuntimeGeneration>>),
    Fixed(SubagentRuntimeGeneration),
}

impl SubagentModelBinding {
    async fn snapshot(&self) -> SubagentRuntimeGeneration {
        match self {
            Self::Inherit(generation) => generation.read().await.clone(),
            Self::Fixed(generation) => generation.clone(),
        }
    }
}

/// Resolve the thinking config for one subagent build.
///
/// An explicit role `thinking` frontmatter spec wins (`parse_spec` syntax;
/// "auto"/empty → model default); without one the generation's inherited
/// thinking applies (parent's current thinking for Inherit bindings, the
/// registration-time copy for Fixed bindings). Unrecognized specs warn loudly
/// and fall back to the generation value rather than failing registration.
fn subagent_build_thinking(
    spec: Option<&str>,
    generation: &SubagentRuntimeGeneration,
) -> Option<echo_agent::llm::ThinkingConfig> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        Some(spec) => match echo_agent::llm::ThinkingConfig::parse_spec(spec) {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(
                    spec = %spec,
                    %error,
                    "Invalid subagent thinking spec — falling back to the generation thinking"
                );
                generation.thinking.clone()
            }
        },
        None => generation.thinking.clone(),
    }
}

fn subagent_model_binding(
    spec: Option<&str>,
    thinking_spec: Option<&str>,
    parent_generation: &SubagentRuntimeGeneration,
    inherited_generation: &Arc<tokio::sync::RwLock<SubagentRuntimeGeneration>>,
) -> SubagentModelBinding {
    let model = resolve_subagent_model(spec, &parent_generation.model);
    if spec.is_none() {
        // Inherit: no thinking of its own — snapshots follow the shared
        // generation the parent republishes on every model hot-swap.
        SubagentModelBinding::Inherit(inherited_generation.clone())
    } else {
        // Fixed: resolve the role's thinking spec once; without one keep the
        // parent's thinking as of registration (mirrors temperature/max_tokens).
        SubagentModelBinding::Fixed(SubagentRuntimeGeneration {
            model: model.clone(),
            llm_config: parent_generation.llm_config.clone().map(|mut config| {
                config.model = model;
                config
            }),
            llm_client: None,
            temperature: parent_generation.temperature,
            max_tokens: parent_generation.max_tokens,
            token_limit: parent_generation.token_limit,
            thinking: subagent_build_thinking(thinking_spec, parent_generation),
        })
    }
}

#[derive(Clone)]
struct InheritedSubagentFactory {
    definition: echo_agent::agent::subagent::SubagentDefinition,
    handle: AgentHandle,
    factory: Arc<dyn AgentFactory>,
}

/// EKO-owned model consumers attached to one parent agent generation.
///
/// Only subagent definitions with omitted `model` frontmatter are included.
/// Explicit frontmatter values are resolved once and remain fixed.
#[derive(Clone)]
pub struct AgentModelConsumers {
    inherited_generation: Arc<tokio::sync::RwLock<SubagentRuntimeGeneration>>,
    registry: Arc<echo_agent::agent::subagent::SubagentRegistry>,
    inherited_factories: Arc<Vec<InheritedSubagentFactory>>,
}

impl AgentModelConsumers {
    async fn publish_inherited_generation(
        &self,
        runtime: &model_config::ModelRuntimeConfig,
        prepared: &PreparedRuntimeLlm,
        token_limit: usize,
    ) {
        *self.inherited_generation.write().await = SubagentRuntimeGeneration {
            model: runtime.model.clone(),
            llm_config: Some(prepared.config.clone()),
            llm_client: Some(prepared.client.clone()),
            temperature: runtime.temperature,
            max_tokens: runtime.max_tokens,
            token_limit,
            thinking: prepared.thinking.clone(),
        };
        for inherited in self.inherited_factories.iter() {
            self.registry
                .register_factory(inherited.definition.clone(), inherited.factory.clone())
                .await;
        }
    }

    #[cfg(test)]
    pub(crate) fn inherited_handle_for_test(&self, name: &str) -> Option<AgentHandle> {
        self.inherited_factories
            .iter()
            .find(|inherited| inherited.definition.name == name)
            .map(|inherited| inherited.handle.clone())
    }
}

/// Registration-time static environment for Subagent system prompts.
///
/// OS/arch are stable for the Agent's lifetime. Date and working directory are
/// dispatch-time facts and are rendered by the invocation compiler.
fn static_subagent_environment() -> String {
    format!(
        "- OS: {} ({})\n- Runtime: local personal assistant on the user's machine",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Product-owned storage policy for complete oversized tool output.
///
/// Workspace conversations keep their logs beside the rest of the workspace
/// state so file tools can recover the complete output within their normal
/// readable scope. Global conversations fall back to EKO's user-data root.
/// Conversation deletion removes its scope, while the 30-day max age prevents
/// abandoned scopes from growing without bound.
pub fn tool_output_artifact_config(
    working_dir: Option<&std::path::Path>,
) -> echo_agent::tools::artifact::ToolOutputArtifactConfig {
    // Use the common artifact root. Tool outputs live under the framework's
    // conversation/run layout while application-owned user input lives under
    // `user-input/`; `read_artifact` and `grep` can therefore recover both
    // without teaching the generic framework an EKO-specific sibling path.
    let root_dir = working_dir
        .map(crate::workspace::layout::WorkspaceLayout::artifacts)
        .unwrap_or_else(|| echo_agent::paths::user_data_path("artifacts"));
    echo_agent::tools::artifact::ToolOutputArtifactConfig::new(root_dir, "conversation_or_30d")
        .threshold_bytes(TOOL_OUTPUT_ARTIFACT_THRESHOLD_BYTES)
        .max_age_secs(Some(TOOL_OUTPUT_ARTIFACT_MAX_AGE_SECS))
}

/// Guide appended to the system prompt when task management tools are
/// available. Instructs the agent to actively manage its task graph and
/// proactively dispatch readonly subagents for investigation-heavy work
/// (对齐 Claude Code 的 subagent:轻量派发是工具,正式并行是 runtime).
pub(crate) const TASK_MANAGEMENT_GUIDE: &str = r#"

## Task And Delegation Tools

Choose the lightest reliable mechanism:
- Direct work: simple questions, narrow edits, short tool sequences.
- `agent_tool`: one bounded Chat subtask with no TaskRuntime entry.
- `task_create` + `task_list` + `task_execute({revision: N})`: one visible task or a dependency DAG. Use it for delegation, parallel work, writers, or verification.
- `create_complex_task`: a long-lived Run for cross-turn or substantial orchestration.

### Task Graph Contract
- Use the user's language for task titles and Subagent briefs; preserve technical identifiers. Give each task a concrete outcome, role, targets, dependencies, and verification.
- `execution_checks` require an observed command pass; `acceptance_criteria` are semantic reviewer judgments. Never declare acceptance passed yourself.
- A completed Subagent is not a completed Task. Acceptance failure blocks for an explicit retry.
- TaskRun already represents the goal. Do not create a wrapper or prose-only summary task; materialize only executable work.
- `task_create` always accepts one atomic `tasks` array, including for a single task. Give every task a stable ID, declare dependencies when they exist, and pass the returned revision to `task_execute`.
- Read-only tasks may run in parallel. Writers must declare owned files or artifacts.
- Keep the graph truthful with `task_update` and `task_list`. Existing graphs require the latest `base_revision`; only the runtime marks completion.
- Do not claim dispatch before `task_execute` accepts the committed revision.
- Background `agent_tool` finishes through events. Never poll its `execution_id` with task-status tools.
- After execution, synthesize evidence and answer the original goal.

### Complex Run Contract
Use `create_complex_task` only for expensive multi-step work, architectural/multi-file implementation, cross-turn state, or multi-source synthesis. Explain why in `reason`.
- Prefer background unless the current reply needs a prompt result.
- Do not use it for ordinary Q&A, a narrow edit, or one lookup.
- Use `check_run_status` only when requested; use `cancel_run` when no longer needed. Do not busy-poll.
"#;

/// Agent creation parameters (extracted from CLI args or config).
#[derive(Default)]
pub struct AgentCreateParams {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub project: Option<String>,
    /// Optional session ID for checkpoint isolation (used by background tasks).
    pub session_id: Option<String>,
    /// Optional conversation ID — required for `RuntimeStateStore` checkpointing
    /// and `ConversationStore` transcript projection. The pool sets this to the
    /// pooled agent's conversation key so per-conversation state can survive
    /// process restarts.
    pub conversation_id: Option<String>,
    /// React loop checkpoint interval in iterations (0 = only at end).
    /// Used by background tasks to enable crash recovery.
    pub react_checkpoint_interval: Option<usize>,
    /// Shared runtime state store. When supplied, the agent
    /// will save `AgentCheckpoint`s + TaskNode DAG entries every iteration.
    pub state_store: Option<Arc<dyn RuntimeStateStore>>,
    /// Optional caller-supplied stable context to inject into the root system
    /// prompt. EKO's workspace instructions and hot memory use replaceable
    /// projections instead so they can refresh without rebuilding the agent.
    pub memory_context_suffix: Option<String>,
    /// Session-bound working directory (worktree path). Propagated to
    /// `ReactAgent.config.working_dir`, which `ExecuteStage` injects into every
    /// tool call's `ToolContext` — so shell/file/git tools run inside the
    /// isolated checkout. None = use process cwd (backward compatible).
    pub working_dir: Option<std::path::PathBuf>,
    /// TaskRuntime store handle. When supplied, `create_agent` registers the
    /// task-management tools (task_create/task_update/task_list) so the
    /// main agent can autonomously manage its plan during execution.
    pub task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    /// Shared application-owned managed browser runtime. The same instance is
    /// installed on the primary agent and all built-in subagents so one
    /// Playwright MCP sidecar owns the managed browser profile.
    pub browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
}

/// Generate a fresh conversation id for the primary (non-pooled) agent.
///
/// Pooled GUI conversations use their frontend-provided conversation key. The
/// primary TUI/CLI agent has no such key, so give each process/session its own
/// id instead of writing every checkpoint to a shared "primary" row.
pub fn default_primary_conversation_id() -> String {
    format!("primary-{}", uuid::Uuid::new_v4())
}

/// Load or create the stable machine-scoped cache user id used by provider KV caches.
///
/// This id is shared by the primary agent and built-in subagents so repeated
/// project prompts land in the same provider cache partition across sessions.
pub fn load_or_create_cache_user_id() -> String {
    let path = echo_agent::paths::user_data_path("cache_user_id");

    if let Ok(existing) = std::fs::read_to_string(&path)
        && !existing.trim().is_empty()
    {
        return existing.trim().to_string();
    }

    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &id);
    tracing::info!(%id, "created new cache_user_id");
    id
}

/// Agent plus EKO-owned prompt assembly diagnostics.
pub struct CreatedAgent {
    pub agent: ReactAgent,
    pub prompt_assembly: crate::project::prompt::PromptAssembly,
    pub model_consumers: AgentModelConsumers,
    pub runtime_model: model_config::ModelRuntimeConfig,
}

/// Create an Agent instance without retaining build diagnostics.
pub async fn create_agent(
    params: &AgentCreateParams,
    app_config: &AppConfig,
) -> std::result::Result<ReactAgent, String> {
    create_agent_with_diagnostics(params, app_config)
        .await
        .map(|created| created.agent)
}

/// Create an agent and retain the application-owned prompt assembly report.
pub async fn create_agent_with_diagnostics(
    params: &AgentCreateParams,
    app_config: &AppConfig,
) -> std::result::Result<CreatedAgent, String> {
    if let Some(store) = &params.task_runtime_store {
        crate::tasks::task_runtime::command_cells::register_task_runtime_store(store);
    }
    // Resolve the product-level configured model first. The legacy `model`
    // section is only a persisted mirror/fallback; GUI/CLI/TUI should all
    // converge on configured_models for actual runtime wiring.
    let runtime_model =
        model_config::resolve_runtime_model_selector(app_config, params.model.as_deref())
            .map_err(|error| error.to_string())?;
    model_config::validate_runtime_model_requirements(&runtime_model)?;
    let model = runtime_model.model.as_str();
    let temperature = runtime_model.temperature.or(app_config.model.temperature);
    let max_tokens = runtime_model.max_tokens.or(app_config.model.max_tokens);

    let base_system_prompt = params
        .system_prompt
        .as_deref()
        .unwrap_or(&app_config.agent.system_prompt);

    // Use PromptAssembler for modular, budget-aware prompt construction
    let model_window = effective_token_limit(app_config, &runtime_model);
    let mut assembler = PromptAssembler::default(
        base_system_prompt,
        Some(TASK_MANAGEMENT_GUIDE),
        None,
        model_window,
    );
    // Inject the unified instruction/profile context so the agent's system prompt
    // reflects EKO user/project/local instruction files. Dynamic long-term
    // memories stay query-dependent and are recalled per turn through the Store.
    if let Some(ref memory_suffix) = params.memory_context_suffix {
        assembler.add_instruction_context(memory_suffix);
    }

    // Resolve subagent .md scopes early so the role catalog can be injected
    // into the system prompt before build (same defs used by register_default_subagents).
    let subagent_project_root = params
        .project
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(|| params.working_dir.clone())
        .or_else(|| crate::project::context::discover_project_root(None));
    let subagent_user_home = dirs::home_dir();
    let discovered_subagents = crate::subagent_loader::discover_subagents(
        subagent_project_root.as_deref(),
        subagent_user_home.as_deref(),
    );
    let subagent_catalog_snapshot = Arc::new(
        crate::subagent_loader::SubagentCatalogSnapshot::from_definitions(&discovered_subagents),
    );
    crate::tasks::task_runtime::profiles::validate_default_subagent_routes(
        &subagent_catalog_snapshot,
    )?;
    let subagent_catalog = subagent_catalog_snapshot.prompt();
    assembler.add_subagent_catalog(&subagent_catalog);
    let prompt_assembly = assembler.assemble_with_report();
    let system_prompt = prompt_assembly.prompt.clone();

    // Determine config values from AppConfig
    let token_limit = effective_token_limit(app_config, &runtime_model);
    let max_tool_output_tokens =
        resolved_max_tool_output_tokens(app_config.agent.max_tool_output_tokens);
    let sandbox_manager = Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox());
    let command_cells =
        crate::tasks::task_runtime::command_cells::shared_command_cells(sandbox_manager.clone())?;
    let subagent_prompt_compiler: Arc<dyn SubagentPromptCompiler> =
        Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler);
    let subagent_registry = Arc::new(echo_agent::agent::subagent::SubagentRegistry::new());
    let run_code_available = sandbox_manager.has_local_os_sandbox().await;
    if !run_code_available {
        tracing::warn!("OS sandbox unavailable; run_code will be disabled for this EKO runtime");
    }

    // Use ReactAgentBuilder — mode is resolved at the CLI layer,
    // framework only receives model + system_prompt + tools.
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(&app_config.agent.name)
        .system_prompt(&system_prompt)
        .enable_tools()
        .enable_memory()
        .command_cells(command_cells.clone())
        // EKO owns planning through TaskRuntime. Background command cells
        // (shell background=true + wait/stop_cell/list_cells) are shared
        // execution primitives with the same safety classifier — not a second
        // task API — so they coexist with task_create/task_execute.
        .enable_subagent()
        .subagent_registry(subagent_registry.clone())
        .subagent_prompt_compiler(subagent_prompt_compiler.clone())
        .register_agent_dispatch_tool() // Phase 0: ad-hoc agent_tool alongside task_execute
        .enable_human_in_loop()
        .max_iterations(app_config.agent.max_iterations)
        .token_limit(token_limit)
        .max_tool_output_tokens(max_tool_output_tokens)
        .tool_output_artifacts(tool_output_artifact_config(params.working_dir.as_deref()))
        .max_tokens(Some(max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: app_config.agent.tool_timeout_ms,
            ..Default::default()
        })
        .sandbox_manager(sandbox_manager.clone())
        .enable_cot();

    // ── Pass the resolved configured model to the LLM client ──
    // Without this, the agent falls back to env vars + echo-agent-models.yaml,
    // which may not exist (especially in GUI apps where shell env vars aren't inherited).
    let provider = runtime_model.provider.as_str();
    let base_url_override = runtime_model.base_url.as_deref();
    let prepared_llm = prepare_runtime_llm(&runtime_model)?;
    let llm_config = prepared_llm.config.clone();
    tracing::info!(
        provider = provider,
        model = model,
        auth_source = %runtime_model.auth_source,
        has_base_url = base_url_override.is_some(),
        "Injecting LlmConfig from configured model"
    );
    let injected_llm_config = Some(llm_config.clone());
    builder = builder.llm_config(llm_config);

    // Set session_id if provided (used by background tasks for checkpoint isolation)
    if let Some(ref sid) = params.session_id {
        builder = builder.session_id(sid.as_str());
    }

    // Set conversation_id if provided. `conversation_id` (distinct from `session_id`)
    // is what `save_runtime_checkpoint` and `ConversationStore` projection key on —
    // without it the framework's checkpoint helpers silently no-op.
    if let Some(ref cid) = params.conversation_id {
        builder = builder.conversation_id(cid.as_str());
    }

    // Bind the session working directory (worktree isolation). Propagated to
    // ReactAgent.config.working_dir, then into every tool's ToolContext via
    // ExecuteStage, so shell/file/git tools run inside the worktree.
    if let Some(ref wd) = params.working_dir {
        builder = builder.working_dir(wd.clone());
    }

    // Inject a workspace/project-scoped memory Store (FileStore). This OVERRIDES
    // the framework's default global `~/.eko/store.json` — dynamic
    // memories (remember / accepted evidence / L3 promotion / TaskRuntime bridge) are
    // physically isolated per project so they don't leak across projects,
    // mirroring how hot-layer `MEMORY.md` already follows the project root.
    // `params.working_dir` (workspace root) takes priority; otherwise we walk
    // up from cwd to find a project root; falling back to the global store.
    //
    // On success, `.store()` makes the builder flip `enable_memory(false)` and
    // inject our store instead of the auto-FileStore (see ReactAgentBuilder::build).
    // On failure we leave `.enable_memory()` in effect so the framework still
    // wires its default store — memory stays usable, just not project-scoped.
    let memory_workspace_root = params.working_dir.as_deref().map(std::path::Path::new);
    let (memory_store_path, _) = resolve_memory_store_paths(memory_workspace_root);
    if let Some(store) = create_memory_store_at(&memory_store_path) {
        builder = builder.store(store);
    }

    // Inject the shared runtime state store. When the product layer supplies a
    // store and a conversation_id, every iteration of `run_core_loop` writes an
    // `AgentCheckpoint` so the conversation can be resumed across restarts.
    if let Some(ref store) = params.state_store {
        builder = builder.state_store(store.clone());
    }

    // Set React loop checkpoint interval if provided (used by background tasks
    // to enable crash recovery — saves conversation history every N iterations)
    if let Some(interval) = params.react_checkpoint_interval
        && interval > 0
    {
        builder = builder.react_checkpoint_interval(interval);
    }

    // Initialize JSONL run store for trace persistence (before build)
    {
        let run_dir = echo_agent::paths::user_data_path("runs");
        match JsonlRunStore::new(&run_dir) {
            Ok(store) => {
                builder = builder.with_run_store(Arc::new(store));
            }
            Err(e) => {
                tracing::warn!("Failed to initialize run store: {e}");
            }
        }
    }

    // Resolve the subagent .md scopes: project root (params.project → working_dir
    // → auto-discover) and user home. Already resolved above for the prompt
    // catalog; reused here for worktree factory + registration.
    // Sprint 8: the same project root also seeds the worktree factory below.
    // (subagent_project_root / subagent_user_home computed before builder)

    // Sprint 8: inject a worktree-isolation factory so Fork-dispatched writer
    // subagents (those declaring `isolate_worktree: true`) can run in isolated
    // git worktrees. Resolve the git repo root best-effort from the project
    // root; if it's not a git repo, no factory is injected (subagents declaring
    // isolation log a warning and run unisolated — the framework's default).
    // No new permission gate: worktree is a user-driven isolation tool
    // (AGENTS.md local-assistant positioning); factory failure still hard-fails
    // the dispatch (data-loss guard, not a permission check).
    let worktree_factory = subagent_project_root.as_ref().and_then(|root| {
        crate::tasks::task_runtime::worktree::git_repo_root(root)
            .ok()
            .map(crate::tasks::task_runtime::worktree::EkoWorktreeFactory::new)
            .map(|f| {
                let arc: std::sync::Arc<
                    dyn echo_agent::agent::subagent::worktree::WorktreeFactory,
                > = std::sync::Arc::new(f);
                arc
            })
    });
    let builder = if let Some(factory) = worktree_factory {
        builder.subagent_worktree_factory(factory)
    } else {
        builder
    };

    // Sprint 10: always inject a data-workspace factory (no git dependency —
    // unlike worktree, tmpdir works anywhere). Fork-dispatched data/research
    // subagents declaring `isolate_workspace: true` get a per-subagent tmpdir so
    // parallel runs emit disjoint output files. Optional base_dir keeps them
    // debuggable under a known parent; fall back to OS temp.
    let data_workspace_factory: std::sync::Arc<
        dyn echo_agent::agent::subagent::workspace::DataWorkspaceFactory,
    > = std::sync::Arc::new(crate::tasks::task_runtime::worktree::EkoDataWorkspaceFactory::new());
    let builder = builder.subagent_data_workspace_factory(data_workspace_factory);

    // Sprint 11: inject a RuntimeStateStore for team-mode checkpoint/resume.
    // `dispatch_team` plumbs it into TeamAgent so a timed-out team run can
    // resume by skipping completed plan/subagent/synthesis phases (DAG
    // skip-on-resume). Reuses the same FileRuntimeStateStore the runtime
    // checkpoint path uses. None if the store couldn't be constructed (teams
    // then run in-memory).
    let builder = if let Some(state_store) = create_runtime_state_store() {
        builder.subagent_runtime_state_store(state_store)
    } else {
        builder
    };

    let mut agent = builder.build().map_err(|e| {
        tracing::error!("Failed to build agent: {e}");
        format!("Failed to initialize agent: {e}. Please check your configuration and try again.")
    })?;
    // `prepare_runtime_llm` already constructed this exact client before any
    // runtime state was accepted. Install it explicitly so agent bootstrap can
    // never fall back to an environment/YAML client after a swallowed rebuild.
    agent.set_llm_client(prepared_llm.client.clone());
    refresh_dynamic_context(&mut agent, subagent_project_root.as_deref()).await;
    configure_run_code_capability(&mut agent, run_code_available);
    agent.set_pre_model_context_projector(Some(std::sync::Arc::new(
        crate::turn_context::EkoContextProjector::new(
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
            crate::turn_context::turn_prompt_context_registry(),
        ),
    )));
    let cache_user_id = load_or_create_cache_user_id();
    agent.config_mut().set_cache_user_id(&cache_user_id);

    if let Some(browser_runtime) = &params.browser_runtime {
        browser_runtime.install_tools(&mut agent);
    }

    // Inject LlmCritic for self-verification. The critic scores the agent's
    // final_answer; if below threshold (7.0), feedback is injected and the
    // agent retries (up to verifier_max_retries=2).
    // The critic consumes the exact prepared transport used by the parent;
    // it never re-resolves global config or assumes Chat Completions.
    // Fail-open on errors (verify.rs:91-93) ensures the main flow is never
    // blocked if the critic LLM call fails.
    agent.set_owned_critic(
        EKO_MODEL_CRITIC_OWNER,
        std::sync::Arc::new(
            echo_agent::agent::critic::LlmCritic::new(prepared_llm.client.clone())
                .with_pass_threshold(7.0)
                .with_cache_user_id(cache_user_id.clone()),
        ),
    );
    agent.config_mut().set_verifier_enabled(true);
    tracing::info!("main agent: Critic self-verification enabled (threshold=7.0, max_retries=2)");

    tracing::info!(
        has_llm_config = injected_llm_config.is_some(),
        model = %model,
        "main agent: registering default subagents with llm_config={}",
        injected_llm_config.as_ref().map(|c| c.model.as_str()).unwrap_or("NONE")
    );
    let model_consumers = register_default_subagents(
        &mut agent,
        SubagentRuntimeGeneration {
            model: model.to_string(),
            llm_config: injected_llm_config,
            llm_client: Some(prepared_llm.client.clone()),
            temperature,
            max_tokens,
            token_limit,
            thinking: prepared_llm.thinking.clone(),
        },
        app_config.agent.tool_timeout_ms,
        max_tool_output_tokens,
        &cache_user_id,
        &discovered_subagents,
        subagent_prompt_compiler.clone(),
        subagent_registry,
        params.browser_runtime.clone(),
        sandbox_manager,
        command_cells.clone(),
        run_code_available,
    )
    .await;
    crate::tasks::task_runtime::command_cells::install_watch_cell_tool(&mut agent, command_cells);

    // Register default hooks
    register_default_hooks(&mut agent);

    // Register task-management tools when a TaskRuntimeStore is available.
    // These let the main Agent atomically create, revise, and inspect one
    // task graph shared by the todo and DAG projections.
    // The store handle is threaded from AppState → SharedResources → params.
    if let Some(store) = &params.task_runtime_store {
        use crate::tasks::task_runtime::task_tools::TaskCapabilityCatalog;
        let store = Arc::clone(store);
        let tool_names = agent.tool_names();
        let capabilities = Arc::new(TaskCapabilityCatalog::new(
            subagent_catalog_snapshot.clone(),
            tool_names,
        ));
        let revision_service =
            crate::tasks::task_runtime::build_eko_task_revision_service(store, capabilities);
        echo_agent::tasks::register_task_tools(&mut agent, revision_service);
        tracing::info!(
            "Registered revisioned task-management tools (task_create/task_update/task_list)"
        );
    }

    Ok(CreatedAgent {
        agent,
        prompt_assembly,
        model_consumers,
        runtime_model,
    })
}

/// Refresh every workspace-dependent context projection on an agent.
pub async fn refresh_dynamic_context(agent: &mut ReactAgent, root: Option<&std::path::Path>) {
    crate::unified_memory::refresh_memory_projections(agent, root).await;
    crate::project::prompt::refresh_project_context_projection(agent, root).await;
}

fn configure_run_code_capability(agent: &mut ReactAgent, available: bool) {
    if available {
        return;
    }
    if agent.remove_tool("run_code").is_some() {
        tracing::warn!(
            agent = %agent.model_name(),
            "run_code removed because no OS-level sandbox is available"
        );
    }
}

/// Resolve a subagent model frontmatter value to a concrete model id.
///
/// - `None` / omitted → parent model
/// - `"inherit"` → current parent model, fixed at registration
/// - `"fast"` → `EKO_FAST_MODEL` env if set, else parent model
/// - any other string → used as-is
pub fn resolve_subagent_model(spec: Option<&str>, parent_model: &str) -> String {
    match spec {
        None => parent_model.to_string(),
        Some("inherit") => parent_model.to_string(),
        Some("fast") => std::env::var("EKO_FAST_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| parent_model.to_string()),
        Some(other) => other.to_string(),
    }
}

/// Register readonly subagents on the given agent.
///
/// Subagent definitions are **hot-loaded from `.md` files** (Sprint 6): project
/// scope `<root>/.eko/subagents/**/*.md` overrides user scope
/// `~/.eko/subagents/**/*.md`, which overrides the builtin defaults
/// compiled into the binary (`src/subagents/coding/*.md`). Editing a `.md`
/// prompt therefore takes effect on next agent build without recompiling.
///
/// Only `readonly` subagents are registered here (the 4 generic capability
/// roles: explorer/reviewer/planner/summarizer). Domain specialization lives
/// in the context/skill layer, not separate agent definitions — aligns with
/// industry consensus (Claude Code ~3 subagents, OpenHands microagents,
/// Devin single-agent). Used by the main agent for L2 delegation.
#[allow(clippy::too_many_arguments)]
async fn register_default_subagents(
    agent: &mut ReactAgent,
    parent_generation: SubagentRuntimeGeneration,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    subagents: &[crate::subagent_loader::SubagentDefinition],
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    subagent_registry: Arc<echo_agent::agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
    run_code_available: bool,
) -> AgentModelConsumers {
    let tool_output_artifacts = agent.tool_output_artifacts();
    tracing::info!(
        count = subagents.len(),
        names = ?subagents.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
        "Loaded subagent definitions from .md (project/user/builtin)"
    );

    struct BuiltSubagent {
        definition: echo_agent::agent::subagent::SubagentDefinition,
        handle: crate::agent_handle::AgentHandle,
        fork_factory: Arc<dyn AgentFactory>,
        readonly: bool,
        isolate_worktree: bool,
        isolate_workspace: bool,
        has_team: bool,
        can_delegate: bool,
        inherits_parent_model: bool,
        tags: Vec<String>,
    }

    let inherited_generation = Arc::new(tokio::sync::RwLock::new(parent_generation.clone()));
    let mut built_subagents: Vec<BuiltSubagent> = Vec::with_capacity(subagents.len());
    for subagent_def in subagents {
        // Sprint 9: register BOTH readonly and writer subagents. Readonly subagents
        // get the readonly tool subset (physical no-write enforcement); writer
        // subagents get the full tool set (shell/file/git) and run inside an
        // isolated git worktree when `isolate_worktree` is set (Sprint 8 wiring).
        // TaskRuntime may run disjoint exact owners concurrently; every writer
        // still gets a separate checkout and a reviewed integration boundary.
        let model_binding = subagent_model_binding(
            subagent_def.model.as_deref(),
            subagent_def.thinking.as_deref(),
            &parent_generation,
            &inherited_generation,
        );
        let initial_generation = model_binding.snapshot().await;
        let max_iterations = subagent_def.max_turns.unwrap_or(0);
        let isolation = crate::subagent_loader::subagent_isolation(subagent_def);
        // Per-subagent thinking: an explicit role spec wins; otherwise the
        // generation's (inherited parent or registration-time fixed) thinking.
        let initial_thinking =
            subagent_build_thinking(subagent_def.thinking.as_deref(), &initial_generation);
        let compiled_system = prompt_compiler.compile_system(&SubagentSystemPromptInput {
            name: &subagent_def.name,
            description: &subagent_def.description,
            role_prompt: &subagent_def.system_prompt,
            readonly: subagent_def.readonly,
            // Wire the frontmatter declaration through so the system-prompt
            // delegation wording matches the `.md` claim and the parent-facing
            // catalog (previously hardcoded false — a display inconsistency).
            can_delegate: subagent_def.can_delegate,
            isolation,
            environment: Some(static_subagent_environment()),
        });
        let build_result = if subagent_def.readonly {
            build_readonly_subagent_agent(
                &subagent_def.name,
                &compiled_system.system_prompt,
                &initial_generation.model,
                initial_generation.llm_config.clone(),
                initial_generation.llm_client.clone(),
                initial_generation.temperature,
                initial_generation.max_tokens,
                initial_generation.token_limit,
                initial_thinking,
                tool_timeout_ms,
                max_tool_output_tokens,
                cache_user_id,
                max_iterations,
                subagent_def.can_delegate,
                prompt_compiler.clone(),
                subagent_registry.clone(),
                browser_runtime.clone(),
                command_cells.clone(),
            )
        } else {
            build_writer_subagent_agent(
                &subagent_def.name,
                &compiled_system.system_prompt,
                &initial_generation.model,
                initial_generation.llm_config.clone(),
                initial_generation.llm_client.clone(),
                initial_generation.temperature,
                initial_generation.max_tokens,
                initial_generation.token_limit,
                initial_thinking,
                tool_timeout_ms,
                max_tool_output_tokens,
                cache_user_id,
                max_iterations,
                subagent_def.can_delegate,
                prompt_compiler.clone(),
                subagent_registry.clone(),
                browser_runtime.clone(),
                sandbox_manager.clone(),
                command_cells.clone(),
                run_code_available,
            )
        };
        match build_result {
            Ok(subagent) => {
                subagent.set_tool_output_artifacts(tool_output_artifacts.clone());
                crate::tasks::task_runtime::compact_context::install_task_context_protection(
                    &subagent,
                )
                .await;
                let subagent_handle = crate::agent_handle::AgentHandle::new(subagent);

                let mut builder = SubagentBuilder::new(&subagent_def.name)
                    .description(&subagent_def.description)
                    .fork_mode();
                if let Some(path) = subagent_def
                    .source
                    .strip_prefix("project:")
                    .or_else(|| subagent_def.source.strip_prefix("user:"))
                {
                    builder = builder.custom(path);
                }
                // Sprint 8/9: honor the frontmatter `worktree: true` flag (only
                // set for non-readonly writers; readonly subagents have it cleared
                // by the loader since they don't mutate files). This makes the
                // framework's dispatch_fork create an isolated worktree for the
                // writer (eko-fork-<label> branch).
                if subagent_def.isolate_worktree {
                    builder = builder.isolate_worktree();
                }
                // Sprint 10: honor the frontmatter `workspace: true` flag for
                // data/research subagents (per-subagent tmpdir, disjoint outputs).
                // Loader clears it when worktree is active (mutually exclusive).
                if subagent_def.isolate_workspace {
                    builder = builder.isolate_workspace();
                }
                // Sprint 11: if this .md declares a team (team_strategy +
                // manager + subagent team members), override execution_mode to Team and
                // attach the TeamSpec. dispatch_team resolves the named
                // manager/subagents from the registry at dispatch time.
                if let Some(spec) = subagent_def.team.clone() {
                    builder = builder.team(spec);
                }
                if let Some(ref m) = subagent_def.model {
                    builder = builder.model(m);
                }
                if let Some(max_turns) = subagent_def.max_turns {
                    builder = builder.max_iterations(max_turns);
                }
                // Per-subagent execution timeout (e.g. the awaiter watches one
                // background cell for up to `timeout_secs` before the framework
                // escalates). None → framework default (no timeout).
                if let Some(timeout_secs) = subagent_def.timeout_secs {
                    builder = builder.timeout(timeout_secs);
                }
                if subagent_def.is_background {
                    builder = builder.background().tag("background");
                }
                if subagent_def.can_delegate {
                    builder = builder.can_delegate();
                }
                builder = builder.tag(format!("prompt_source:{}", subagent_def.source));
                builder = builder.tag(if subagent_def.readonly {
                    "capability:readonly"
                } else {
                    "capability:writer"
                });
                builder = builder.tag(if subagent_def.isolate_worktree {
                    "isolation:worktree"
                } else if subagent_def.isolate_workspace {
                    "isolation:workspace"
                } else {
                    "isolation:context"
                });
                for tag in &subagent_def.tags {
                    builder = builder.tag(tag);
                }
                let def = builder.build();
                let factory_def = subagent_def.clone();
                let factory_model_binding = model_binding.clone();
                let factory_cache_user_id = cache_user_id.to_string();
                let factory_browser_runtime = browser_runtime.clone();
                let factory_sandbox_manager = sandbox_manager.clone();
                let factory_command_cells = command_cells.clone();
                let factory_tool_output_artifacts = tool_output_artifacts.clone();
                let factory_system_prompt = compiled_system.system_prompt.clone();
                let factory_prompt_compiler = prompt_compiler.clone();
                let factory_subagent_registry = subagent_registry.clone();
                let fork_factory = Arc::new(FnAgentFactory::new(
                    move || -> BoxFuture<'static, echo_agent::error::Result<Box<dyn Agent>>> {
                        let subagent_def = factory_def.clone();
                        let model_binding = factory_model_binding.clone();
                        let cache_user_id = factory_cache_user_id.clone();
                        let browser_runtime = factory_browser_runtime.clone();
                        let sandbox_manager = factory_sandbox_manager.clone();
                        let command_cells = factory_command_cells.clone();
                        let tool_output_artifacts = factory_tool_output_artifacts.clone();
                        let system_prompt = factory_system_prompt.clone();
                        let prompt_compiler = factory_prompt_compiler.clone();
                        let subagent_registry = factory_subagent_registry.clone();
                        Box::pin(async move {
                            let model_generation = model_binding.snapshot().await;
                            let max_iterations = subagent_def.max_turns.unwrap_or(0);
                            // Re-resolve per fork build: role spec wins, else the
                            // snapshotted generation thinking (Inherit tracks the
                            // parent's hot-swaps, Fixed stays registration-time).
                            let thinking = subagent_build_thinking(
                                subagent_def.thinking.as_deref(),
                                &model_generation,
                            );
                            let catalog_registry = subagent_registry.clone();
                            let subagent = if subagent_def.readonly {
                                build_readonly_subagent_agent(
                                    &subagent_def.name,
                                    &system_prompt,
                                    &model_generation.model,
                                    model_generation.llm_config,
                                    model_generation.llm_client,
                                    model_generation.temperature,
                                    model_generation.max_tokens,
                                    model_generation.token_limit,
                                    thinking,
                                    tool_timeout_ms,
                                    max_tool_output_tokens,
                                    &cache_user_id,
                                    max_iterations,
                                    subagent_def.can_delegate,
                                    prompt_compiler,
                                    subagent_registry,
                                    browser_runtime,
                                    command_cells,
                                )?
                            } else {
                                build_writer_subagent_agent(
                                    &subagent_def.name,
                                    &system_prompt,
                                    &model_generation.model,
                                    model_generation.llm_config,
                                    model_generation.llm_client,
                                    model_generation.temperature,
                                    model_generation.max_tokens,
                                    model_generation.token_limit,
                                    thinking,
                                    tool_timeout_ms,
                                    max_tool_output_tokens,
                                    &cache_user_id,
                                    max_iterations,
                                    subagent_def.can_delegate,
                                    prompt_compiler,
                                    subagent_registry,
                                    browser_runtime,
                                    sandbox_manager,
                                    command_cells,
                                    run_code_available,
                                )?
                            };
                            subagent.set_tool_output_artifacts(tool_output_artifacts);
                            if subagent_def.can_delegate {
                                let definitions = catalog_registry.list_available().await;
                                subagent.sync_subagent_dispatch_catalog(&definitions);
                            }
                            crate::tasks::task_runtime::compact_context::install_task_context_protection(
                                &subagent,
                            )
                            .await;
                            Ok(Box::new(subagent) as Box<dyn Agent>)
                        })
                    },
                )) as Arc<dyn AgentFactory>;
                built_subagents.push(BuiltSubagent {
                    definition: def,
                    handle: subagent_handle,
                    fork_factory,
                    readonly: subagent_def.readonly,
                    isolate_worktree: subagent_def.isolate_worktree,
                    isolate_workspace: subagent_def.isolate_workspace,
                    has_team: subagent_def.team.is_some(),
                    can_delegate: subagent_def.can_delegate,
                    inherits_parent_model: subagent_def.model.is_none(),
                    tags: subagent_def.tags.clone(),
                });
            }
            Err(err) => tracing::warn!(
                subagent = %subagent_def.name,
                readonly = subagent_def.readonly,
                error = %err,
                "Failed to build default subagent"
            ),
        }
    }

    // Register every subagent on the primary agent.
    for built in &built_subagents {
        agent.register_subagent_with_definition(
            built.definition.clone(),
            built.handle.to_boxed_agent().await,
        );
        agent.register_subagent_factory(built.definition.clone(), built.fork_factory.clone());
        tracing::info!(
            subagent = %built.definition.name,
            readonly = built.readonly,
            isolate_worktree = built.isolate_worktree,
            isolate_workspace = built.isolate_workspace,
            has_team = built.has_team,
            tags = ?built.tags,
            "registered default subagent"
        );
    }
    let registered_definitions = subagent_registry.list_available().await;
    for built in &built_subagents {
        if built.can_delegate {
            built
                .handle
                .write(|subagent| {
                    subagent.sync_subagent_dispatch_catalog(&registered_definitions);
                })
                .await;
        }
    }
    let inherited_factories = built_subagents
        .iter()
        .filter(|built| built.inherits_parent_model)
        .map(|built| InheritedSubagentFactory {
            definition: built.definition.clone(),
            handle: built.handle.clone(),
            factory: built.fork_factory.clone(),
        })
        .collect();
    AgentModelConsumers {
        inherited_generation,
        registry: subagent_registry,
        inherited_factories: Arc::new(inherited_factories),
    }
}

/// Build a **writer** subagent (Sprint 9): same as the readonly subagent
/// but with full write tools (shell/file/git) instead of the readonly subset.
/// Used for `Implementation`/`Debugging` tasks that route to Fork subagents in
/// isolated git worktrees. TaskRuntime runs disjoint exact owners concurrently,
/// while overlapping or unknown ownership is split into separate write waves.
#[allow(clippy::too_many_arguments)]
fn build_writer_subagent_agent(
    name: &str,
    prompt: &str,
    model: &str,
    llm_config: Option<LlmConfig>,
    llm_client: Option<Arc<dyn LlmClient>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    thinking: Option<echo_agent::llm::ThinkingConfig>,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    max_iterations: usize,
    can_delegate: bool,
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    subagent_registry: Arc<echo_agent::agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
    run_code_available: bool,
) -> std::result::Result<ReactAgent, echo_agent::error::ReactError> {
    // Mirror build_readonly_subagent_agent, but OMIT `.readonly_tools()` → the
    // default `readonly_tools: false` triggers `register_all_tools`, giving the
    // writer shell/file/git write capability. Isolation is enforced physically
    // by the worktree (Sprint 8): the subagent's working_dir is bound to its own
    // worktree checkout, so writes can't reach the main workspace even though
    // the tools could.
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(name)
        .system_prompt(prompt)
        .enable_tools()
        // NO .readonly_tools() → full tool set (write capability).
        .enable_memory()
        .enable_cot()
        .max_iterations(max_iterations)
        .token_limit(token_limit)
        .max_tool_output_tokens(max_tool_output_tokens)
        .max_tokens(max_tokens.or(Some(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: tool_timeout_ms,
            ..Default::default()
        })
        .sandbox_manager(sandbox_manager)
        // 与主智能体共享同一进程级 registry；cell 自身通过同一个 sandbox
        // executor 执行，不会从前台策略静默降级为宿主机直连。
        .command_cells(command_cells);

    if can_delegate {
        builder = builder
            .enable_subagent()
            .subagent_registry(subagent_registry)
            .subagent_prompt_compiler(prompt_compiler)
            .register_agent_dispatch_tool();
    }

    let has_llm_config = llm_config.is_some();
    if let Some(config) = llm_config {
        tracing::info!(
            subagent_name = name,
            model = %config.model,
            has_auth = !config.api_key.is_empty(),
            "writer subagent: injecting LlmConfig"
        );
        builder = builder.llm_config(config);
    } else {
        tracing::warn!(
            subagent_name = name,
            "writer subagent: NO LlmConfig injected — will fall back to env vars / models.yaml"
        );
    }

    let mut subagent = builder.build()?;
    if let Some(client) = llm_client {
        subagent.set_llm_client(client);
    }
    // Per-subagent reasoning depth: role spec or inherited parent generation
    // thinking, applied to every chat request this subagent issues.
    subagent.set_thinking(thinking);
    configure_run_code_capability(&mut subagent, run_code_available);
    if let Some(browser_runtime) = browser_runtime {
        browser_runtime.install_subagent_tools(&mut subagent);
    }
    let has_client = subagent.llm_client().is_some();
    tracing::info!(
        subagent_name = name,
        has_llm_config,
        has_llm_client = has_client,
        model = %subagent.model_name(),
        "writer subagent built: LLM client status (full write tools)"
    );
    subagent.config_mut().set_cache_user_id(cache_user_id);
    Ok(subagent)
}

#[allow(clippy::too_many_arguments)]
fn build_readonly_subagent_agent(
    name: &str,
    prompt: &str,
    model: &str,
    llm_config: Option<LlmConfig>,
    llm_client: Option<Arc<dyn LlmClient>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    thinking: Option<echo_agent::llm::ThinkingConfig>,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    max_iterations: usize,
    can_delegate: bool,
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    subagent_registry: Arc<echo_agent::agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
) -> std::result::Result<ReactAgent, echo_agent::error::ReactError> {
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(name)
        .system_prompt(prompt)
        .enable_tools()
        .readonly_tools() // SA-2: physical enforcement — no shell/write tools
        .enable_memory()
        .enable_cot()
        .max_iterations(max_iterations)
        .token_limit(token_limit)
        .max_tool_output_tokens(max_tool_output_tokens)
        .max_tokens(max_tokens.or(Some(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: tool_timeout_ms,
            ..Default::default()
        })
        // readonly 子智能体没有 shell,但注入共享 cell registry 后仍获得
        // wait/stop_cell/list_cells——awaiter 角色正是靠这组工具等待后台命令。
        .command_cells(command_cells);

    if can_delegate {
        builder = builder
            .enable_subagent()
            .subagent_registry(subagent_registry)
            .subagent_prompt_compiler(prompt_compiler)
            .register_agent_dispatch_tool();
    }

    let has_llm_config = llm_config.is_some();
    if let Some(config) = llm_config {
        tracing::info!(
            subagent_name = name,
            model = %config.model,
            has_auth = !config.api_key.is_empty(),
            "subagent: injecting LlmConfig"
        );
        builder = builder.llm_config(config);
    } else {
        tracing::warn!(
            subagent_name = name,
            "subagent: NO LlmConfig injected — will fall back to env vars / models.yaml"
        );
    }

    let mut subagent = builder.build()?;
    if let Some(client) = llm_client {
        subagent.set_llm_client(client);
    }
    // Per-subagent reasoning depth: role spec or inherited parent generation
    // thinking, applied to every chat request this subagent issues.
    subagent.set_thinking(thinking);
    if let Some(browser_runtime) = browser_runtime {
        browser_runtime.install_subagent_tools(&mut subagent);
    }
    let has_client = subagent.llm_client().is_some();
    tracing::info!(
        subagent_name = name,
        has_llm_config,
        has_llm_client = has_client,
        model = %subagent.model_name(),
        "subagent built: LLM client status"
    );
    subagent.config_mut().set_cache_user_id(cache_user_id);
    Ok(subagent)
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
/// When a pass changes the hot layer, the primary and pooled agents refresh
/// their replaceable instruction projection immediately. Best-effort: errors
/// are logged and the next pass still runs.
pub fn spawn_dreaming_task(
    review_integration: Arc<crate::evolution::ReviewIntegration>,
    primary_agent: crate::agent_handle::AgentHandle,
    pool: Option<Arc<crate::agent_pool::AgentPool>>,
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
                    let pass = run_dreaming_pass(
                        &review_integration,
                        &primary_agent,
                        pool.as_ref(),
                    );
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
    primary_agent: &crate::agent_handle::AgentHandle,
    pool: Option<&Arc<crate::agent_pool::AgentPool>>,
) -> anyhow::Result<echo_agent::evolution::DreamingReport> {
    // The lease covers both framework writes and EKO's replaceable projection
    // refresh. A workspace transition therefore either observes the complete
    // old generation or returns Busy before publishing the new generation.
    let generation_lease = review_integration
        .lease_generation()
        .map_err(anyhow::Error::from)?;
    let layer_manager = std::sync::Arc::new(generation_lease.create_layer_manager());
    let dreaming = echo_agent::evolution::Dreaming::new(
        layer_manager,
        echo_agent::evolution::DreamingConfig::default(),
    );
    let report = dreaming.run().await.map_err(anyhow::Error::from)?;
    if report.promoted > 0 {
        let root = primary_agent.read(|agent| agent.working_dir()).await;
        primary_agent
            .write_async(|agent| {
                Box::pin(async move {
                    crate::unified_memory::refresh_hot_memory_projection(agent, root.as_deref())
                        .await;
                })
            })
            .await;
        if let Some(agent_pool) = pool {
            agent_pool.refresh_hot_memory_context().await;
        }
    }
    Ok(report)
}

/// 创建对话持久化 Store（文件），失败时返回 None（禁用持久化）
pub fn create_conversation_store() -> Option<Arc<dyn ConversationStore>> {
    let base = echo_agent::paths::user_data_dir();

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
    create_runtime_state_store_in(echo_agent::paths::user_data_dir())
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
    let echo_agent_dir = echo_agent::paths::user_data_dir();
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

type BootstrapShutdownFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a>>;

struct BootstrapShutdownStep<'a> {
    name: &'static str,
    future: BootstrapShutdownFuture<'a>,
}

async fn drain_bootstrap_shutdown_steps(
    primary_error: anyhow::Error,
    steps: Vec<BootstrapShutdownStep<'_>>,
) -> anyhow::Error {
    let mut errors = vec![format!("service bootstrap: {primary_error}")];
    for step in steps {
        if let Err(error) = step.future.await {
            tracing::warn!(step = step.name, %error, "bootstrap shutdown step failed");
            errors.push(format!("{}: {error}", step.name));
        }
    }
    anyhow::anyhow!(errors.join("; "))
}

/// Drain application owners that predate scheduler/task-service startup.
/// This is a thin composition helper; each subsystem remains its sole
/// lifecycle authority.
pub async fn settle_service_bootstrap_failure(
    primary_error: anyhow::Error,
    task_runtime_store: Option<&Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    pool: Option<&Arc<crate::agent_pool::AgentPool>>,
    plugin_runtime: &Arc<crate::plugin_runtime::PluginRuntimeService>,
    config_watcher: &Arc<crate::config_watcher::ConfigWatcherHandle>,
    mcp_config_runtime: &Arc<crate::mcp_config_runtime::McpConfigRuntime>,
    browser_runtime: &Arc<crate::browser::BrowserRuntime>,
) -> anyhow::Error {
    let steps = vec![
        BootstrapShutdownStep {
            name: "TaskRun drivers",
            future: Box::pin(async {
                if let Some(store) = task_runtime_store {
                    store
                        .shutdown_run_drivers()
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
        BootstrapShutdownStep {
            name: "agent pool",
            future: Box::pin(async {
                if let Some(pool) = pool {
                    pool.shutdown().await.map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
        BootstrapShutdownStep {
            name: "plugin runtime",
            future: Box::pin(async { plugin_runtime.shutdown().await }),
        },
        BootstrapShutdownStep {
            name: "config watcher",
            future: Box::pin(async { config_watcher.shutdown().await }),
        },
        BootstrapShutdownStep {
            name: "MCP runtime",
            future: Box::pin(async {
                mcp_config_runtime.shutdown().await;
                Ok(())
            }),
        },
        BootstrapShutdownStep {
            name: "browser runtime",
            future: Box::pin(async {
                browser_runtime.shutdown().await;
                Ok(())
            }),
        },
        BootstrapShutdownStep {
            name: "task hook dispatcher",
            future: Box::pin(async {
                if let Some(store) = task_runtime_store {
                    store
                        .shutdown_hook_events()
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Ok(())
            }),
        },
    ];
    drain_bootstrap_shutdown_steps(primary_error, steps).await
}

#[cfg(test)]
mod bootstrap_shutdown_tests {
    use super::{BootstrapShutdownStep, drain_bootstrap_shutdown_steps};
    use std::sync::{Arc, Mutex};

    fn recorded_step(
        calls: Arc<Mutex<Vec<&'static str>>>,
        name: &'static str,
        fail: bool,
    ) -> BootstrapShutdownStep<'static> {
        BootstrapShutdownStep {
            name,
            future: Box::pin(async move {
                calls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("bootstrap shutdown log is unavailable"))?
                    .push(name);
                if fail {
                    Err(anyhow::anyhow!("{name} injected failure"))
                } else {
                    Ok(())
                }
            }),
        }
    }

    #[tokio::test]
    async fn bootstrap_failure_drains_all_started_owners_after_earlier_errors() -> anyhow::Result<()>
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let steps = [
            ("drivers", true),
            ("pool", true),
            ("plugin", false),
            ("watcher", false),
            ("mcp", false),
            ("browser", false),
            ("hooks", false),
        ]
        .into_iter()
        .map(|(name, fail)| recorded_step(Arc::clone(&calls), name, fail))
        .collect();

        let error = drain_bootstrap_shutdown_steps(
            anyhow::anyhow!("injected scheduler bootstrap failure"),
            steps,
        )
        .await;
        let observed = calls
            .lock()
            .map_err(|_| anyhow::anyhow!("bootstrap shutdown log is unavailable"))?
            .clone();

        assert_eq!(
            observed,
            [
                "drivers", "pool", "plugin", "watcher", "mcp", "browser", "hooks"
            ]
        );
        assert!(error.to_string().contains("drivers injected failure"));
        assert!(error.to_string().contains("pool injected failure"));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    Stderr,
    TuiFile,
}

/// Load shell environment variables by spawning the user's login shell.
///
/// On macOS, GUI apps launched from Dock/Finder/Spotlight do NOT inherit
/// shell environment variables (from ~/.zshrc, ~/.bash_profile, etc.).
/// This function bridges that gap — the same approach used by VS Code,
/// JetBrains, and other macOS GUI apps.
///
/// Only sets variables that are NOT already present in the process environment,
/// so explicit env vars always take precedence.
///
/// Only imports known API key variables to avoid polluting the environment
/// with unrelated shell state.
pub fn load_shell_env() {
    #[cfg(target_os = "macos")]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

        // Spawn a login interactive shell and print its environment.
        // -l = login shell (sources ~/.zprofile, ~/.bash_profile, etc.)
        // -i = interactive (sources ~/.zshrc, ~/.bashrc, etc.)
        // -c = run command then exit
        let output = match std::process::Command::new(&shell)
            .args(["-lic", "env"])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("Failed to spawn shell for env loading: {e}");
                return;
            }
        };

        if !output.status.success() {
            tracing::warn!("Shell env command exited with status: {}", output.status);
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Whitelist of env vars to import — known API keys and config paths
        // that EKO needs. These are provider-specific standard names
        // (DEEPSEEK_API_KEY etc.) plus EKO product aliases and MCP path.
        //
        // Note: the framework (echo-agent) does NOT read LLM credential env
        // vars itself — EKO's `resolve_runtime_model` reads them and injects
        // the values into ModelConfig fields before passing to the framework.
        // EKO_AUTH_TOKEN / EKO_BASE_URL / EKO_MODEL are EKO product aliases
        // kept for backwards compatibility with existing user setups.
        const API_KEY_VARS: &[&str] = &[
            "DEEPSEEK_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "DASHSCOPE_API_KEY",
            "QWEN_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_API_KEY",
            "ZHIPU_API_KEY",
            "GLM_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            // EKO product aliases (backwards compat).
            "EKO_AUTH_TOKEN",
            "EKO_BASE_URL",
            "EKO_MODEL",
            "MCP_CONFIG_PATH",
        ];

        // SAFETY: `std::env::set_var` is not thread-safe in Rust. We use a
        // `std::sync::Once` to guarantee this block runs at most once per
        // process lifetime, and it must be called early in `main()` / app
        // startup before background threads are spawned.
        static SHELL_ENV_LOADED: std::sync::Once = std::sync::Once::new();
        let mut loaded = Vec::new();
        SHELL_ENV_LOADED.call_once(|| {
            for line in stdout.lines() {
                if let Some((key, value)) = line.split_once('=')
                    && API_KEY_VARS.contains(&key)
                    && std::env::var(key).is_err()
                    && !value.is_empty()
                {
                    unsafe { std::env::set_var(key, value) };
                    loaded.push(key.to_string());
                }
            }
        });

        if !loaded.is_empty() {
            tracing::info!(vars = loaded.join(", "), "Loaded shell env vars (GUI mode)");
        }
    }
}

pub fn init_logging_for_tui(level: &str) {
    init_logging_with_target(level, LogTarget::TuiFile);
}

pub fn init_logging(level: &str) {
    init_logging_with_target(level, LogTarget::Stderr);
}

/// 本地时区的日志时间格式化器。
///
/// tracing-subscriber 默认用 UTC（RFC3339 带 `Z` 后缀）。本格式器改用
/// `chrono::Local` 输出机器当前时区时间（如 `2026-07-09T09:50:48.876+08:00`），
/// 便于本地排查问题。chrono::Local 读取系统时区（`TZ` 环境变量或系统配置）。
#[cfg(not(feature = "telemetry"))]
struct LocalTimer;

#[cfg(not(feature = "telemetry"))]
impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        // RFC3339 + 本地时区偏移，保留毫秒精度（与默认 SystemTime 精度一致）。
        write!(
            w,
            "{}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        )
    }
}

/// 初始化日志系统（线程安全，仅执行一次）
///
/// When the `telemetry` feature is enabled, this delegates to
/// [`echo_agent::telemetry::init_telemetry`] which sets up OTLP tracing + metrics
/// configured via `OTEL_EXPORTER_OTLP_ENDPOINT` (defaults to `http://localhost:4317`).
pub fn init_logging_with_target(level: &str, target: LogTarget) {
    // `level` is consumed by the EnvFilter below when `telemetry` is off;
    // reference it here so the param is considered used under all feature combos.
    #[cfg(feature = "telemetry")]
    let _ = level;
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
        }

        #[cfg(not(feature = "telemetry"))]
        {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            // Include echo_agent_app_core so the task_runtime module's traces
            // (task_execute/execute_run/drain loop) are visible by default.
            // Previously this crate was omitted, silently hiding all B1-B7
            // instrumentation unless RUST_LOG was set explicitly.
            let default_filter = format!(
                "echo_agent_cli={level},echo_agent={level},echo_agent_app_core={level},tower_http=info"
            );
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
                                    .with_ansi(false)
                                    .with_timer(LocalTimer),
                            )
                            .try_init();
                    }
                }
                LogTarget::Stderr => {
                    // Dual sink: keep the stderr console output (visible in the
                    // `cargo tauri dev` terminal) AND mirror to a rotating-ish
                    // file at ~/.eko/logs/app.log so issues can be
                    // diagnosed after the fact without re-running. Append mode
                    // so restarts don't wipe the log.
                    use tracing_subscriber::layer::SubscriberExt;
                    let registry = tracing_subscriber::registry().with(env_filter());
                    let file_layer = app_log_file().map(|file| {
                        tracing_subscriber::fmt::layer()
                            .with_writer(std::sync::Mutex::new(file))
                            .with_ansi(false)
                            .with_timer(LocalTimer)
                    });
                    if let Some(file_layer) = file_layer {
                        let _ = registry
                            .with(
                                tracing_subscriber::fmt::layer().with_timer(LocalTimer),
                            )
                            .with(file_layer)
                            .try_init();
                    } else {
                        let _ = registry
                            .with(tracing_subscriber::fmt::layer().with_timer(LocalTimer))
                            .try_init();
                    }
                }
            }
        }
    });
}

#[cfg(not(feature = "telemetry"))]
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

    let dir = echo_agent::paths::user_data_path("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tui.log")
}

/// Open the shared GUI app log file for appending: `~/.eko/logs/app.log`.
///
/// Used by the Stderr log target as a second sink so that `cargo tauri dev`
/// output is also persisted to disk (the stderr stream itself is lost once the
/// terminal that launched the app is closed). Append mode keeps history across
/// restarts; rotate/truncate manually if it grows too large.
#[cfg(not(feature = "telemetry"))]
fn app_log_file() -> Option<std::fs::File> {
    let dir = echo_agent::paths::user_data_path("logs");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
        .ok()
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
            if lower.starts_with("gpt-") {
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
                "unknown"
            }
        })
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
    let mut config = echo_agent::config::load_config(None);
    echo_agent::config::apply_env_overrides(&mut config);
    run_base_doctor_for_model(&config.model.get_model_name())
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

    let base = echo_agent::paths::user_data_dir();
    let base_display = base.display();

    let provider = provider_from_model(model);
    let required_keys = model_config::provider_env_vars(provider);
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
        // block_in_place is required when called from within a tokio task
        // context (e.g. from a Tauri command handler).
        let probe_result = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(probe_model_connectivity(model)))
            }
            Err(_) => Err(echo_agent::error::ReactError::Other(
                "Not running in a tokio runtime".to_string(),
            )),
        };
        match probe_result {
            Ok(()) => checks.push(format!("✅ 模型连通性: {} 可用", model)),
            Err(e) => issues.push(format!("❌ 模型连通性检查失败: {}", e)),
        }
    }

    let config_path = base.join("config.yaml");
    if config_path.exists() {
        checks.push(format!("✅ 配置文件: {}/config.yaml", base_display));
    } else {
        issues.push(format!(
            "⚠️  未找到配置文件 {}/config.yaml (使用默认配置)",
            base_display
        ));
    }

    let mcp_path = base.join("mcp.json");
    if mcp_path.exists() {
        checks.push(format!("✅ MCP 配置: {}/mcp.json", base_display));
    } else {
        checks.push(format!(
            "ℹ️  未找到 MCP 配置 (如需工具扩展可创建 {}/mcp.json)",
            base_display
        ));
    }

    if base.exists() {
        checks.push(format!("✅ 数据目录: {}/", base_display));
    } else {
        issues.push(format!(
            "⚠️  数据目录 {}/ 不存在 (运行 echo-agent-cli onboard 初始化)",
            base_display
        ));
    }

    let conv_dir = base.join("conversations");
    if conv_dir.exists() {
        checks.push(format!("✅ 对话存储目录: {}/conversations/", base_display));
    } else {
        checks.push("ℹ️  对话存储目录尚未创建 (首次对话后自动创建)".to_string());
    }

    if let Some(root) = crate::project::context::discover_project_root(None) {
        // Instruction files are loaded by InstructionProvider (the single
        // authority); ProjectContext now carries only structural context.
        let provider = crate::instruction_provider::InstructionProvider::load_for(Some(&root));
        let count = [
            provider.user_level.as_ref(),
            provider.repository_level.as_ref(),
            provider.project_level.as_ref(),
            provider.agents_level.as_ref(),
            provider.local_level.as_ref(),
            provider.hot_memory.as_ref(),
        ]
        .iter()
        .filter(|opt| opt.is_some())
        .count();
        if count == 0 {
            checks.push(
                "ℹ️  项目目录已检测到, 但未找到指令文件 (AGENTS.md / user.md / project.md / learned-rules.md 等)"
                    .to_string(),
            );
        } else {
            checks.push(format!("✅ 项目指令: {count} 个文件已加载"));
        }
    } else {
        checks.push("ℹ️  未检测到项目目录 (可在项目根目录创建 .eko/project.md)".to_string());
    }

    DoctorResult { issues, checks }
}

/// Load user hooks from **all** user-config sources into the agent's hook
/// registry, as a single merged `HooksDefinition`.
///
/// 这是 P0-1 修复后的**唯一** user hook 注册入口(bootstrap 路径)。
/// 它通过 [`crate::hook_config_loader::HookConfigLoader::load_merged`] 把
/// 三个来源(echo-agent.yaml 内嵌 + ~/.eko/hooks.yaml + .eko/hooks.yaml)
/// 按固定顺序合并成单个 `HooksDefinition`,然后**一次性**
/// `clear_user_hooks()` + `register_user_hooks(merged)`。
///
/// **重要**:调用方在调用本函数后,不应再单独加载或注册文件 hooks。
/// 文件 hooks 已包含在本函数的合并结果里。
///
/// 旧的实现只 register `app_config.hooks`(内嵌),把文件来源留给
/// `runtime.rs::bootstrap` 单独 register —— 但 `register_user_hooks`
/// 内部会覆盖 `UserConfig` 单槽位,导致文件来源 clear 掉内嵌来源。
pub async fn load_user_hooks(agent: &AgentHandle, app_config: &AppConfig) {
    let load_result = crate::hook_config_loader::HookConfigLoader::load_merged(app_config);
    for error in &load_result.errors {
        tracing::warn!(%error, "User hook source was not loaded");
    }
    let hooks_def = load_result.definition;
    if hooks_def.is_empty() {
        return;
    }
    let rule_count: usize = hooks_def.rules.values().map(Vec::len).sum();
    agent
        .write_async(|a| {
            Box::pin(async move {
                let mut registry = a.hook_registry().write().await;
                // 一次性 clear + register 合并后的完整 user hook 集。
                // 这里 clear 是为了支持 config reload(避免重复注册);
                // 因为我们已把三源合并,clear 不会丢任何来源。
                registry.clear_user_hooks();
                registry.register_user_hooks(hooks_def);
            })
        })
        .await;
    tracing::info!(
        count = rule_count,
        files = ?load_result.loaded_from,
        "User hooks loaded (merged: inline echo-agent.yaml + hooks.yaml files)"
    );
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
    println!("│                    🏥 EKO 诊断                        │");
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

/// Build an [`LlmConfig`] from the AppConfig's model section.
///
/// Maps the provider string to the appropriate factory method and optionally
/// overrides the base URL. This enables auth_token / base_url from
/// `echo-agent.yaml` to flow through to the agent's LLM client without
/// requiring `echo-agent-models.yaml` or provider-specific env vars.
pub fn build_llm_config(
    provider: &str,
    auth_token: &str,
    model: &str,
    base_url_override: Option<&str>,
    api_protocol: Option<LlmApiProtocol>,
) -> LlmConfig {
    let default_base_url = echo_agent::llm::config::provider_base_url(provider)
        .unwrap_or(echo_agent::llm::config::provider_urls::OPENAI_CHAT_COMPLETIONS);
    let mut config = match provider.to_lowercase().as_str() {
        "anthropic" => LlmConfig::anthropic(auth_token, model),
        "deepseek" => LlmConfig::deepseek(auth_token, model),
        "dashscope" | "qwen" | "aliyun" => LlmConfig::dashscope(auth_token, model),
        _ => {
            // 兜底：按 OpenAI 兼容处理。gemini/azure/ollama 等暂不支持的
            // provider 会落到这里，其 auth 差异可能导致请求失败。
            if matches!(
                provider.to_lowercase().as_str(),
                "gemini" | "google" | "ollama" | "azure" | "azure_openai"
            ) {
                tracing::warn!(
                    provider = %provider,
                    "provider 暂不支持，按 OpenAI 兼容处理（auth 差异可能导致失败）"
                );
            }
            let url = base_url_override.unwrap_or(default_base_url);
            LlmConfig::new(url, auth_token, model)
        }
    };
    // Apply base_url override for non-default providers
    if let Some(url) = base_url_override {
        config.base_url = url.to_string();
    }
    config.api_protocol = api_protocol
        .or_else(|| base_url_override.and_then(LlmApiProtocol::try_from_endpoint))
        .or_else(|| {
            echo_agent::llm::config::provider_metadata(provider)
                .map(|metadata| metadata.default_api_protocol)
        })
        .unwrap_or_else(|| LlmApiProtocol::from_endpoint(&config.base_url));
    // Ensure provider_name is set so the thinking-protocol resolver picks the
    // right wire field (e.g. enable_thinking for dashscope vs reasoning_effort
    // for deepseek). The named constructors already set it; the generic
    // fallback (`LlmConfig::new`) does not, so set it here uniformly.
    if config.provider_name.is_none() && !provider.trim().is_empty() {
        config.provider_name = Some(provider.to_string());
    }
    config
}

/// Convert one fully resolved EKO model into the framework wire configuration.
/// Empty API keys remain valid for local endpoints; required-key policy is
/// enforced separately before this adapter is called.
pub fn build_runtime_llm_config(
    runtime: &model_config::ModelRuntimeConfig,
) -> Result<LlmConfig, String> {
    model_config::validate_runtime_model_endpoint(runtime)?;
    Ok(build_llm_config(
        &runtime.provider,
        runtime.auth_token.as_deref().unwrap_or_default(),
        &runtime.model,
        runtime.base_url.as_deref(),
        Some(runtime.api_protocol),
    ))
}

/// One fully validated EKO runtime model and the client built from it.
///
/// The application uses this as a pre-commit value: invalid headers, tokens,
/// endpoints, and provider/client combinations fail before config persistence
/// or live-agent mutation begins.
#[derive(Clone)]
pub struct PreparedRuntimeLlm {
    pub config: LlmConfig,
    pub client: Arc<dyn LlmClient>,
    pub thinking: Option<echo_agent::llm::ThinkingConfig>,
}

/// Exact admission receipt for publishing one prepared model generation to a
/// live parent agent and its inherited subagent factories.
pub(crate) struct PreparedAgentModelPublication {
    generation: echo_agent::agent::PreparedAgentModelGeneration,
    inherited: Vec<PreparedInheritedSubagentPublication>,
    consumers: AgentModelConsumers,
    runtime: model_config::ModelRuntimeConfig,
    prepared: PreparedRuntimeLlm,
    token_limit: usize,
}

struct PreparedInheritedSubagentPublication {
    generation: echo_agent::agent::PreparedAgentModelGeneration,
}

impl PreparedInheritedSubagentPublication {
    fn commit(self) {
        self.generation.commit();
    }
}

impl PreparedAgentModelPublication {
    /// Publish only pre-owned values. All validation and context admission was
    /// completed by [`prepare_agent_model_publication`].
    pub(crate) async fn commit(self) {
        self.generation.commit();
        for inherited in self.inherited {
            inherited.commit();
        }
        self.consumers
            .publish_inherited_generation(&self.runtime, &self.prepared, self.token_limit)
            .await;
    }
}

/// Resolve and construct the exact client used by every EKO model surface.
pub fn prepare_runtime_llm(
    runtime: &model_config::ModelRuntimeConfig,
) -> Result<PreparedRuntimeLlm, String> {
    model_config::validate_runtime_model_requirements(runtime)?;
    let config = build_runtime_llm_config(runtime)?;
    let thinking = match runtime.thinking.as_deref() {
        Some(spec) => echo_agent::llm::ThinkingConfig::parse_spec(spec)?,
        None => None,
    };
    let client: Arc<dyn LlmClient> = config
        .build_client()
        .map(Arc::from)
        .map_err(|error| format!("Failed to create client: {error}"))?;
    Ok(PreparedRuntimeLlm {
        config,
        client,
        thinking,
    })
}

/// Prepare one live agent and every model-coupled runtime consumer without
/// changing any published state.
pub(crate) async fn prepare_agent_model_publication(
    handle: &AgentHandle,
    consumers: AgentModelConsumers,
    runtime: &model_config::ModelRuntimeConfig,
    prepared: &PreparedRuntimeLlm,
    token_limit: usize,
) -> Result<PreparedAgentModelPublication, String> {
    let (critic_owner, cache_user_id) = handle
        .read(|agent| {
            (
                agent.critic_owner().map(str::to_string),
                agent.config().get_cache_user_id().map(str::to_string),
            )
        })
        .await;
    let critic = if critic_owner.as_deref() == Some(EKO_MODEL_CRITIC_OWNER) {
        let mut critic = echo_agent::agent::critic::LlmCritic::new(prepared.client.clone())
            .with_pass_threshold(7.0);
        if let Some(cache_user_id) = cache_user_id {
            critic = critic.with_cache_user_id(cache_user_id);
        }
        echo_agent::agent::PreparedCriticUpdate::ReplaceOwned {
            owner: EKO_MODEL_CRITIC_OWNER.to_string(),
            critic: Arc::new(critic),
        }
    } else {
        echo_agent::agent::PreparedCriticUpdate::Preserve
    };
    let generation = handle
        .prepare_model_generation(
            prepared.config.clone(),
            prepared.client.clone(),
            runtime.temperature,
            runtime.max_tokens.or(Some(DEFAULT_MAX_TOKENS)),
            prepared.thinking.clone(),
            token_limit,
            critic,
        )
        .await
        .map_err(|error| format!("Failed to prepare agent model generation: {error}"))?;
    let mut inherited_publications = Vec::with_capacity(consumers.inherited_factories.len());
    for inherited in consumers.inherited_factories.iter() {
        let inherited_generation = inherited
            .handle
            .prepare_model_generation(
                prepared.config.clone(),
                prepared.client.clone(),
                runtime.temperature,
                runtime.max_tokens.or(Some(DEFAULT_MAX_TOKENS)),
                prepared.thinking.clone(),
                token_limit,
                echo_agent::agent::PreparedCriticUpdate::Preserve,
            )
            .await
            .map_err(|error| {
                format!(
                    "Failed to prepare inherited subagent '{}' model generation: {error}",
                    inherited.definition.name
                )
            })?;
        inherited_publications.push(PreparedInheritedSubagentPublication {
            generation: inherited_generation,
        });
    }
    Ok(PreparedAgentModelPublication {
        generation,
        inherited: inherited_publications,
        consumers,
        runtime: runtime.clone(),
        prepared: prepared.clone(),
        token_limit,
    })
}

#[cfg(test)]
mod llm_config_tests {
    use super::{
        AgentCreateParams, build_llm_config, build_runtime_llm_config,
        create_agent_with_diagnostics, prepare_runtime_llm,
    };
    use crate::model_config::ModelRuntimeConfig;
    use echo_agent::agent::Agent;
    use echo_agent::config::{AppConfig, ConfiguredModel, ModelProviderConfig};
    use echo_agent::llm::LlmApiProtocol;

    #[test]
    fn builtin_protocol_defaults_come_from_framework_config() {
        for (provider, expected) in [
            ("openai", LlmApiProtocol::Responses),
            ("anthropic", LlmApiProtocol::Anthropic),
            ("deepseek", LlmApiProtocol::ChatCompletions),
        ] {
            assert_eq!(
                build_llm_config(provider, "test-key", "test-model", None, None).api_protocol,
                expected,
                "unexpected builder protocol for {provider}"
            );
        }
    }

    #[test]
    fn custom_endpoint_inference_preserves_explicit_override_priority() {
        let endpoint = "https://gateway.example/v1/responses";
        assert_eq!(
            build_llm_config("custom", "test-key", "test-model", Some(endpoint), None).api_protocol,
            LlmApiProtocol::Responses
        );
        assert_eq!(
            build_llm_config(
                "custom",
                "test-key",
                "test-model",
                Some(endpoint),
                Some(LlmApiProtocol::ChatCompletions),
            )
            .api_protocol,
            LlmApiProtocol::ChatCompletions
        );
    }

    #[test]
    fn named_provider_constructors_infer_complete_endpoint_overrides() {
        for (provider, endpoint, expected) in [
            (
                "anthropic",
                "https://gateway.example/v1/responses",
                LlmApiProtocol::Responses,
            ),
            (
                "deepseek",
                "https://gateway.example/v1/messages",
                LlmApiProtocol::Anthropic,
            ),
            (
                "dashscope",
                "https://gateway.example/v1/responses",
                LlmApiProtocol::Responses,
            ),
        ] {
            assert_eq!(
                build_llm_config(provider, "test-key", "test-model", Some(endpoint), None,)
                    .api_protocol,
                expected,
                "unexpected endpoint override for {provider}"
            );
        }
    }

    #[test]
    fn runtime_builder_injects_local_endpoint_without_api_key() -> Result<(), String> {
        let runtime = ModelRuntimeConfig {
            id: "local:model".to_string(),
            display_name: "Local model".to_string(),
            provider: "local".to_string(),
            model: "model".to_string(),
            api_protocol: LlmApiProtocol::ChatCompletions,
            api_protocol_explicit: false,
            auth_token: None,
            auth_source: "none".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking: None,
        };

        let config = build_runtime_llm_config(&runtime)?;
        assert!(config.api_key.is_empty());
        assert_eq!(
            config.base_url,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(config.api_protocol, LlmApiProtocol::ChatCompletions);
        Ok(())
    }

    #[test]
    fn runtime_preflight_rejects_invalid_authorization_header() {
        let runtime = ModelRuntimeConfig {
            id: "openai:gpt-test".to_string(),
            display_name: "Invalid token".to_string(),
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            api_protocol: LlmApiProtocol::Responses,
            api_protocol_explicit: true,
            auth_token: Some("invalid\nheader".to_string()),
            auth_source: "input".to_string(),
            base_url: Some("https://api.openai.com/v1/responses".to_string()),
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking: None,
        };

        assert!(prepare_runtime_llm(&runtime).is_err());
    }

    fn selectable_model_config(agent_token_limit: usize) -> Result<AppConfig, String> {
        let mut config = AppConfig::default();
        config.agent.token_limit = agent_token_limit;
        config.model_providers.insert(
            "local".to_string(),
            ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            },
        );
        config.configured_models = vec![
            ConfiguredModel {
                id: "local:default".to_string(),
                display_name: "Default".to_string(),
                provider: "local".to_string(),
                model: "default-runtime".to_string(),
                api_protocol: Some(LlmApiProtocol::ChatCompletions),
                context_window: Some(16_384),
                ..ConfiguredModel::default()
            },
            ConfiguredModel {
                id: "local:selected".to_string(),
                display_name: "Selected".to_string(),
                provider: "local".to_string(),
                model: "selected-runtime".to_string(),
                api_protocol: Some(LlmApiProtocol::ChatCompletions),
                context_window: Some(65_536),
                ..ConfiguredModel::default()
            },
        ];
        crate::model_config::set_default_model(&mut config, "local:default")?;
        Ok(config)
    }

    #[tokio::test]
    async fn default_and_cli_selected_models_use_their_effective_context_window()
    -> Result<(), String> {
        let config = selectable_model_config(0)?;
        let default = create_agent_with_diagnostics(&AgentCreateParams::default(), &config).await?;
        let selected = create_agent_with_diagnostics(
            &AgentCreateParams {
                model: Some("local:selected".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;

        assert_eq!(default.agent.model_name(), "default-runtime");
        assert_eq!(default.agent.config().get_token_limit(), 16_384);
        assert_eq!(
            default
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            16_384
        );
        assert_eq!(selected.agent.model_name(), "selected-runtime");
        assert_eq!(selected.agent.config().get_token_limit(), 65_536);
        assert_eq!(
            selected
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            65_536
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_agent_token_limit_overrides_cli_selected_model_window() -> Result<(), String>
    {
        let config = selectable_model_config(7_777)?;
        let selected = create_agent_with_diagnostics(
            &AgentCreateParams {
                model: Some("local:selected".to_string()),
                ..Default::default()
            },
            &config,
        )
        .await?;

        assert_eq!(selected.agent.config().get_token_limit(), 7_777);
        assert_eq!(
            selected
                .model_consumers
                .inherited_generation
                .read()
                .await
                .token_limit,
            7_777
        );
        Ok(())
    }
}

#[cfg(test)]
mod resolve_subagent_model_tests {
    use super::{
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS, SubagentRuntimeGeneration, TASK_MANAGEMENT_GUIDE,
        build_readonly_subagent_agent, build_writer_subagent_agent, configure_run_code_capability,
        resolve_subagent_model, resolved_max_tool_output_tokens, subagent_model_binding,
        tool_output_artifact_config,
    };
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::agent::subagent::{SubagentPromptCompiler, SubagentRegistry};
    use echo_agent::sandbox::SandboxManager;
    use std::sync::Arc;

    fn test_command_cells()
    -> echo_agent::error::Result<Arc<dyn echo_agent::tools::cell::CommandCellRegistry>> {
        crate::tasks::task_runtime::command_cells::shared_command_cells(Arc::new(
            SandboxManager::local_sandbox(),
        ))
        .map_err(echo_agent::error::ReactError::Other)
    }

    #[test]
    fn stable_task_guide_stays_within_cache_budget() {
        assert!(TASK_MANAGEMENT_GUIDE.chars().count() <= 2_400);
    }

    #[test]
    fn tool_output_budget_uses_eko_default_when_unset() {
        assert_eq!(
            resolved_max_tool_output_tokens(0),
            DEFAULT_MAX_TOOL_OUTPUT_TOKENS
        );
        assert_eq!(resolved_max_tool_output_tokens(4_000), 4_000);
    }

    #[test]
    fn workspace_tool_output_artifacts_stay_inside_workspace_state() -> anyhow::Result<()> {
        let workspace = tempfile::tempdir()?;
        let config = tool_output_artifact_config(Some(workspace.path()));

        assert_eq!(config.root_dir, workspace.path().join(".eko/artifacts"));
        assert!(config.root_dir.starts_with(workspace.path()));
        assert_eq!(config.threshold_bytes, 32 * 1024);
        assert_eq!(config.max_age_secs, Some(30 * 24 * 60 * 60));
        Ok(())
    }

    #[test]
    fn none_inherits_parent() {
        assert_eq!(resolve_subagent_model(None, "parent-model"), "parent-model");
    }

    #[tokio::test]
    async fn explicit_inherit_is_resolved_once_and_remains_fixed() {
        let initial = SubagentRuntimeGeneration {
            model: "parent-a".to_string(),
            llm_config: None,
            llm_client: None,
            temperature: None,
            max_tokens: None,
            token_limit: 16_384,
            thinking: None,
        };
        let authority = Arc::new(tokio::sync::RwLock::new(initial.clone()));
        let binding = subagent_model_binding(Some("inherit"), None, &initial, &authority);
        *authority.write().await = SubagentRuntimeGeneration {
            model: "parent-b".to_string(),
            token_limit: 65_536,
            ..initial
        };

        let fixed = binding.snapshot().await;
        assert_eq!(fixed.model, "parent-a");
        assert_eq!(fixed.token_limit, 16_384);
    }

    #[tokio::test]
    async fn fixed_binding_parses_role_thinking_once() {
        // Explicit model + explicit thinking → the Fixed generation carries the
        // parsed spec; parent hot-swaps no longer affect it.
        let parent = SubagentRuntimeGeneration {
            model: "parent-a".to_string(),
            llm_config: None,
            llm_client: None,
            temperature: None,
            max_tokens: None,
            token_limit: 16_384,
            thinking: Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::High,
            )),
        };
        let authority = Arc::new(tokio::sync::RwLock::new(parent.clone()));
        let binding = subagent_model_binding(Some("fast"), Some("low"), &parent, &authority);
        let fixed = binding.snapshot().await;
        assert_eq!(
            fixed.thinking,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );

        // Explicit model but NO thinking spec → the registration-time parent
        // thinking is copied (mirrors temperature/max_tokens).
        let no_spec = subagent_model_binding(Some("fast"), None, &parent, &authority);
        assert_eq!(no_spec.snapshot().await.thinking, parent.thinking);

        // No model spec → Inherit binding follows the parent generation's
        // thinking, so the published parent value flows into forks.
        let inherit = subagent_model_binding(None, None, &parent, &authority);
        assert_eq!(inherit.snapshot().await.thinking, parent.thinking);
    }

    #[test]
    fn fast_falls_back_to_parent_without_env() {
        // Do not assert env-dependent path; only the no-env fallback.
        let got = resolve_subagent_model(Some("fast"), "parent-model");
        // If EKO_FAST_MODEL is set in the environment, honor it; otherwise parent.
        if let Ok(fast) = std::env::var("EKO_FAST_MODEL") {
            let trimmed = fast.trim();
            if !trimmed.is_empty() {
                assert_eq!(got, trimmed);
                return;
            }
        }
        assert_eq!(got, "parent-model");
    }

    #[test]
    fn concrete_model_passthrough() {
        assert_eq!(
            resolve_subagent_model(Some("claude-haiku"), "parent"),
            "claude-haiku"
        );
    }

    #[test]
    fn unavailable_os_sandbox_removes_run_code() -> echo_agent::error::Result<()> {
        let mut agent = ReactAgentBuilder::new()
            .model("test-model")
            .enable_tools()
            .build()?;
        assert!(agent.list_tools().iter().any(|name| name == "run_code"));

        configure_run_code_capability(&mut agent, false);
        assert!(!agent.list_tools().iter().any(|name| name == "run_code"));
        Ok(())
    }

    #[test]
    fn writer_subagent_inherits_sandbox_manager() -> echo_agent::error::Result<()> {
        let sandbox = Arc::new(SandboxManager::local_sandbox());
        let subagent = build_writer_subagent_agent(
            "writer",
            "write files",
            "test-model",
            None,
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler)
                as Arc<dyn SubagentPromptCompiler>,
            Arc::new(SubagentRegistry::new()),
            None,
            sandbox,
            test_command_cells()?,
            true,
        )?;

        assert!(subagent.sandbox_manager().is_some());
        assert!(!subagent.is_plan_mode());
        for tool in ["write_file", "edit_file", "shell"] {
            assert!(
                subagent.list_tools().iter().any(|name| name == tool),
                "writer subagent must expose {tool}"
            );
        }
        assert!(subagent.list_tools().iter().any(|name| name == "run_code"));
        assert!(
            !subagent
                .list_tools()
                .iter()
                .any(|name| name == "agent_tool")
        );
        Ok(())
    }

    #[test]
    fn delegation_capability_registers_agent_tool() -> echo_agent::error::Result<()> {
        let subagent = build_writer_subagent_agent(
            "delegator",
            "delegate bounded work",
            "test-model",
            None,
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            1,
            true,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            Arc::new(SandboxManager::local_sandbox()),
            test_command_cells()?,
            true,
        )?;

        assert!(
            subagent
                .list_tools()
                .iter()
                .any(|name| name == "agent_tool")
        );
        Ok(())
    }

    #[test]
    fn readonly_subagent_applies_per_subagent_thinking() -> echo_agent::error::Result<()> {
        // build_readonly_subagent_agent must install the resolved thinking on
        // the built agent (ReactAgent::thinking getter) — this is the wire the
        // awaiter's `thinking: low` frontmatter rides on.
        let low = echo_agent::llm::ThinkingConfig::Level(echo_agent::llm::ThinkingLevel::Low);
        let subagent = build_readonly_subagent_agent(
            "awaiter",
            "wait for one background cell",
            "test-model",
            None,
            None,
            None,
            None,
            8_192,
            Some(low.clone()),
            30_000,
            1_024,
            "test-cache-user",
            64,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            test_command_cells()?,
        )?;
        assert_eq!(subagent.thinking(), Some(&low));

        // None → the agent keeps "use the model default" (no thinking field).
        let plain = build_readonly_subagent_agent(
            "explorer",
            "explore",
            "test-model",
            None,
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            0,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            test_command_cells()?,
        )?;
        assert!(plain.thinking().is_none());
        Ok(())
    }

    #[test]
    fn readonly_and_writer_subagents_share_cell_tools() -> echo_agent::error::Result<()> {
        // C2b:两个构建路径都注入进程级共享 cell registry——readonly 子智能体
        // (如 awaiter)没有 shell,但必须拥有 wait/stop_cell/list_cells。
        let readonly = build_readonly_subagent_agent(
            "awaiter",
            "wait for one background cell",
            "test-model",
            None,
            None,
            None,
            None,
            8_192,
            None,
            30_000,
            1_024,
            "test-cache-user",
            64,
            false,
            Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler),
            Arc::new(SubagentRegistry::new()),
            None,
            test_command_cells()?,
        )?;
        let readonly_names = readonly.tool_names();
        for expected in ["wait", "stop_cell", "list_cells"] {
            assert!(
                readonly_names.contains(&expected.to_string()),
                "readonly subagent missing cell tool {expected}: {readonly_names:?}"
            );
        }
        assert!(
            !readonly_names.contains(&"shell".to_string()),
            "readonly subagent must not gain shell: {readonly_names:?}"
        );
        Ok(())
    }

    #[test]
    fn loader_thinking_specs_parse_through_binding_path() {
        // The builtin awaiter declares `thinking: low`; the same parse_spec the
        // binding uses must accept it, and the loader spec string must flow
        // unchanged from the .md frontmatter.
        let defs = crate::subagent_loader::discover_subagents(None, None);
        let awaiter = defs
            .iter()
            .find(|d| d.name == "awaiter")
            .expect("builtin awaiter.md must load");
        let parsed = echo_agent::llm::ThinkingConfig::parse_spec(
            awaiter.thinking.as_deref().unwrap_or_default(),
        )
        .expect("awaiter thinking spec must parse");
        assert_eq!(
            parsed,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );
    }
}
