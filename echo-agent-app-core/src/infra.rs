//! 基础设施函数
//!
//! 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::agent::subagent::{
    AgentFactory, FnAgentFactory, SubagentBuilder, SubagentPromptCompiler,
    SubagentSystemPromptInput,
};
use echo_agent::llm::LlmConfig;
use echo_agent::memory::ConversationStore;
use echo_agent::prelude::*;
use echo_agent::state::RuntimeStateStore;
use futures::future::BoxFuture;

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::model_config;
use crate::project::prompt::PromptAssembler;

/// Default context window size in tokens (396K).
const DEFAULT_CONTEXT_WINDOW: usize = 396_000;

/// Default max output tokens when not configured (sensible for 128K context models).
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// EKO product default for one tool result returned to the model. The generic
/// framework keeps 0 as opt-out so other consumers choose their own budget.
const DEFAULT_MAX_TOOL_OUTPUT_TOKENS: usize = 8_000;
const TOOL_OUTPUT_ARTIFACT_THRESHOLD_BYTES: usize = 32 * 1024;
const TOOL_OUTPUT_ARTIFACT_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;
fn resolved_max_tool_output_tokens(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS
    }
}

/// Product-owned storage policy for complete oversized tool output.
///
/// Artifacts use one stable global root so worktree/workspace removal cannot
/// invalidate a running session's complete logs. Conversation deletion removes
/// its scope immediately, while the 30-day max age prevents abandoned scopes
/// from growing without bound.
pub fn tool_output_artifact_config(
    _working_dir: Option<&std::path::Path>,
) -> echo_agent::tools::artifact::ToolOutputArtifactConfig {
    let root_dir = echo_agent::paths::user_data_path("artifacts").join("tool-logs");
    echo_agent::tools::artifact::ToolOutputArtifactConfig::new(root_dir, "conversation_or_30d")
        .threshold_bytes(TOOL_OUTPUT_ARTIFACT_THRESHOLD_BYTES)
        .max_age_secs(Some(TOOL_OUTPUT_ARTIFACT_MAX_AGE_SECS))
}

/// Guide appended to the system prompt when task management tools are
/// available. Instructs the agent to actively manage its task plan and
/// proactively dispatch readonly subagents for investigation-heavy work
/// (对齐 Claude Code 的 subagent:轻量派发是工具,正式并行是 runtime).
pub(crate) const TASK_MANAGEMENT_GUIDE: &str = r#"

## Task And Delegation Tools

Choose the lightest reliable mechanism:
- Direct work: simple questions, narrow edits, short tool sequences.
- `agent_tool`: one bounded Chat subtask. Fresh, no TaskRuntime entry; fork only for required history.
- `plan_create` + `task_list` + `plan_execute({plan_revision: N})`: the required path for any delegated work in Auto or Task mode, and for dependencies, parallel work, writers, or verification.
- `create_complex_task`: a long-lived Run for cross-turn or substantial orchestration.

### Formal Plan Contract
- Use the user's language for task titles, descriptions, and Subagent briefs; preserve technical identifiers. Give each task a concrete outcome, kind, role, targets, dependencies, and verification.
- Verification splits into `execution_checks` (shell commands requiring observed pass, e.g. `cargo test`) and `acceptance_criteria` (semantic statements a reviewer judges against the output). Never declare acceptance passed yourself.
- A completed Subagent is not a completed PlanTask. Tasks Blocked on acceptance pause the run for an explicit retry, never auto-redispatch.
- The TaskRun already represents the user goal. Do not create a wrapper, placeholder, or prose-only summary task for that goal; materialize only work a Subagent will actually execute.
- One `plan_create` atomically creates the complete initial DAG. Give every task a stable ID, submit all dependencies in that call, then pass the returned revision to `plan_execute`.
- Read-only tasks may run in parallel. Writers must declare owned files or artifacts.
- Keep plans truthful with `plan_patch` and `task_list`. A patch must include the latest `base_revision`; only the runtime marks completion.
- Do not claim dispatch before `plan_execute` accepts the full plan.
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
    /// Shared runtime state store (sqlite-backed). When supplied, the agent
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
    /// task-management tools (plan_create/update/complete/skip/list) so the
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
    // Resolve the product-level configured model first. The legacy `model`
    // section is only a persisted mirror/fallback; GUI/CLI/TUI should all
    // converge on configured_models for actual runtime wiring.
    let runtime_model = model_config::resolve_runtime_model(
        app_config,
        app_config.model.default_model_id.as_deref(),
    );
    let model = params.model.as_deref().unwrap_or(&runtime_model.model);
    let temperature = runtime_model.temperature.or(app_config.model.temperature);
    let max_tokens = runtime_model.max_tokens.or(app_config.model.max_tokens);

    let base_system_prompt = params
        .system_prompt
        .as_deref()
        .unwrap_or(&app_config.agent.system_prompt);

    // Use PromptAssembler for modular, budget-aware prompt construction
    let model_window = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        DEFAULT_CONTEXT_WINDOW
    };
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
    let token_limit = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        DEFAULT_CONTEXT_WINDOW
    };
    let max_tool_output_tokens =
        resolved_max_tool_output_tokens(app_config.agent.max_tool_output_tokens);
    let sandbox_manager = Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox());
    let subagent_prompt_compiler: Arc<dyn SubagentPromptCompiler> =
        Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler);
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
        // EKO owns planning through TaskRuntime. The framework's optional
        // background-task tools use a separate store and must not be exposed
        // alongside plan_create/plan_execute.
        .enable_subagent()
        .subagent_prompt_compiler(subagent_prompt_compiler.clone())
        .register_agent_dispatch_tool() // Phase 0: ad-hoc agent_tool alongside plan_execute
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
    let mut injected_llm_config: Option<LlmConfig> = None;
    if let Some(auth_token) = runtime_model.auth_token.as_deref() {
        let provider = runtime_model.provider.as_str();
        let base_url_override = runtime_model.base_url.as_deref();
        let llm_config = build_llm_config(provider, auth_token, model, base_url_override);
        tracing::info!(
            provider = provider,
            model = model,
            auth_source = %runtime_model.auth_source,
            has_base_url = base_url_override.is_some(),
            "Injecting LlmConfig from configured model"
        );
        injected_llm_config = Some(llm_config.clone());
        builder = builder.llm_config(llm_config);
    }

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
    refresh_dynamic_context(&mut agent, subagent_project_root.as_deref()).await;
    configure_run_code_capability(&mut agent, run_code_available);
    agent.set_pre_model_context_projector(Some(std::sync::Arc::new(
        crate::tasks::task_runtime::compact_context::TaskRuntimeContextProjector::new(
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
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
    // LlmCritic uses llm::chat → Config::get_model, the same config source
    // as the main agent, so it reuses the already-configured model.
    // Fail-open on errors (verify.rs:91-93) ensures the main flow is never
    // blocked if the critic LLM call fails.
    agent.set_critic(std::sync::Arc::new(
        echo_agent::agent::critic::LlmCritic::new(model)
            .with_pass_threshold(7.0)
            .with_cache_user_id(cache_user_id.clone()),
    ));
    agent.config_mut().set_verifier_enabled(true);
    tracing::info!("main agent: Critic self-verification enabled (threshold=7.0, max_retries=2)");

    tracing::info!(
        has_llm_config = injected_llm_config.is_some(),
        model = %model,
        "main agent: registering default subagents with llm_config={}",
        injected_llm_config.as_ref().map(|c| c.model.as_str()).unwrap_or("NONE")
    );
    register_default_subagents(
        &mut agent,
        model,
        injected_llm_config,
        temperature,
        max_tokens,
        token_limit,
        app_config.agent.tool_timeout_ms,
        max_tool_output_tokens,
        &cache_user_id,
        &discovered_subagents,
        subagent_prompt_compiler.clone(),
        params.browser_runtime.clone(),
        sandbox_manager,
        run_code_available,
    )
    .await;

    // Register default hooks
    register_default_hooks(&mut agent);

    // Register task-management tools when a TaskRuntimeStore is available.
    // These let the main Agent atomically create, revise, and inspect plans.
    // The store handle is threaded from AppState → SharedResources → params.
    if let Some(store) = &params.task_runtime_store {
        use crate::tasks::task_runtime::task_tools::{
            PlanCapabilityCatalog, PlanPatchTool, TaskCreateTool, TaskListTool,
        };
        let store = Arc::clone(store);
        let tool_names = agent.tool_names();
        let capabilities = Arc::new(PlanCapabilityCatalog::new(
            subagent_catalog_snapshot.clone(),
            tool_names,
        ));
        agent.add_tool(Box::new(TaskCreateTool {
            store: Arc::clone(&store),
            capabilities: capabilities.clone(),
        }));
        agent.add_tool(Box::new(PlanPatchTool {
            store: Arc::clone(&store),
            capabilities,
        }));
        agent.add_tool(Box::new(TaskListTool {
            store: Arc::clone(&store),
        }));
        tracing::info!(
            "Registered revisioned task-management tools (plan_create/plan_patch/task_list)"
        );
    }

    Ok(CreatedAgent {
        agent,
        prompt_assembly,
    })
}

/// Refresh every workspace-dependent context projection on an agent.
pub async fn refresh_dynamic_context(agent: &mut ReactAgent, root: Option<&std::path::Path>) {
    crate::unified_memory::refresh_instruction_projection(agent, root).await;
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
/// - `"fast"` → `EKO_FAST_MODEL` env if set, else parent model
/// - any other string → used as-is
pub fn resolve_subagent_model(spec: Option<&str>, parent_model: &str) -> String {
    match spec {
        None => parent_model.to_string(),
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
    model: &str,
    llm_config: Option<LlmConfig>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    subagents: &[crate::subagent_loader::SubagentDefinition],
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    run_code_available: bool,
) {
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
        tags: Vec<String>,
    }

    let mut built_subagents: Vec<BuiltSubagent> = Vec::with_capacity(subagents.len());
    for subagent_def in subagents {
        // Sprint 9: register BOTH readonly and writer subagents. Readonly subagents
        // get the readonly tool subset (physical no-write enforcement); writer
        // subagents get the full tool set (shell/file/git) and run inside an
        // isolated git worktree when `isolate_worktree` is set (Sprint 8 wiring).
        // TaskRuntime may run disjoint exact owners concurrently; every writer
        // still gets a separate checkout and a reviewed integration boundary.
        let subagent_model = resolve_subagent_model(subagent_def.model.as_deref(), model);
        let subagent_llm = llm_config.clone().map(|mut cfg| {
            cfg.model = subagent_model.clone();
            cfg
        });
        let max_iterations = subagent_def.max_turns.unwrap_or(0);
        let isolation = crate::subagent_loader::subagent_isolation(subagent_def);
        let compiled_system = prompt_compiler.compile_system(&SubagentSystemPromptInput {
            name: &subagent_def.name,
            description: &subagent_def.description,
            role_prompt: &subagent_def.system_prompt,
            readonly: subagent_def.readonly,
            can_delegate: false,
            isolation,
        });
        let build_result = if subagent_def.readonly {
            build_readonly_subagent_agent(
                &subagent_def.name,
                &compiled_system.system_prompt,
                &subagent_model,
                subagent_llm.clone(),
                temperature,
                max_tokens,
                token_limit,
                tool_timeout_ms,
                max_tool_output_tokens,
                cache_user_id,
                max_iterations,
                browser_runtime.clone(),
            )
        } else {
            build_writer_subagent_agent(
                &subagent_def.name,
                &compiled_system.system_prompt,
                &subagent_model,
                subagent_llm.clone(),
                temperature,
                max_tokens,
                token_limit,
                tool_timeout_ms,
                max_tool_output_tokens,
                cache_user_id,
                max_iterations,
                browser_runtime.clone(),
                sandbox_manager.clone(),
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
                if subagent_def.is_background {
                    builder = builder.background().tag("background");
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
                let factory_model = subagent_model.clone();
                let factory_llm = subagent_llm.clone();
                let factory_cache_user_id = cache_user_id.to_string();
                let factory_browser_runtime = browser_runtime.clone();
                let factory_sandbox_manager = sandbox_manager.clone();
                let factory_tool_output_artifacts = tool_output_artifacts.clone();
                let factory_system_prompt = compiled_system.system_prompt.clone();
                let fork_factory = Arc::new(FnAgentFactory::new(
                    move || -> BoxFuture<'static, echo_agent::error::Result<Box<dyn Agent>>> {
                        let subagent_def = factory_def.clone();
                        let model = factory_model.clone();
                        let llm = factory_llm.clone();
                        let cache_user_id = factory_cache_user_id.clone();
                        let browser_runtime = factory_browser_runtime.clone();
                        let sandbox_manager = factory_sandbox_manager.clone();
                        let tool_output_artifacts = factory_tool_output_artifacts.clone();
                        let system_prompt = factory_system_prompt.clone();
                        Box::pin(async move {
                            let max_iterations = subagent_def.max_turns.unwrap_or(0);
                            let subagent = if subagent_def.readonly {
                                build_readonly_subagent_agent(
                                    &subagent_def.name,
                                    &system_prompt,
                                    &model,
                                    llm,
                                    temperature,
                                    max_tokens,
                                    token_limit,
                                    tool_timeout_ms,
                                    max_tool_output_tokens,
                                    &cache_user_id,
                                    max_iterations,
                                    browser_runtime,
                                )?
                            } else {
                                build_writer_subagent_agent(
                                    &subagent_def.name,
                                    &system_prompt,
                                    &model,
                                    llm,
                                    temperature,
                                    max_tokens,
                                    token_limit,
                                    tool_timeout_ms,
                                    max_tool_output_tokens,
                                    &cache_user_id,
                                    max_iterations,
                                    browser_runtime,
                                    sandbox_manager,
                                    run_code_available,
                                )?
                            };
                            subagent.set_tool_output_artifacts(tool_output_artifacts);
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
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    max_iterations: usize,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
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
        .sandbox_manager(sandbox_manager);

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
    subagent.set_plan_mode(true);
    Ok(subagent)
}

#[allow(clippy::too_many_arguments)]
fn build_readonly_subagent_agent(
    name: &str,
    prompt: &str,
    model: &str,
    llm_config: Option<LlmConfig>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    max_iterations: usize,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
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
        });

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
    subagent.set_plan_mode(true);
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
    let default_paths = [echo_agent::paths::user_data_path("mcp.json")];

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
) {
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
                    match run_dreaming_pass(&review_integration).await {
                        Ok(report) => {
                            tracing::info!(
                                scanned = report.scanned,
                                promoted = report.promoted,
                                revived = report.revived,
                                demoted = report.demoted,
                                "Dreaming pass completed"
                            );
                            if report.promoted > 0 {
                                let root = primary_agent
                                    .read(|agent| agent.working_dir())
                                    .await;
                                primary_agent
                                    .write_async(|agent| {
                                        Box::pin(async move {
                                            crate::unified_memory::refresh_instruction_projection(
                                                agent,
                                                root.as_deref(),
                                            )
                                            .await;
                                        })
                                    })
                                    .await;
                                if let Some(ref agent_pool) = pool {
                                    agent_pool.refresh_instruction_context().await;
                                }
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "Dreaming pass failed"),
                    }
                }
            }
        }
    });
}

async fn run_dreaming_pass(
    review_integration: &crate::evolution::ReviewIntegration,
) -> anyhow::Result<echo_agent::evolution::DreamingReport> {
    let layer_manager = std::sync::Arc::new(review_integration.create_layer_manager());
    let dreaming = echo_agent::evolution::Dreaming::new(
        layer_manager,
        echo_agent::evolution::DreamingConfig::default(),
    );
    dreaming.run().await.map_err(anyhow::Error::from)
}

/// 创建对话持久化 Store（文件），失败时返回 None（禁用持久化）
pub fn create_conversation_store() -> Option<Arc<dyn ConversationStore>> {
    let base = echo_agent::paths::user_data_dir();

    match crate::conversation_file::FileConversationStore::new(&base) {
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
    match crate::runtime_state_file::FileRuntimeStateStore::new(&base_dir) {
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
    // Only relevant on macOS where GUI apps miss shell env vars.
    // On Linux, desktop environments generally inherit the shell env.
    #[cfg(not(target_os = "macos"))]
    {
        return;
    }

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
            // (plan_execute/execute_run/drain loop) are visible by default.
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

fn provider_required_keys(provider: &str) -> &'static [&'static str] {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "dashscope" | "qwen" | "aliyun" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "moonshot" | "kimi" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "zhipu" | "glm" => &["ZHIPU_API_KEY", "GLM_API_KEY"],
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
) -> LlmConfig {
    let default_base_url = echo_agent::llm::config::provider_base_url(provider)
        .unwrap_or("https://api.openai.com/v1/chat/completions");
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
    // Ensure provider_name is set so the thinking-protocol resolver picks the
    // right wire field (e.g. enable_thinking for dashscope vs reasoning_effort
    // for deepseek). The named constructors already set it; the generic
    // fallback (`LlmConfig::new`) does not, so set it here uniformly.
    if config.provider_name.is_none() && !provider.trim().is_empty() {
        config.provider_name = Some(provider.to_string());
    }
    config
}

#[cfg(test)]
mod resolve_subagent_model_tests {
    use super::{
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS, TASK_MANAGEMENT_GUIDE, build_writer_subagent_agent,
        configure_run_code_capability, resolve_subagent_model, resolved_max_tool_output_tokens,
    };
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::sandbox::SandboxManager;
    use std::sync::Arc;

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
    fn none_inherits_parent() {
        assert_eq!(resolve_subagent_model(None, "parent-model"), "parent-model");
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
            8_192,
            30_000,
            1_024,
            "test-cache-user",
            1,
            None,
            sandbox,
            true,
        )?;

        assert!(subagent.sandbox_manager().is_some());
        assert!(subagent.list_tools().iter().any(|name| name == "run_code"));
        assert!(
            !subagent
                .list_tools()
                .iter()
                .any(|name| name == "agent_tool")
        );
        Ok(())
    }
}
