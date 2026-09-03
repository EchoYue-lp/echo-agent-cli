// 基础设施函数
//
// 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::subagent::{
    AgentFactory, FnAgentFactory, SubagentAccessMode, SubagentBuilder,
    SubagentExecutionBoundary, SubagentPromptCompiler, SubagentSystemPromptInput,
    ToolCapabilitySnapshot,
};
use echo_agent::llm::{LlmApiProtocol, LlmClient, LlmConfig};
use echo_agent::memory::ConversationStore;
use echo_agent::prelude::*;
use echo_agent::state::RuntimeStateStore;
use futures::future::BoxFuture;

use crate::agent_handle::AgentHandle;
use crate::config::EkoConfig;
use crate::model_config;
use crate::project::prompt::PromptAssembler;

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
const EKO_ACTIVE_TOOL_TRACE_TURNS: usize = 1;
const EKO_MIN_TOOL_TRACE_TOKENS: usize = 4_000;
const EKO_MAX_TOOL_TRACE_TOKENS: usize = 40_000;
const EKO_MAX_COMPACTION_SAVINGS_THRESHOLD: usize = 20_000;
const TOOL_OUTPUT_ARTIFACT_THRESHOLD_BYTES: usize = 32 * 1024;
const TOOL_OUTPUT_ARTIFACT_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;

fn eko_visibility_horizon(
    token_limit: usize,
) -> echo_agent::compression::horizon::VisibilityHorizonConfig {
    let retained_tool_tokens = token_limit
        .saturating_mul(25)
        .saturating_div(100)
        .clamp(EKO_MIN_TOOL_TRACE_TOKENS, EKO_MAX_TOOL_TRACE_TOKENS);
    let compact_minimum_tokens = retained_tool_tokens.saturating_div(2).clamp(
        EKO_MIN_TOOL_TRACE_TOKENS,
        EKO_MAX_COMPACTION_SAVINGS_THRESHOLD,
    );
    echo_agent::compression::horizon::VisibilityHorizonConfig {
        active_window_turns: EKO_ACTIVE_TOOL_TRACE_TURNS,
        retained_tool_tokens: Some(retained_tool_tokens),
        compact_minimum_tokens,
        ..Default::default()
    }
}

fn resolved_max_tool_output_tokens(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MAX_TOOL_OUTPUT_TOKENS
    }
}

/// Convert EKO's `0` iteration sentinel into the positive framework value that
/// represents an effectively unlimited ReAct loop. The framework builder
/// rejects zero, while EKO configuration intentionally documents zero as
/// "until completion or cancellation".
pub(crate) fn resolved_max_iterations(configured: usize) -> usize {
    if configured == 0 {
        usize::MAX
    } else {
        configured
    }
}

/// Resolve the one context budget used by prompt assembly, the parent agent,
/// and every subagent that inherits the selected runtime model.
///
/// Product priority is explicit agent override, selected model context window,
/// then EKO's documented fallback.
pub fn effective_token_limit(
    app_config: &EkoConfig,
    runtime: Option<&model_config::ModelRuntimeConfig>,
) -> usize {
    if app_config.agent.token_limit > 0 {
        return app_config.agent.token_limit;
    }
    runtime
        .and_then(|runtime| runtime.context_window)
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
    Fixed(Box<SubagentRuntimeGeneration>),
}

impl SubagentModelBinding {
    async fn snapshot(&self) -> SubagentRuntimeGeneration {
        match self {
            Self::Inherit(generation) => generation.read().await.clone(),
            Self::Fixed(generation) => generation.as_ref().clone(),
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
    app_config: &EkoConfig,
    parent_generation: &SubagentRuntimeGeneration,
    inherited_generation: &Arc<tokio::sync::RwLock<SubagentRuntimeGeneration>>,
) -> SubagentModelBinding {
    if spec.is_none() {
        // Inherit: no thinking of its own — snapshots follow the shared
        // generation the parent republishes on every model hot-swap.
        SubagentModelBinding::Inherit(inherited_generation.clone())
    } else {
        let mut generation = resolve_fixed_subagent_generation(spec, app_config, parent_generation);
        // Fixed: resolve the role's thinking spec once; without one keep the
        // parent's thinking as of registration (mirrors temperature/max_tokens).
        generation.thinking = subagent_build_thinking(thinking_spec, &generation);
        SubagentModelBinding::Fixed(Box::new(generation))
    }
}

fn resolve_fixed_subagent_generation(
    spec: Option<&str>,
    app_config: &EkoConfig,
    parent_generation: &SubagentRuntimeGeneration,
) -> SubagentRuntimeGeneration {
    let selector = match spec.map(str::trim).filter(|value| !value.is_empty()) {
        Some("inherit") | None => return parent_generation.clone(),
        Some("fast") => match std::env::var("EKO_FAST_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(selector) => selector,
            None => return parent_generation.clone(),
        },
        Some(selector) => selector.to_string(),
    };
    let runtime = match model_config::resolve_runtime_model(app_config, Some(&selector)) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(
                selector,
                %error,
                "Configured Subagent model selector is unavailable; using the complete parent generation"
            );
            return parent_generation.clone();
        }
    };
    let prepared = match prepare_runtime_llm(&runtime) {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(
                selector,
                %error,
                "Configured Subagent model profile could not be prepared; using the complete parent generation"
            );
            return parent_generation.clone();
        }
    };
    SubagentRuntimeGeneration {
        model: runtime.model.clone(),
        llm_config: Some(prepared.config),
        llm_client: Some(prepared.client),
        temperature: runtime.temperature,
        max_tokens: runtime.max_tokens,
        token_limit: effective_token_limit(app_config, Some(&runtime)),
        thinking: prepared.thinking,
    }
}

#[derive(Clone)]
struct InheritedSubagentFactory {
    definition: echo_agent::subagent::SubagentDefinition,
    handle: AgentHandle,
    factory: Arc<dyn AgentFactory>,
}

#[derive(Clone)]
struct SubagentPromptPublication {
    definition: crate::subagent_loader::SubagentDefinition,
    handle: AgentHandle,
    compiler: Arc<dyn SubagentPromptCompiler>,
}

/// EKO-owned model consumers attached to one parent agent generation.
///
/// Only subagent definitions with omitted `model` frontmatter are included.
/// Explicit frontmatter values are resolved once and remain fixed.
#[derive(Clone)]
pub struct AgentModelConsumers {
    subagent_catalog: Arc<crate::subagent_loader::SubagentCatalogSnapshot>,
    inherited_generation: Arc<tokio::sync::RwLock<SubagentRuntimeGeneration>>,
    registry: Arc<echo_agent::subagent::SubagentRegistry>,
    inherited_factories: Arc<Vec<InheritedSubagentFactory>>,
    prompt_publications: Arc<Vec<SubagentPromptPublication>>,
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    disabled_tools_projection: Arc<tokio::sync::RwLock<Option<std::collections::HashSet<String>>>>,
}

impl AgentModelConsumers {
    pub(crate) fn subagent_catalog(
        &self,
    ) -> Arc<crate::subagent_loader::SubagentCatalogSnapshot> {
        self.subagent_catalog.clone()
    }

    pub(crate) async fn apply_disabled_tools(
        &self,
        disabled_tools: Option<std::collections::HashSet<String>>,
    ) {
        *self.disabled_tools_projection.write().await = disabled_tools.clone();
        for publication in self.prompt_publications.iter() {
            let disabled_tools = disabled_tools.clone();
            let definition = publication.definition.clone();
            let compiler = publication.compiler.clone();
            publication
                .handle
                .write(move |agent| {
                    agent.set_disabled_tools(disabled_tools.clone());
                    let disabled = disabled_tools.unwrap_or_default();
                    let capabilities = ToolCapabilitySnapshot::from_definitions(
                        &agent.tool_definitions(),
                        &disabled,
                    );
                    let compiled = compiler.compile_system(&SubagentSystemPromptInput {
                        actor: echo_agent::subagent::PromptActor::Subagent,
                        name: &definition.name,
                        description: &definition.description,
                        role_prompt: &definition.system_prompt,
                        capabilities: &capabilities,
                        boundary: SubagentExecutionBoundary {
                            access: if definition.readonly {
                                SubagentAccessMode::ReadOnly
                            } else {
                                SubagentAccessMode::Write
                            },
                            isolation: crate::subagent_loader::subagent_isolation(&definition),
                            can_delegate: definition.can_delegate,
                        },
                    });
                    agent.replace_system_prompt(compiled.system_prompt);
                })
                .await;
        }
        for definition in self.registry.list_available().await {
            if !matches!(
                &definition.kind,
                echo_agent::subagent::SubagentKind::Plugin { .. }
            ) {
                continue;
            }
            let Some(agent) = self.registry.get_agent(&definition.name).await else {
                continue;
            };
            let capabilities = ToolCapabilitySnapshot::from_definitions(
                &agent.tool_definitions(),
                &agent.disabled_tool_names(),
            );
            let role_prompt = definition.system_prompt.as_deref().unwrap_or_default();
            let compiled = self
                .prompt_compiler
                .compile_system(&SubagentSystemPromptInput {
                    actor: echo_agent::subagent::PromptActor::Subagent,
                    name: &definition.name,
                    description: &definition.description,
                    role_prompt,
                    capabilities: &capabilities,
                    boundary: SubagentExecutionBoundary {
                        access: definition.access_mode,
                        isolation: definition.isolation.as_deref().unwrap_or("context"),
                        can_delegate: definition.can_delegate,
                    },
                });
            agent.set_system_prompt(&compiled.system_prompt);
        }
    }

    #[cfg(test)]
    pub(crate) async fn tool_control_is_projected_for_test(&self, name: &str) -> bool {
        let projection_contains = self
            .disabled_tools_projection
            .read()
            .await
            .as_ref()
            .is_some_and(|disabled| disabled.contains(name));
        if !projection_contains || self.prompt_publications.is_empty() {
            return false;
        }
        for publication in self.prompt_publications.iter() {
            if !crate::tool_control::snapshot_disabled_tools(&publication.handle)
                .await
                .contains(name)
            {
                return false;
            }
        }
        true
    }

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

    async fn clear_inherited_generation(&self) {
        let mut generation = self.inherited_generation.write().await;
        generation.model.clear();
        generation.llm_config = None;
        generation.llm_client = None;
        generation.temperature = None;
        generation.max_tokens = None;
        generation.thinking = None;
    }

    /// Republish the parent's reasoning-depth knob.
    ///
    /// Inherit-binding Subagents have no `thinking` of their own: update the
    /// shared generation (read by every future fork build) and the live
    /// registered handles. Fixed bindings keep their registration-time value.
    pub(crate) async fn apply_thinking(&self, thinking: Option<echo_agent::llm::ThinkingConfig>) {
        self.inherited_generation.write().await.thinking = thinking.clone();
        for inherited in self.inherited_factories.iter() {
            let thinking = thinking.clone();
            let _updated = inherited.handle.try_write(|agent| {
                    if agent.thinking() != thinking.as_ref() {
                        agent.set_thinking(thinking);
                    }
                });
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
        .unwrap_or_else(|| crate::data_root::user_data_path("artifacts"));
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
    pub command_cell_runtime:
        Option<Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>>,
    /// Application-generation owner for every blocking product-data phase
    /// reachable from this Agent and its tools.
    pub product_data_io: Option<crate::product_data_io::ProductDataIoService>,
    pub execution_scope: Option<crate::workspace::WorkspaceExecutionScope>,
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
    let path = crate::data_root::user_data_path("cache_user_id");

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
    pub runtime_model: Option<model_config::ModelRuntimeConfig>,
    pub command_cell_runtime:
        Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
}

/// Create an Agent instance without retaining build diagnostics.
pub async fn create_agent(
    params: &AgentCreateParams,
    app_config: &EkoConfig,
) -> std::result::Result<ReactAgent, String> {
    create_agent_with_diagnostics(params, app_config)
        .await
        .map(|created| created.agent)
}

/// Create an agent and retain the application-owned prompt assembly report.
pub async fn create_agent_with_diagnostics(
    params: &AgentCreateParams,
    app_config: &EkoConfig,
) -> std::result::Result<CreatedAgent, String> {
    // EKO can boot before the user configures a provider. An explicit selector
    // must still resolve, while an absent selector leaves the Agent detached
    // from LLM transport until the first model mutation is published.
    let runtime_model =
        match model_config::resolve_runtime_model(app_config, params.model.as_deref()) {
            Ok(runtime) => {
                model_config::validate_runtime_model_requirements(&runtime)?;
                Some(runtime)
            }
            Err(model_config::ModelSelectionError::NotConfigured) if params.model.is_none() => None,
            Err(error) => return Err(error.to_string()),
        };
    let model = runtime_model
        .as_ref()
        .map(|runtime| runtime.model.as_str())
        .unwrap_or_default();
    let temperature = runtime_model
        .as_ref()
        .and_then(|runtime| runtime.temperature);
    let max_tokens = runtime_model
        .as_ref()
        .and_then(|runtime| runtime.max_tokens);

    let base_system_prompt = params
        .system_prompt
        .as_deref()
        .unwrap_or(&app_config.agent.system_prompt);

    // Use PromptAssembler for modular, budget-aware prompt construction
    let model_window = effective_token_limit(app_config, runtime_model.as_ref());
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

    // Determine config values from EkoConfig
    let token_limit = effective_token_limit(app_config, runtime_model.as_ref());
    let max_tool_output_tokens =
        resolved_max_tool_output_tokens(app_config.agent.max_tool_output_tokens);
    let sandbox_manager = Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox());
    let product_data_io = match params.product_data_io.clone() {
        Some(product_data_io) => product_data_io,
        #[cfg(test)]
        None => crate::product_data_io::ProductDataIoService::new(),
        #[cfg(not(test))]
        None => {
            return Err(
                "Agent creation requires an application-owned product-data I/O service".to_string(),
            );
        }
    };
    let script_execution_profile_resolver: Arc<
        dyn echo_agent::tools::ScriptExecutionProfileResolver,
    > = Arc::new(
        crate::analysis_runtime::AnalyticsRuntime::with_product_data_io(product_data_io.clone()),
    );
    let command_cell_runtime = match params.command_cell_runtime.clone() {
        Some(runtime) => runtime,
        None => crate::tasks::task_runtime::command_cells::CommandCellRuntimeService::new(
            sandbox_manager.clone(),
            Arc::new(crate::chat_event_log::ChatEventLog::at_default_root()),
            product_data_io,
        )?,
    };
    let execution_scope = params.execution_scope.clone().unwrap_or_else(|| {
        crate::workspace::WorkspaceExecutionScope::global(
            params
                .working_dir
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        )
    });
    let command_cells =
        command_cell_runtime.scoped(execution_scope.clone(), params.task_runtime_store.clone());
    let subagent_prompt_compiler: Arc<dyn SubagentPromptCompiler> =
        Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler);
    let subagent_registry = Arc::new(echo_agent::subagent::SubagentRegistry::new());
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
        .max_iterations(resolved_max_iterations(app_config.agent.max_iterations))
        .token_limit(token_limit)
        .visibility_horizon(eko_visibility_horizon(token_limit))
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
    let prepared_llm = runtime_model
        .as_ref()
        .map(prepare_runtime_llm)
        .transpose()?;
    let injected_llm_config = prepared_llm
        .as_ref()
        .map(|prepared| prepared.config.clone());
    if let (Some(runtime), Some(llm_config)) = (runtime_model.as_ref(), injected_llm_config.clone())
    {
        tracing::info!(
            provider = %runtime.provider,
            model = %runtime.model,
            auth_source = %runtime.auth_source,
            has_base_url = runtime.base_url.is_some(),
            "Injecting LlmConfig from configured model"
        );
        builder = builder.llm_config(llm_config);
    } else {
        tracing::info!("Starting without an LLM; waiting for the first configured model");
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
    // The same store handle is threaded into every default Subagent below:
    // Subagents must share the workspace memory instead of each opening the
    // framework default at an empty `memory_path` (which always fails its
    // authority lease and logs "authority path has no parent" per Subagent).
    let memory_workspace_root = params.working_dir.as_deref().map(std::path::Path::new);
    #[cfg(not(test))]
    let (memory_store_path, _) = resolve_memory_store_paths(memory_workspace_root);
    #[cfg(not(test))]
    let subagent_memory_store = create_memory_store_at(&memory_store_path);
    // Tests without an explicit workspace need no durable side effect. Tests
    // that exercise workspace persistence pass a TempDir-backed working_dir
    // and continue to use the real FileStore path inside that owned scope.
    #[cfg(test)]
    let subagent_memory_store: Option<Arc<dyn echo_agent::memory::Store>> =
        match memory_workspace_root {
            Some(root) => {
                let (memory_store_path, _) = resolve_memory_store_paths(Some(root));
                create_memory_store_at(&memory_store_path)
            }
            None => Some(Arc::new(echo_agent::memory::InMemoryStore::new())),
        };
    if let Some(store) = subagent_memory_store.clone() {
        builder = builder.store(store);
    }

    // Set React loop checkpoint interval if provided (used by background tasks
    // to enable crash recovery — saves conversation history every N iterations)
    if let Some(interval) = params.react_checkpoint_interval
        && interval > 0
    {
        builder = builder.react_checkpoint_interval(interval);
    }

    // Initialize JSONL run store for production trace persistence. Unit tests
    // that need a RunStore inject one explicitly; ordinary Agent-construction
    // tests must not create durable files outside their fixture.
    #[cfg(not(test))]
    {
        let run_dir = crate::data_root::user_data_path("runs");
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

    // EKO owns the concrete worktree/workspace policies behind one framework
    // isolation boundary. A requested kind that cannot be established fails
    // rather than silently sharing the primary workspace.
    let repo_root = subagent_project_root
        .as_ref()
        .and_then(|root| crate::tasks::task_runtime::worktree::git_repo_root(root).ok());
    let isolation_provider: std::sync::Arc<dyn echo_agent::subagent::IsolationProvider> =
        std::sync::Arc::new(
            crate::tasks::task_runtime::worktree::EkoIsolationProvider::new(repo_root),
        );
    let builder = builder.subagent_isolation_provider(isolation_provider);

    // Reuse one canonical checkpoint store for ReAct and compiled team task
    // graphs. A caller-provided store is authoritative; otherwise EKO installs
    // its durable file-backed default. Without a conversation id the framework
    // has no checkpoint key, so the default store remains inert. Tests only
    // use a caller-provided state store and otherwise stay in memory.
    #[cfg(not(test))]
    let state_store = params
        .state_store
        .clone()
        .or_else(create_runtime_state_store);
    #[cfg(test)]
    let state_store = params.state_store.clone();
    let builder = if let Some(state_store) = state_store {
        builder.state_store(state_store)
    } else {
        builder
    };

    let mut agent = builder.build().map_err(|e| {
        tracing::error!("Failed to build agent: {e}");
        format!("Failed to initialize agent: {e}. Please check your configuration and try again.")
    })?;
    // The built parent is the memory authority. This also preserves memory in
    // the fallback path where the workspace FileStore could not be opened and
    // the framework installed its configured default store instead.
    let subagent_memory_store = agent.store().cloned();
    install_eko_shell_policy(&mut agent, sandbox_manager.clone(), command_cells.clone());
    agent
        .tool_manager()
        .apply_script_execution_profile_resolver(script_execution_profile_resolver.clone());
    // `prepare_runtime_llm` already constructed this exact client before any
    // runtime state was accepted. Install it explicitly so agent bootstrap can
    // never fall back to an environment/YAML client after a swallowed rebuild.
    if let Some(prepared) = prepared_llm.as_ref() {
        agent.set_llm_client(prepared.client.clone());
    }
    refresh_dynamic_context(&mut agent, subagent_project_root.as_deref()).await;
    configure_run_code_capability(&mut agent, run_code_available);
    agent.set_pre_model_context_projector(Some(std::sync::Arc::new(
        crate::turn_context::EkoContextProjector::new(
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
            crate::turn_context::turn_prompt_context_registry(),
        )
        .with_command_cell_watches(command_cell_runtime.clone(), execution_scope.clone()),
    )));
    let cache_user_id = load_or_create_cache_user_id();
    agent.config_mut().set_cache_user_id(&cache_user_id);

    if let Some(browser_runtime) = &params.browser_runtime {
        browser_runtime
            .register_workspace_root(
                execution_scope.workspace_id().to_string(),
                execution_scope.root().to_path_buf(),
            )
            .await;
        browser_runtime.install_tools(&mut agent);
    }

    // Inject LlmCritic for self-verification. The critic scores the agent's
    // final_answer; if below threshold (7.0), feedback is injected and the
    // agent retries (up to verifier_max_retries=2).
    // The critic consumes the exact prepared transport used by the parent;
    // it never re-resolves global config or assumes Chat Completions.
    // Fail-open on errors (verify.rs:91-93) ensures the main flow is never
    // blocked if the critic LLM call fails.
    if let Some(prepared) = prepared_llm.as_ref() {
        agent.set_owned_critic(
            EKO_MODEL_CRITIC_OWNER,
            std::sync::Arc::new(
                echo_agent::agent::critic::LlmCritic::new(prepared.client.clone())
                    .with_pass_threshold(7.0)
                    .with_cache_user_id(cache_user_id.clone()),
            ),
        );
        agent.config_mut().set_verifier_enabled(true);
        tracing::info!(
            "main agent: Critic self-verification enabled (threshold=7.0, max_retries=2)"
        );
    }

    tracing::info!(
        has_llm_config = injected_llm_config.is_some(),
        model = %model,
        "main agent: registering default subagents with llm_config={}",
        injected_llm_config.as_ref().map(|c| c.model.as_str()).unwrap_or("NONE")
    );
    let model_consumers = register_default_subagents(
        &mut agent,
        app_config,
        SubagentRuntimeGeneration {
            model: model.to_string(),
            llm_config: injected_llm_config,
            llm_client: prepared_llm
                .as_ref()
                .map(|prepared| prepared.client.clone()),
            temperature,
            max_tokens,
            token_limit,
            thinking: prepared_llm
                .as_ref()
                .and_then(|prepared| prepared.thinking.clone()),
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
        script_execution_profile_resolver,
        run_code_available,
        subagent_memory_store,
    )
    .await;
    crate::tasks::task_runtime::command_cells::install_watch_cell_tool(
        &mut agent,
        command_cells,
        command_cell_runtime.clone(),
        execution_scope,
    );

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
            crate::tasks::task_runtime::build_task_revision_service(store, capabilities);
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
        command_cell_runtime,
    })
}

/// Refresh every workspace-dependent context projection on an agent.
pub async fn refresh_dynamic_context(agent: &mut ReactAgent, root: Option<&std::path::Path>) {
    match crate::unified_memory::load_instruction_projection_strict(root) {
        Ok(snapshot) => {
            crate::unified_memory::apply_instruction_projection_snapshot(agent, &snapshot).await;
        }
        Err(error) => {
            tracing::warn!(%error, "failed to refresh instruction context projection");
        }
    }
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
    app_config: &EkoConfig,
    parent_generation: SubagentRuntimeGeneration,
    tool_timeout_ms: u64,
    max_tool_output_tokens: usize,
    cache_user_id: &str,
    subagents: &[crate::subagent_loader::SubagentDefinition],
    prompt_compiler: Arc<dyn SubagentPromptCompiler>,
    subagent_registry: Arc<echo_agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
    script_execution_profile_resolver: Arc<dyn echo_agent::tools::ScriptExecutionProfileResolver>,
    run_code_available: bool,
    memory_store: Option<Arc<dyn echo_agent::memory::Store>>,
) -> AgentModelConsumers {
    let tool_output_artifacts = agent.tool_output_artifacts();
    tracing::info!(
        count = subagents.len(),
        names = ?subagents.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
        "Loaded subagent definitions from .md (project/user/builtin)"
    );

    struct BuiltSubagent {
        definition: echo_agent::subagent::SubagentDefinition,
        prompt_definition: crate::subagent_loader::SubagentDefinition,
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
    let disabled_tools_projection = Arc::new(tokio::sync::RwLock::new(None));
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
            app_config,
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
        let build_result = if subagent_def.readonly {
            build_readonly_subagent_agent(
                &subagent_def.name,
                &subagent_def.system_prompt,
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
                memory_store.clone(),
            )
        } else {
            build_writer_subagent_agent(
                &subagent_def.name,
                &subagent_def.system_prompt,
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
                script_execution_profile_resolver.clone(),
                run_code_available,
                memory_store.clone(),
            )
        };
        match build_result {
            Ok(mut subagent) => {
                let disabled_tools: std::collections::HashSet<String> = disabled_tools_projection
                    .read()
                    .await
                    .clone()
                    .unwrap_or_default();
                subagent.set_disabled_tools((!disabled_tools.is_empty()).then_some(disabled_tools.clone()));
                let capabilities = ToolCapabilitySnapshot::from_definitions(
                    &subagent.tool_definitions(),
                    &disabled_tools,
                );
                let compiled_system = prompt_compiler.compile_system(&SubagentSystemPromptInput {
                    actor: echo_agent::subagent::PromptActor::Subagent,
                    name: &subagent_def.name,
                    description: &subagent_def.description,
                    role_prompt: &subagent_def.system_prompt,
                    capabilities: &capabilities,
                    boundary: SubagentExecutionBoundary {
                        access: if subagent_def.readonly {
                            SubagentAccessMode::ReadOnly
                        } else {
                            SubagentAccessMode::Write
                        },
                        isolation,
                        can_delegate: subagent_def.can_delegate,
                    },
                });
                subagent.replace_system_prompt(compiled_system.system_prompt);
                subagent.set_tool_output_artifacts(tool_output_artifacts.clone());
                crate::tasks::task_runtime::compact_context::install_task_context_protection(
                    &subagent,
                )
                .await;
                let subagent_handle = crate::agent_handle::AgentHandle::new(subagent);

                let mut builder = SubagentBuilder::new(&subagent_def.name)
                    .description(&subagent_def.description)
                    .fork_mode();
                if subagent_def.readonly {
                    builder = builder.read_only();
                }
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
                    builder = builder.isolation("worktree");
                }
                // Sprint 10: honor the frontmatter `workspace: true` flag for
                // data/research subagents (per-subagent tmpdir, disjoint outputs).
                // Loader clears it when worktree is active (mutually exclusive).
                if subagent_def.isolate_workspace {
                    builder = builder.isolation("workspace");
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
                // Optional per-Subagent execution timeout. None keeps the
                // framework default.
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
                let factory_script_execution_profile_resolver =
                    script_execution_profile_resolver.clone();
                let factory_tool_output_artifacts = tool_output_artifacts.clone();
                let factory_prompt_compiler = prompt_compiler.clone();
                let factory_subagent_registry = subagent_registry.clone();
                let factory_memory_store = memory_store.clone();
                let factory_disabled_tools = Arc::clone(&disabled_tools_projection);
                let fork_factory = Arc::new(FnAgentFactory::new(
                    move || -> BoxFuture<'static, echo_agent::error::Result<Box<dyn Agent>>> {
                        let subagent_def = factory_def.clone();
                        let model_binding = factory_model_binding.clone();
                        let cache_user_id = factory_cache_user_id.clone();
                        let browser_runtime = factory_browser_runtime.clone();
                        let sandbox_manager = factory_sandbox_manager.clone();
                        let command_cells = factory_command_cells.clone();
                        let script_execution_profile_resolver =
                            factory_script_execution_profile_resolver.clone();
                        let tool_output_artifacts = factory_tool_output_artifacts.clone();
                        let prompt_compiler = factory_prompt_compiler.clone();
                        let subagent_registry = factory_subagent_registry.clone();
                        let memory_store = factory_memory_store.clone();
                        let disabled_tools = Arc::clone(&factory_disabled_tools);
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
                            let mut subagent = if subagent_def.readonly {
                                build_readonly_subagent_agent(
                                    &subagent_def.name,
                                    &subagent_def.system_prompt,
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
                                    prompt_compiler.clone(),
                                    subagent_registry,
                                    browser_runtime,
                                    command_cells,
                                    memory_store,
                                )?
                            } else {
                                build_writer_subagent_agent(
                                    &subagent_def.name,
                                    &subagent_def.system_prompt,
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
                                    prompt_compiler.clone(),
                                    subagent_registry,
                                    browser_runtime,
                                    sandbox_manager,
                                    command_cells,
                                    script_execution_profile_resolver,
                                    run_code_available,
                                    memory_store,
                                )?
                            };
                            let disabled_tools: std::collections::HashSet<String> = disabled_tools
                                .read()
                                .await
                                .clone()
                                .unwrap_or_default();
                            subagent.set_disabled_tools(
                                (!disabled_tools.is_empty()).then_some(disabled_tools.clone()),
                            );
                            let capabilities = ToolCapabilitySnapshot::from_definitions(
                                &subagent.tool_definitions(),
                                &disabled_tools,
                            );
                            let compiled_system = prompt_compiler.compile_system(
                                &SubagentSystemPromptInput {
                                    actor: echo_agent::subagent::PromptActor::Subagent,
                                    name: &subagent_def.name,
                                    description: &subagent_def.description,
                                    role_prompt: &subagent_def.system_prompt,
                                    capabilities: &capabilities,
                                    boundary: SubagentExecutionBoundary {
                                        access: if subagent_def.readonly {
                                            SubagentAccessMode::ReadOnly
                                        } else {
                                            SubagentAccessMode::Write
                                        },
                                        isolation: crate::subagent_loader::subagent_isolation(
                                            &subagent_def,
                                        ),
                                        can_delegate: subagent_def.can_delegate,
                                    },
                                },
                            );
                            subagent.replace_system_prompt(compiled_system.system_prompt);
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
                    prompt_definition: subagent_def.clone(),
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
    let prompt_publications = built_subagents
        .iter()
        .map(|built| SubagentPromptPublication {
            definition: built.prompt_definition.clone(),
            handle: built.handle.clone(),
            compiler: prompt_compiler.clone(),
        })
        .collect();
    AgentModelConsumers {
        subagent_catalog: Arc::new(
            crate::subagent_loader::SubagentCatalogSnapshot::from_definitions(subagents),
        ),
        inherited_generation,
        registry: subagent_registry,
        inherited_factories: Arc::new(inherited_factories),
        prompt_publications: Arc::new(prompt_publications),
        prompt_compiler,
        disabled_tools_projection,
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
    subagent_registry: Arc<echo_agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
    script_execution_profile_resolver: Arc<dyn echo_agent::tools::ScriptExecutionProfileResolver>,
    run_code_available: bool,
    memory_store: Option<Arc<dyn echo_agent::memory::Store>>,
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
        .register_subagent_message_tools()
        .enable_cot()
        .token_limit(token_limit)
        .visibility_horizon(eko_visibility_horizon(token_limit))
        .max_tool_output_tokens(max_tool_output_tokens)
        .max_tokens(max_tokens.or(Some(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: tool_timeout_ms,
            ..Default::default()
        })
        .sandbox_manager(sandbox_manager.clone())
        // 与主智能体共享同一进程级 registry；cell 自身通过同一个 sandbox
        // executor 执行，不会从前台策略静默降级为宿主机直连。
        .command_cells(command_cells.clone());

    // Share the parent's workspace memory store. Without one the Subagent
    // runs without memory tools — the framework default would open an empty
    // `memory_path` that always fails its authority lease.
    if let Some(store) = memory_store {
        builder = builder.store(store);
    }

    if max_iterations > 0 {
        builder = builder.max_iterations(max_iterations);
    }

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
        // No model configured is a supported first-run state; the parent
        // already emits the single "Starting without an LLM" INFO. Keep this
        // per-subagent note at debug so N subagents don't spam WARN for it.
        tracing::debug!(
            subagent_name = name,
            "writer subagent: no LlmConfig — will follow the first configured model"
        );
    }

    let mut subagent = builder.build()?;
    install_eko_shell_policy(&mut subagent, sandbox_manager, command_cells);
    subagent
        .tool_manager()
        .apply_script_execution_profile_resolver(script_execution_profile_resolver);
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

fn install_eko_shell_policy(
    agent: &mut ReactAgent,
    sandbox_manager: Arc<echo_agent::sandbox::SandboxManager>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
) {
    // EKO is a local personal assistant. The application PermissionService is
    // the authority for automatic tool policy; retaining the framework's
    // generic fixed whitelist here would reject harmless full-auto commands
    // after permission had already been granted. The framework default stays
    // strict for other embedders, while EKO still keeps the shell tool's
    // dangerous blocklist and explicit-approval classifications.
    agent.replace_tool(Box::new(
        echo_agent::tools::shell::ShellTool::new()
            .with_command_policy(Arc::new(crate::permission::EkoCommandPolicy))
            .with_sandbox(sandbox_manager)
            .with_cell_launcher(command_cells),
    ));
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
    subagent_registry: Arc<echo_agent::subagent::SubagentRegistry>,
    browser_runtime: Option<Arc<crate::browser::BrowserRuntime>>,
    command_cells: Arc<dyn echo_agent::tools::cell::CommandCellRegistry>,
    memory_store: Option<Arc<dyn echo_agent::memory::Store>>,
) -> std::result::Result<ReactAgent, echo_agent::error::ReactError> {
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(name)
        .system_prompt(prompt)
        .enable_tools()
        .readonly_tools() // SA-2: physical enforcement — no shell/write tools
        // In-tree communication tools: report/escalate to the run driver and
        // queue-only sibling messaging (uplink read from ToolContext).
        .register_subagent_message_tools()
        .enable_cot()
        .token_limit(token_limit)
        .visibility_horizon(eko_visibility_horizon(token_limit))
        .max_tool_output_tokens(max_tool_output_tokens)
        .max_tokens(max_tokens.or(Some(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: tool_timeout_ms,
            ..Default::default()
        })
        // Read-only Subagents cannot launch shell work, but may inspect command
        // cells already owned by their invocation scope.
        .command_cells(command_cells);

    // Share the parent's workspace memory store. Without one the Subagent
    // runs without memory tools — the framework default would open an empty
    // `memory_path` that always fails its authority lease.
    if let Some(store) = memory_store {
        builder = builder.store(store);
    }

    if max_iterations > 0 {
        builder = builder.max_iterations(max_iterations);
    }

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
        // No model configured is a supported first-run state; see the writer
        // builder note — keep the per-subagent line at debug.
        tracing::debug!(
            subagent_name = name,
            "subagent: no LlmConfig — will follow the first configured model"
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
