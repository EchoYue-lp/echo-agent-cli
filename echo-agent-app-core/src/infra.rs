//! 基础设施函数
//!
//! 提供 Agent 创建、MCP 加载、对话持久化、关闭信号等共享工具。

use std::sync::Arc;

use echo_agent::agent::subagent::SubagentBuilder;
use echo_agent::llm::LlmConfig;
use echo_agent::memory::ConversationStore;
use echo_agent::memory::SqliteConversationStore;
use echo_agent::prelude::*;
use echo_agent::state::{RuntimeStateStore, SqliteRuntimeStateStore};

use crate::agent_handle::AgentHandle;
use crate::config::AppConfig;
use crate::model_config;
use crate::project::prompt::PromptAssembler;
use crate::tasks::task_runtime::TaskRouteKind;

/// Default context window size in tokens (128K).
const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Default max output tokens when not configured (sensible for 128K context models).
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Guide appended to the system prompt when task management tools are
/// available. Instructs the agent to actively manage its task plan.
const TASK_MANAGEMENT_GUIDE: &str = r#"

## Task Management
You have tools to manage your task plan: task_create, task_update, task_complete, task_skip, task_list.
- When you discover additional work is needed, use task_create to add it.
- When a task is no longer relevant (e.g., you found it's unnecessary), use task_skip.
- When you complete a task, use task_complete to mark it done.
- Use task_list to review current plan state.
Update your plan frequently as your understanding deepens.

## 复杂任务编排(主 agent 职责)
当用户任务涉及多步调研/分析/实现时,你是编排者:
1. **拆分计划**:用 task_create 把任务拆成可独立执行的子任务(每个有清晰 title/description/kind),用 depends_on 表达依赖(并行任务无依赖,串行任务 A depends_on B)。title 不能为空。
2. **统一执行**:拆完计划后,调 execute_plan 把整个计划交给并行执行引擎。引擎(run_dag)按依赖自动并行调度多个 worker,并收集它们的产出摘要。**不要自己逐个 delegate_readonly 派 worker**——那样会串行执行且丢失 token 统计。
3. **收口**:execute_plan 返回结果(含各 worker 产出摘要)后,基于结果写最终答案给用户。
4. **写任务**:如果子任务需要修改文件(implementation/debugging/verification),在 task_create 时用对应 kind,execute_plan 会安排主 agent 自己执行(不经 worker)。

你是长生命周期的主 agent:跨多个对话 turn 保持上下文。用户可能中途插话改计划——用 task_update/task_complete/task_skip 调整。

## 重要:delegate_readonly 工具
delegate_readonly 是给 **worker 内部** 委派子任务用的(L3 嵌套),**主 agent 不要直接使用 delegate_readonly 派 worker**。如果你已经有 plan(调过 task_create),直接调 execute_plan 即可。如果你在 plan 之外用了 delegate_readonly,系统会拒绝并提示你改用 execute_plan。
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
    /// Pre-computed instruction/profile context to inject into the system prompt
    /// (for example, user/project/local EKO instruction files). Dynamic
    /// long-term memories are recalled per turn through the agent memory store,
    /// not baked into this boot-time suffix.
    pub memory_context_suffix: Option<String>,
    /// Session-bound working directory (worktree path). Propagated to
    /// `ReactAgent.config.working_dir`, which `ExecuteStage` injects into every
    /// tool call's `ToolContext` — so shell/file/git tools run inside the
    /// isolated checkout. None = use process cwd (backward compatible).
    pub working_dir: Option<std::path::PathBuf>,
    /// TaskRuntime store handle. When supplied, `create_agent` registers the
    /// task-management tools (task_create/update/complete/skip/list) so the
    /// main agent can autonomously manage its plan during execution.
    pub task_runtime_store: Option<Arc<crate::tasks::task_runtime::TaskRuntimeStore>>,
    /// Route kind for execute_plan tool registration. When Some, the
    /// `execute_plan` tool is registered on the agent (never on workers,
    /// per §10.2). The route determines whether ComplexRuntime approval
    /// gating is active (§10.5).
    pub route: Option<TaskRouteKind>,
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
/// This id is shared by the primary agent and built-in worker agents so repeated
/// project prompts land in the same provider cache partition across sessions.
pub fn load_or_create_cache_user_id() -> String {
    let path = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".echo-agent")
            .join("cache_user_id")
    };

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
/// 创建 Agent 实例
///
/// Uses `ReactAgentBuilder` to construct the agent. The framework is mode-agnostic;
/// domain specialization is handled by the Skill system (SKILL.md files).
///
/// # Errors
///
/// Returns `Err` if the agent builder fails (e.g. missing required config like
/// an API key or an invalid model name). Callers should surface this to the user
/// rather than crashing.
pub async fn create_agent(
    params: &AgentCreateParams,
    app_config: &AppConfig,
) -> std::result::Result<ReactAgent, String> {
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
    let model_window = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        DEFAULT_CONTEXT_WINDOW
    };
    let mut assembler =
        PromptAssembler::default(base_system_prompt, project_ctx.as_ref(), model_window);
    // Inject the unified instruction/profile context so the agent's system prompt
    // reflects EKO user/project/local instruction files. Dynamic long-term
    // memories stay query-dependent and are recalled per turn through the Store.
    if let Some(ref memory_suffix) = params.memory_context_suffix {
        assembler.add_memory_context(memory_suffix);
    }
    let mut system_prompt = assembler.assemble();

    // Inject task management guide when the agent has task tools available.
    system_prompt.push_str(TASK_MANAGEMENT_GUIDE);

    // Determine config values from AppConfig
    let token_limit = if app_config.agent.token_limit > 0 {
        app_config.agent.token_limit
    } else {
        DEFAULT_CONTEXT_WINDOW
    };

    // Use ReactAgentBuilder — mode is resolved at the CLI layer,
    // framework only receives model + system_prompt + tools.
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(&app_config.agent.name)
        .system_prompt(&system_prompt)
        .enable_tools()
        .enable_memory()
        .enable_planning()
        .enable_subagent()
        .enable_human_in_loop()
        .max_iterations(app_config.agent.max_iterations)
        .token_limit(token_limit)
        .max_tokens(Some(max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: app_config.agent.tool_timeout_ms,
            ..Default::default()
        })
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

    let mut agent = builder.build().map_err(|e| {
        tracing::error!("Failed to build agent: {e}");
        format!("Failed to initialize agent: {e}. Please check your configuration and try again.")
    })?;
    let cache_user_id = load_or_create_cache_user_id();
    agent.config_mut().set_cache_user_id(&cache_user_id);

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
        &cache_user_id,
    )
    .await;

    // Register default hooks
    register_default_hooks(&mut agent);

    // Register task-management tools when a TaskRuntimeStore is available.
    // These let the main agent autonomously create / update / complete / skip /
    // list tasks during execution (mirrors Claude Code's TaskCreate/Update).
    // The store handle is threaded from AppState → SharedResources → params.
    if let Some(store) = &params.task_runtime_store {
        use crate::tasks::task_runtime::task_tools::{
            TaskCompleteTool, TaskCreateTool, TaskListTool, TaskSkipTool, TaskUpdateTool,
        };
        let store = Arc::clone(store);
        agent.add_tool(Box::new(TaskCreateTool {
            store: Arc::clone(&store),
        }));
        agent.add_tool(Box::new(TaskUpdateTool {
            store: Arc::clone(&store),
        }));
        agent.add_tool(Box::new(TaskCompleteTool {
            store: Arc::clone(&store),
        }));
        agent.add_tool(Box::new(TaskSkipTool {
            store: Arc::clone(&store),
        }));
        agent.add_tool(Box::new(TaskListTool {
            store: Arc::clone(&store),
        }));
        tracing::info!(
            "Registered 5 task-management tools (task_create/update/complete/skip/list)"
        );
    }

    Ok(agent)
}

/// Readonly worker role definitions for subagent delegation (L2 and L3 nesting).
///
/// Each entry is (name, description, system_prompt). Used by both the main agent
/// (L2 delegation) and worker agents (L3 delegation via spec §3.3).
const WORKER_DEFINITIONS: &[(&str, &str, &str)] = &[
    (
        "project_explorer",
        "只读探索项目结构、配置、文档和相关代码，输出关键文件、事实发现和不确定点。",
        r#"你是 EKO 的只读项目探索 worker。

任务：快速建立项目地图，识别入口、模块边界、关键配置、文档、测试和运行方式。
边界：只读；不要修改文件；不要运行 shell；不要做最终综合结论。
方法：优先读取目录结构、README/配置/manifest、主入口、核心模块和测试布局；记录不确定点。
输出：按"项目定位 / 关键文件 / 架构结构 / 重要事实 / 不确定点"组织，文件引用使用 path:line。"#,
    ),
    (
        "code_reviewer",
        "只读审查指定模块的 bug、重复实现、架构问题、边界条件和测试缺口。",
        r#"你是 EKO 的只读代码审查 worker。

任务：寻找真实 bug、架构风险、重复机制、错误处理缺口、并发/状态/持久化边界问题和缺失测试。
边界：只读；不要修改文件；不要运行 shell；不要输出泛泛建议。
方法：从调用链和状态流入手，优先确认可复现风险；区分确定问题、可疑问题和设计建议。
输出：按严重程度排序；每条包含 path:line、风险、原因、建议验证方式。"#,
    ),
    (
        "test_planner",
        "只读规划应运行的检查、测试和 walkthrough，说明验证优先级和风险。",
        r#"你是 EKO 的只读验证规划 worker。

任务：设计最小但有力的验证方案，覆盖编译、单测、集成、GUI/TUI 行为、回归风险和手工 walkthrough。
边界：只读；不要修改文件；不要运行 shell；不要假设测试已经通过。
方法：根据改动面和风险决定验证层级；指出哪些检查是必须、哪些是可选、哪些受环境限制。
输出：按优先级列出验证步骤、命令/入口、预期信号、失败时下一步。"#,
    ),
    (
        "summary_writer",
        "汇总多个 worker 的发现，压缩成清晰结论、计划或交付说明。",
        r#"你是 EKO 的综合汇总 worker。

任务：合并多个 worker 的发现，去重、消解冲突、提炼结论、给出可执行下一步。
边界：只读；不要修改文件；不要运行 shell；不要发明 worker 没有提供的事实。
方法：优先保留有证据的发现；把推断和确定事实分开；指出剩余不确定性。
输出：先给综合结论，再给关键证据、风险排序、建议行动计划。"#,
    ),
    (
        "data_profiler",
        "只读检查数据来源、schema、缺失值、异常值、样本范围和字段含义。",
        r#"你是 EKO 的只读数据画像 worker。

任务：识别数据来源、schema、字段语义、样本范围、缺失/异常/重复、时间粒度和潜在偏差。
边界：只读；不要修改文件；不要运行 shell；不要给出超过数据支持的结论。
输出：按"数据来源 / 字段与范围 / 质量问题 / 分析风险 / 需要进一步确认"组织。"#,
    ),
    (
        "analysis_reviewer",
        "只读审查分析方法、指标定义、统计假设、图表表达和结论是否被数据支持。",
        r#"你是 EKO 的只读分析审查 worker。

任务：审查指标定义、分析方法、统计假设、图表表达、因果表述和结论是否被数据支持。
边界：只读；不要修改文件；不要运行 shell。
输出：列出被数据支持的结论、证据不足的结论、方法风险、建议复核或补充分析。"#,
    ),
    (
        "reproducibility_planner",
        "只读规划数据处理与分析任务的复现路径、检查步骤和交付物。",
        r#"你是 EKO 的只读可复现性规划 worker。

任务：规划从原始数据到结论的可复现路径，包括环境、输入、处理步骤、脚本/notebook、验证和报告产物。
边界：只读；不要修改文件；不要运行 shell。
输出：给出复现步骤、必须记录的参数、检查点、产物清单和失败排查顺序。"#,
    ),
    (
        "literature_scout",
        "只读探索学术资料、检索策略、候选论文、关键词和证据缺口。",
        r#"你是 EKO 的只读学术资料探索 worker。

任务：提出检索策略、关键词、数据库/来源、纳入排除思路、候选资料类型和证据缺口。
边界：只读；不要修改文件；不要运行 shell；不要编造论文、作者、DOI 或结论。
输出：检索策略、候选主题、优先阅读顺序、需要验证的引用信息。"#,
    ),
    (
        "evidence_reviewer",
        "只读审查证据质量、研究类型、引用可靠性、争议点和结论强度。",
        r#"你是 EKO 的只读证据审查 worker。

任务：评估研究类型、方法质量、样本/数据限制、引用可靠性、争议点和结论强度。
边界：只读；不要修改文件；不要运行 shell；不要让结论超过证据。
输出：按证据等级/可信度组织，标明支持、反对、不确定和需要进一步检索的点。"#,
    ),
    (
        "synthesis_planner",
        "只读规划综述、证据表、引用结构和最终研究交付物。",
        r#"你是 EKO 的只读研究综合规划 worker。

任务：规划综述/报告结构、证据表、论证链、引用组织、局限性和最终交付物。
边界：只读；不要修改文件；不要运行 shell。
输出：章节结构、证据矩阵字段、关键论点、局限性和写作/验证步骤。"#,
    ),
    (
        "medical_literature_scout",
        "只读探索医学资料、指南、系统综述、临床研究和检索策略。",
        r#"你是 EKO 的只读医学资料探索 worker。

任务：规划医学检索，优先指南、系统综述、随机对照研究和高质量真实世界研究；记录 PICO、关键词、来源和证据缺口。
边界：只读；不要修改文件；不要运行 shell；不要给诊断或治疗建议；不要编造 PMID/DOI/指南。
输出：检索策略、优先证据来源、需要核验的医学声明和安全不确定性。"#,
    ),
    (
        "clinical_evidence_reviewer",
        "只读审查医学证据等级、临床适用性、指南一致性和引用支撑。",
        r#"你是 EKO 的只读临床证据审查 worker。

任务：审查证据等级、研究设计、适用人群、结局指标、指南一致性、风险收益和引用支撑。
边界：只读；不要修改文件；不要运行 shell；不要给诊断或治疗建议。
输出：证据强度、适用边界、冲突证据、安全风险和需要临床专业确认的点。"#,
    ),
    (
        "safety_reviewer",
        "只读审查医学安全边界、免责声明、禁忌风险和是否存在过度医疗建议。",
        r#"你是 EKO 的只读安全审查 worker。

任务：检查医学/高风险内容是否越界，是否存在诊断治疗建议、禁忌遗漏、紧急风险、误导性确定表述或缺少专业就医边界。
边界：只读；不要修改文件；不要运行 shell。
输出：安全问题清单、建议改写方向、必须保留的不确定性和升级给专业人士的条件。"#,
    ),
];

/// Register readonly worker subagents on the given agent.
///
/// Builds and registers all 13 readonly subagent definitions and agents.
/// Used by the main agent for L2 delegation, and called on each worker
/// agent for L3 nesting (spec §3.3).
#[allow(clippy::too_many_arguments)]
async fn register_worker_subagents(
    agent: &mut ReactAgent,
    model: &str,
    llm_config: Option<LlmConfig>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    cache_user_id: &str,
) {
    for &(name, description, prompt) in WORKER_DEFINITIONS {
        match build_readonly_worker_agent(
            name,
            prompt,
            model,
            llm_config.clone(),
            temperature,
            max_tokens,
            token_limit,
            tool_timeout_ms,
            cache_user_id,
        ) {
            Ok(sub_worker) => {
                let def = SubagentBuilder::new(name)
                    .description(description)
                    .fork_mode()
                    .tag("readonly")
                    .tag("parallel")
                    .build();
                agent.register_subagent_with_definition(def, Box::new(sub_worker));
            }
            Err(err) => tracing::warn!(
                subagent = name,
                error = %err,
                "Failed to register read-only subagent"
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn register_default_subagents(
    agent: &mut ReactAgent,
    model: &str,
    llm_config: Option<LlmConfig>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    cache_user_id: &str,
) {
    for &(name, description, prompt) in WORKER_DEFINITIONS {
        match build_readonly_worker_agent(
            name,
            prompt,
            model,
            llm_config.clone(),
            temperature,
            max_tokens,
            token_limit,
            tool_timeout_ms,
            cache_user_id,
        ) {
            Ok(mut worker) => {
                // L3 nesting: register sub-agents on this worker so it can
                // recursively delegate via delegate_readonly (spec §3.3).
                // Sub-sub-workers are NOT further registered (L4 is blocked
                // by MAX_DELEGATE_DEPTH=3 in the framework).
                register_worker_subagents(
                    &mut worker,
                    model,
                    llm_config.clone(),
                    temperature,
                    max_tokens,
                    token_limit,
                    tool_timeout_ms,
                    cache_user_id,
                )
                .await;

                // Register delegate_readonly on the worker so it can
                // recursively delegate to L3 sub-workers (spec §3.3).
                let worker_handle = crate::agent_handle::AgentHandle::new(worker);
                // Workers are subagents, not orchestrators — plan-existence
                // interception is unnecessary (only main agent creates plans).
                crate::tasks::task_runtime::delegate_readonly_tool::
                    register_delegate_readonly_on_handle(&worker_handle, None).await;

                let def = SubagentBuilder::new(name)
                    .description(description)
                    .fork_mode()
                    .tag("readonly")
                    .tag("parallel")
                    .build();
                agent.register_subagent_with_definition(def, worker_handle.to_boxed_agent().await);
            }
            Err(err) => tracing::warn!(
                subagent = name,
                error = %err,
                "Failed to register default read-only subagent"
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_readonly_worker_agent(
    name: &str,
    prompt: &str,
    model: &str,
    llm_config: Option<LlmConfig>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    token_limit: usize,
    tool_timeout_ms: u64,
    cache_user_id: &str,
) -> std::result::Result<ReactAgent, echo_agent::error::ReactError> {
    let mut builder = ReactAgentBuilder::new()
        .model(model)
        .name(name)
        .system_prompt(prompt)
        .enable_tools()
        .enable_memory()
        .enable_cot()
        .enable_subagent()
        .max_iterations(0)
        .token_limit(token_limit)
        .max_tokens(max_tokens.or(Some(DEFAULT_MAX_TOKENS)))
        .temperature(temperature)
        .tool_execution(echo_agent::tools::ToolExecutionConfig {
            timeout_ms: tool_timeout_ms,
            ..Default::default()
        });

    let has_llm_config = llm_config.is_some();
    if let Some(config) = llm_config {
        tracing::info!(
            worker_name = name,
            model = %config.model,
            has_auth = !config.api_key.is_empty(),
            "worker: injecting LlmConfig"
        );
        builder = builder.llm_config(config);
    } else {
        tracing::warn!(
            worker_name = name,
            "worker: NO LlmConfig injected — will fall back to env vars / models.yaml"
        );
    }

    let mut worker = builder.build()?;
    let has_client = worker.llm_client().is_some();
    tracing::info!(
        worker_name = name,
        has_llm_config,
        has_llm_client = has_client,
        model = %worker.model_name(),
        "worker built: LLM client status"
    );
    worker.config_mut().set_cache_user_id(cache_user_id);
    worker.set_plan_mode(true);
    Ok(worker)
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

/// 创建运行时状态 Store（SQLite），失败时返回 None（禁用 checkpoint）
///
/// Persists `AgentCheckpoint`s (full messages + plan + active_skills + blocked_reason)
/// and the TaskNode DAG so a conversation can be resumed across process restarts.
/// Distinct from [`create_conversation_store`], which only stores user-visible
/// transcript projections.
pub fn create_runtime_state_store() -> Option<Arc<dyn RuntimeStateStore>> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    create_runtime_state_store_in(std::path::PathBuf::from(home).join(".echo-agent"))
}

/// 创建指定 base dir 下的运行时状态 Store（SQLite）。
pub fn create_runtime_state_store_in(
    base_dir: impl AsRef<std::path::Path>,
) -> Option<Arc<dyn RuntimeStateStore>> {
    let db_path = base_dir.as_ref().join("runtime_state.db");

    match SqliteRuntimeStateStore::new(&db_path) {
        Ok(store) => {
            tracing::info!("RuntimeStateStore (SQLite) 初始化: {}", db_path.display());
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!("RuntimeStateStore 初始化失败: {e}, 禁用运行时 checkpoint");
            None
        }
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
        // startup before any worker threads are spawned.
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
                    // Dual sink: keep the stderr console output (visible in the
                    // `cargo tauri dev` terminal) AND mirror to a rotating-ish
                    // file at ~/.echo-agent/logs/app.log so issues can be
                    // diagnosed after the fact without re-running. Append mode
                    // so restarts don't wipe the log.
                    use tracing_subscriber::layer::SubscriberExt;
                    let registry = tracing_subscriber::registry().with(env_filter());
                    let file_layer = app_log_file().map(|file| {
                        tracing_subscriber::fmt::layer()
                            .with_writer(std::sync::Mutex::new(file))
                            .with_ansi(false)
                    });
                    if let Some(file_layer) = file_layer {
                        let _ = registry
                            .with(tracing_subscriber::fmt::layer())
                            .with(file_layer)
                            .try_init();
                    } else {
                        let _ = registry.with(tracing_subscriber::fmt::layer()).try_init();
                    }
                }
            }
        }
    });
}

#[cfg_attr(feature = "telemetry", allow(dead_code))]
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

/// Open the shared GUI app log file for appending: `~/.echo-agent/logs/app.log`.
///
/// Used by the Stderr log target as a second sink so that `cargo tauri dev`
/// output is also persisted to disk (the stderr stream itself is lost once the
/// terminal that launched the app is closed). Append mode keeps history across
/// restarts; rotate/truncate manually if it grows too large.
fn app_log_file() -> Option<std::fs::File> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(home)
        .join(".echo-agent")
        .join("logs");
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
