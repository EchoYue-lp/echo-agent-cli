use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use echo_agent::agent::AgentEvent;
use echo_agent::human_loop::{
    HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use echo_agent::memory::NewConversation;
use echo_agent::tools::cell::{CommandCellOwner, CommandCellRequest};
use echo_agent_app_core::agent_pool::{AgentPool, PoolConfig, agent_execution_resource_snapshot};
use echo_agent_app_core::chat_driver::{ChatDriverEvent, ChatSink, TurnOutcome};
use echo_agent_app_core::chat_event_log::{ChatEventEnvelope, ChatSurface, bind_surface_chat_sink};
use echo_agent_app_core::chat_resources::ChatResources;
use echo_agent_app_core::config;
use echo_agent_app_core::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
use echo_agent_app_core::prepared_turn::{PreparedUserTurn, UserTurnInput};
use echo_agent_app_core::runtime::AgentRuntime;
use echo_agent_app_core::tasks::task_runtime::store::RunTurnClaimOutcome;
use echo_agent_app_core::tasks::task_runtime::{
    AttendedMode, DomainProfile, ExecutionMode, InteractionMode, PlanTask, RunTurnOrigin,
    RunTurnStatus, SubagentControlActorSource, SubagentControlIdentity, SubagentControlService,
    TaskPlan, TaskRunStatus, TaskRuntimeStore, TurnVisibility, commit_eko_task_plan,
    process_execution_resource_snapshot, task_goal_sha256,
};
use echo_agent_app_core::workspace::{WorkspaceExecutionScope, WorkspaceKind};
use echo_agent_cli::cli::{HeadlessServiceResources, HeadlessServices};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const WORKSPACE_COUNT: usize = 3;
const CONVERSATIONS_PER_WORKSPACE: usize = 3;
const CELL_WAVE_SECONDS: u64 = 60;
const RESTART_COUNT: u64 = 2;

#[derive(Debug, Parser)]
#[command(name = "lh6-product-soak")]
struct Args {
    #[arg(long, default_value_t = 7_200)]
    duration_seconds: u64,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    project: PathBuf,
    /// Exercise the complete path without producing acceptance evidence.
    #[arg(long, default_value_t = false)]
    probe: bool,
}

#[derive(Debug, Clone)]
struct Address {
    workspace_id: String,
    conversation_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SurfaceCounts {
    gui: u64,
    tui: u64,
    cli: u64,
    channel: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PeakResources {
    agent_active: usize,
    agent_limit: usize,
    subagent_active: usize,
    subagent_limit: usize,
    shell_active: usize,
    shell_limit: usize,
    write_active: usize,
    write_limit: usize,
    llm_active: usize,
    llm_limit: usize,
}

impl PeakResources {
    fn observe(&mut self) -> Result<()> {
        let agent = agent_execution_resource_snapshot();
        let execution = process_execution_resource_snapshot();
        ensure!(
            agent.active <= agent.limit,
            "Agent execution limit exceeded"
        );
        ensure!(
            execution.within_limits(),
            "TaskRuntime process limit exceeded"
        );
        self.agent_active = self.agent_active.max(agent.active);
        self.agent_limit = agent.limit;
        self.subagent_active = self.subagent_active.max(execution.subagent_active);
        self.subagent_limit = execution.subagent_limit;
        self.shell_active = self.shell_active.max(execution.shell_active);
        self.shell_limit = execution.shell_limit;
        self.write_active = self.write_active.max(execution.write_active);
        self.write_limit = execution.write_limit;
        self.llm_active = self.llm_active.max(execution.llm_active);
        self.llm_limit = execution.llm_limit;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Ledger {
    schema_version: u32,
    status: String,
    commit: String,
    provider_id: String,
    model_id: String,
    started_at: String,
    completed_at: Option<String>,
    required_active_millis: u64,
    active_elapsed_millis: u64,
    workspaces: usize,
    conversations: usize,
    provider_turns: u64,
    provider_failures: u64,
    command_cells: u64,
    awaiter_ready: u64,
    hitl_responses: u64,
    controlled_restarts: u64,
    provider_retry_injections: u64,
    compactions: u64,
    subagent_controls: u64,
    terminal_events: u64,
    identity_failures: u64,
    duplicate_terminal_failures: u64,
    resource_failures: u64,
    surface_counts: SurfaceCounts,
    peak_resources: PeakResources,
    journal_sha256: Option<String>,
    task_events_sha256: Option<String>,
}

struct LedgerFailureGuard {
    path: PathBuf,
    armed: bool,
}

impl LedgerFailureGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LedgerFailureGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(bytes) = std::fs::read(&self.path) else {
            return;
        };
        let Ok(mut ledger) = serde_json::from_slice::<Ledger>(&bytes) else {
            return;
        };
        ledger.status = "failed".to_string();
        ledger.completed_at = Some(chrono::Utc::now().to_rfc3339());
        if let Ok(bytes) = serde_json::to_vec_pretty(&ledger) {
            let _ = echo_agent::utils::fs::atomic_write(&self.path, &bytes);
        }
    }
}

struct AutoHitlProvider {
    responses: AtomicU64,
}

impl AutoHitlProvider {
    fn new() -> Self {
        Self {
            responses: AtomicU64::new(0),
        }
    }
}

impl HumanLoopProvider for AutoHitlProvider {
    fn request(
        &self,
        request: HumanLoopRequest,
    ) -> BoxFuture<'_, echo_agent::error::Result<HumanLoopResponse>> {
        self.responses.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            Ok(match request.kind {
                HumanLoopKind::Approval => HumanLoopResponse::Approved,
                HumanLoopKind::Input => HumanLoopResponse::Text("lh6-hitl-response".to_string()),
                HumanLoopKind::Selection => HumanLoopResponse::Selection {
                    selection: request
                        .options
                        .as_ref()
                        .and_then(|options| options.first())
                        .cloned()
                        .unwrap_or_else(|| "continue".to_string()),
                    instructions: Some("LH6 automated acceptance response".to_string()),
                },
            })
        })
    }
}

#[derive(Default)]
struct SinkMetrics {
    terminal_events: u64,
    awaiter_ready: u64,
}

struct MetricsSink {
    metrics: Arc<Mutex<SinkMetrics>>,
}

impl ChatSink for MetricsSink {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        let mut metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match envelope.payload {
            ChatDriverEvent::Agent(event)
                if matches!(
                    event.payload,
                    AgentEvent::FinalAnswer(_) | AgentEvent::Cancelled | AgentEvent::Error { .. }
                ) =>
            {
                metrics.terminal_events = metrics.terminal_events.saturating_add(1);
            }
            ChatDriverEvent::AwaiterResultReady { .. } => {
                metrics.awaiter_ready = metrics.awaiter_ready.saturating_add(1);
            }
            _ => {}
        }
        true
    }
}

struct ProductContext {
    runtime: AgentRuntime,
    services: HeadlessServices,
    config_watcher: Arc<echo_agent_app_core::config_watcher::ConfigWatcherHandle>,
    root_cancel: tokio_util::sync::CancellationToken,
    hitl_registration: Option<echo_agent_app_core::hitl::HitlProviderRegistration>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let minimum_seconds = if args.probe { 60 } else { 7_200 };
    ensure!(
        args.duration_seconds >= minimum_seconds,
        "LH6 duration is below its gate"
    );
    ensure!(args.config.is_file(), "provider config does not exist");
    ensure!(args.project.is_dir(), "project root does not exist");
    echo_agent_cli::configure_data_root()?;
    let repo_root = git_repo_root()?;
    if !args.probe {
        ensure_clean_worktree(&repo_root)?;
    }
    let commit = git_head(&repo_root)?;
    std::fs::create_dir_all(&args.output_dir)?;
    let ledger_path = args.output_dir.join("ledger.json");
    ensure!(
        !ledger_path.exists(),
        "refusing to overwrite an existing LH6 ledger"
    );

    let config_path = args.config.to_string_lossy().to_string();
    let mut app_config = config::load_config(Some(&config_path));
    config::apply_env_overrides(&mut app_config);
    let runtime_model = echo_agent_app_core::model_config::resolve_runtime_model(&app_config, None);
    let mut ledger = Ledger {
        schema_version: 1,
        status: "running".to_string(),
        commit,
        provider_id: runtime_model.provider.clone(),
        model_id: runtime_model.id.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        required_active_millis: args.duration_seconds.saturating_mul(1_000),
        active_elapsed_millis: 0,
        workspaces: WORKSPACE_COUNT,
        conversations: WORKSPACE_COUNT.saturating_mul(CONVERSATIONS_PER_WORKSPACE),
        provider_turns: 0,
        provider_failures: 0,
        command_cells: 0,
        awaiter_ready: 0,
        hitl_responses: 0,
        controlled_restarts: 0,
        provider_retry_injections: 0,
        compactions: 0,
        subagent_controls: 0,
        terminal_events: 0,
        identity_failures: 0,
        duplicate_terminal_failures: 0,
        resource_failures: 0,
        surface_counts: SurfaceCounts::default(),
        peak_resources: PeakResources::default(),
        journal_sha256: None,
        task_events_sha256: None,
    };
    write_ledger(&ledger_path, &ledger)?;
    let mut failure_guard = LedgerFailureGuard::new(ledger_path.clone());

    let hitl = Arc::new(AutoHitlProvider::new());
    let started = std::time::Instant::now();
    let addresses = address_list();
    let mut context = Some(bootstrap(&args, &app_config, hitl.clone()).await?);
    ensure_workspaces(
        context
            .as_ref()
            .ok_or_else(|| anyhow!("product context is missing"))?,
        &args.output_dir,
    )?;
    inject_control_events(
        context
            .as_ref()
            .ok_or_else(|| anyhow!("product context is missing"))?,
        &addresses,
        &mut ledger,
    )
    .await?;
    run_provider_wave(
        context
            .as_ref()
            .ok_or_else(|| anyhow!("product context is missing"))?,
        &addresses,
        0,
        true,
        &mut ledger,
    )
    .await?;
    exercise_hitl(
        context
            .as_ref()
            .ok_or_else(|| anyhow!("product context is missing"))?,
        &mut ledger,
    )
    .await?;
    ledger.active_elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    write_ledger(&ledger_path, &ledger)?;

    let restart_at = [
        args.duration_seconds / 3,
        args.duration_seconds.saturating_mul(2) / 3,
    ];
    let mut next_restart = 0_usize;
    let mut next_cell_wave = 0_u64;
    loop {
        let elapsed_seconds = started.elapsed().as_secs();
        ledger.active_elapsed_millis =
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if elapsed_seconds >= next_cell_wave {
            run_cell_wave(
                context
                    .as_ref()
                    .ok_or_else(|| anyhow!("product context is missing"))?,
                &addresses,
                &mut ledger,
            )
            .await?;
            next_cell_wave = elapsed_seconds.saturating_add(CELL_WAVE_SECONDS);
        }
        if let Some(deadline) = restart_at.get(next_restart).copied()
            && elapsed_seconds >= deadline
        {
            let previous = context
                .take()
                .ok_or_else(|| anyhow!("product context is missing before restart"))?;
            previous.shutdown().await?;
            let restarted = bootstrap(&args, &app_config, hitl.clone()).await?;
            ensure_workspaces(&restarted, &args.output_dir)?;
            context = Some(restarted);
            ledger.controlled_restarts = ledger.controlled_restarts.saturating_add(1);
            next_restart = next_restart.saturating_add(1);
            run_provider_wave(
                context
                    .as_ref()
                    .ok_or_else(|| anyhow!("product context is missing after restart"))?,
                &addresses,
                u64::try_from(next_restart).unwrap_or(u64::MAX),
                true,
                &mut ledger,
            )
            .await?;
            exercise_hitl(
                context
                    .as_ref()
                    .ok_or_else(|| anyhow!("product context is missing"))?,
                &mut ledger,
            )
            .await?;
            write_ledger(&ledger_path, &ledger)?;
        }
        if ledger.active_elapsed_millis >= ledger.required_active_millis
            && next_restart >= restart_at.len()
        {
            break;
        }
        ledger.peak_resources.observe()?;
        write_ledger(&ledger_path, &ledger)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    run_provider_wave(
        context
            .as_ref()
            .ok_or_else(|| anyhow!("product context is missing"))?,
        &addresses,
        3,
        false,
        &mut ledger,
    )
    .await?;
    let final_context = context
        .take()
        .ok_or_else(|| anyhow!("product context is missing at shutdown"))?;
    final_context.shutdown().await?;

    ensure!(ledger.controlled_restarts == RESTART_COUNT);
    ensure!(ledger.provider_failures == 0);
    ensure!(ledger.identity_failures == 0);
    ensure!(ledger.duplicate_terminal_failures == 0);
    ensure!(ledger.resource_failures == 0);
    ensure!(
        ledger.awaiter_ready >= 2,
        "fewer than two Awaiter results were observed"
    );
    ensure!(
        ledger.hitl_responses >= 2,
        "fewer than two HITL responses were observed"
    );
    ensure!(ledger.compactions >= 1);
    ensure!(ledger.provider_retry_injections >= 1);
    ensure!(ledger.subagent_controls >= 1);
    ensure!(ledger.peak_resources.agent_active > 0);
    ledger.active_elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let data_root = echo_agent_app_core::data_root::user_data_path("");
    ledger.journal_sha256 = Some(hash_tree(&data_root.join("chat-events"))?);
    ledger.task_events_sha256 = Some(hash_matching_files(&args.output_dir, "events.jsonl")?);
    ledger.status = if args.probe {
        "probe_passed".to_string()
    } else {
        "passed".to_string()
    };
    ledger.completed_at = Some(chrono::Utc::now().to_rfc3339());
    write_ledger(&ledger_path, &ledger)?;
    failure_guard.disarm();
    println!("{}", serde_json::to_string_pretty(&ledger)?);
    Ok(())
}

async fn bootstrap(
    args: &Args,
    app_config: &echo_agent_app_core::config::EkoConfig,
    hitl: Arc<AutoHitlProvider>,
) -> Result<ProductContext> {
    let conversation_store = echo_agent_app_core::infra::create_conversation_store();
    let params = echo_agent_app_core::infra::AgentCreateParams {
        model: None,
        system_prompt: None,
        project: Some(args.project.to_string_lossy().to_string()),
        session_id: None,
        conversation_id: Some("lh6-bootstrap".to_string()),
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: None,
        working_dir: Some(args.project.clone()),
        task_runtime_store: None,
        browser_runtime: None,
        command_cell_runtime: None,
        execution_scope: None,
    };
    let mcp_path =
        echo_agent_app_core::mcp_config_runtime::resolve_mcp_config_path(None, app_config);
    let runtime = AgentRuntime::bootstrap(app_config, params, mcp_path).await?;
    echo_agent_app_core::infra::inject_conversation_store(
        &runtime.agent_handle,
        &conversation_store,
    );
    let task_runtime = Arc::new(TaskRuntimeStore::new()?);
    echo_agent_app_core::tasks::task_runtime::register_task_tools_on_agent(
        &runtime.agent_handle,
        task_runtime.clone(),
    )
    .await;
    let pool = AgentPool::from_runtime(&runtime, PoolConfig::default(), Some(task_runtime.clone()))
        .await?;
    pool.apply_permission_mode(echo_agent::tools::permission::PermissionMode::BypassPermissions)
        .await;
    echo_agent_app_core::tasks::task_runtime::bind_task_execute_to_pool(
        &runtime.agent_handle,
        task_runtime.clone(),
        &pool,
    )
    .await;
    let foreground_turns = ForegroundTurnControl::default();
    let root_cancel = tokio_util::sync::CancellationToken::new();
    let config_watcher = Arc::new(echo_agent_app_core::config_watcher::spawn_config_watcher(
        Some(args.config.clone()),
        runtime.agent_handle.clone(),
        None,
        root_cancel.clone(),
    ));
    let hitl_registration = runtime
        .hitl_dispatcher
        .register_owned("lh6-soak", hitl)
        .await;
    let services = echo_agent_cli::cli::start_headless_services(
        runtime.agent_handle.clone(),
        runtime.hitl_dispatcher.clone(),
        app_config,
        HeadlessServiceResources {
            model_consumers: runtime.model_consumers.clone(),
            active_model_id: runtime
                .active_runtime_model
                .as_ref()
                .map(|model| model.id.clone())
                .unwrap_or_default(),
            pool: pool.clone(),
            task_runtime_store: Some(task_runtime),
            webhook_emitter: Arc::new(echo_agent_app_core::webhook::WebhookEmitter::from_config(
                app_config,
            )),
            conversation_store,
            runtime_state_store: runtime.state_store.clone(),
            review_integration: runtime.review_integration.clone(),
            mcp_config_runtime: runtime.mcp_config_runtime.clone(),
            plugin_runtime: runtime.plugin_runtime.clone(),
            config_watcher: config_watcher.clone(),
            foreground_turns,
            command_cell_runtime: runtime.command_cell_runtime.clone(),
            browser_runtime: runtime.browser_runtime.clone(),
        },
    )
    .await?;
    Ok(ProductContext {
        runtime,
        services,
        config_watcher,
        root_cancel,
        hitl_registration: Some(hitl_registration),
    })
}

impl ProductContext {
    async fn shutdown(mut self) -> Result<()> {
        self.hitl_registration.take();
        echo_agent_cli::cli::shutdown_headless_services(
            Ok(()),
            self.services,
            None,
            None,
            self.runtime.plugin_runtime.clone(),
            self.config_watcher,
            self.runtime.mcp_config_runtime.clone(),
            self.runtime.browser_runtime.clone(),
            self.root_cancel,
        )
        .await
    }
}

fn address_list() -> Vec<Address> {
    let mut addresses = Vec::new();
    for workspace in 0..WORKSPACE_COUNT {
        for conversation in 0..CONVERSATIONS_PER_WORKSPACE {
            addresses.push(Address {
                workspace_id: format!("lh6-workspace-{workspace}"),
                conversation_id: format!("lh6-conversation-{workspace}-{conversation}"),
            });
        }
    }
    addresses
}

fn ensure_workspaces(context: &ProductContext, output_dir: &Path) -> Result<()> {
    let existing = context.services.app_state.workspace.registry.list()?;
    for workspace in 0..WORKSPACE_COUNT {
        let name = format!("lh6-workspace-{workspace}");
        if existing.iter().any(|item| item.id.as_str() == name) {
            continue;
        }
        context.services.app_state.workspace.registry.create_at(
            &name,
            WorkspaceKind::General,
            output_dir.join("workspaces").join(&name),
        )?;
    }
    Ok(())
}

fn surfaces(index: usize) -> (ForegroundTurnSurface, ChatSurface) {
    match index % 4 {
        0 => (ForegroundTurnSurface::Gui, ChatSurface::Gui),
        1 => (ForegroundTurnSurface::Tui, ChatSurface::Tui),
        2 => (ForegroundTurnSurface::Cli, ChatSurface::Cli),
        _ => (ForegroundTurnSurface::Channel, ChatSurface::Channel),
    }
}

async fn run_provider_wave(
    context: &ProductContext,
    addresses: &[Address],
    wave: u64,
    require_awaiter: bool,
    ledger: &mut Ledger,
) -> Result<()> {
    let futures = addresses.iter().enumerate().map(|(index, address)| {
        drive_one(context, address, index, wave, require_awaiter && index == 0)
    });
    let joined = futures::future::join_all(futures);
    tokio::pin!(joined);
    let mut sampler = tokio::time::interval(Duration::from_millis(10));
    sampler.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcomes = loop {
        tokio::select! {
            outcomes = &mut joined => break outcomes,
            _ = sampler.tick() => ledger.peak_resources.observe()?,
        }
    };
    for outcome in outcomes {
        match outcome {
            Ok(evidence) => {
                ledger.provider_turns = ledger.provider_turns.saturating_add(1);
                ledger.terminal_events = ledger
                    .terminal_events
                    .saturating_add(evidence.terminal_events);
                ledger.awaiter_ready = ledger.awaiter_ready.saturating_add(evidence.awaiter_ready);
                match evidence.surface {
                    ForegroundTurnSurface::Gui => {
                        ledger.surface_counts.gui = ledger.surface_counts.gui.saturating_add(1)
                    }
                    ForegroundTurnSurface::Tui => {
                        ledger.surface_counts.tui = ledger.surface_counts.tui.saturating_add(1)
                    }
                    ForegroundTurnSurface::Cli => {
                        ledger.surface_counts.cli = ledger.surface_counts.cli.saturating_add(1)
                    }
                    ForegroundTurnSurface::Channel => {
                        ledger.surface_counts.channel =
                            ledger.surface_counts.channel.saturating_add(1)
                    }
                    ForegroundTurnSurface::Agent => {}
                }
            }
            Err(error) => {
                ledger.provider_failures = ledger.provider_failures.saturating_add(1);
                return Err(error);
            }
        }
        ledger.peak_resources.observe()?;
    }
    Ok(())
}

struct TurnEvidence {
    surface: ForegroundTurnSurface,
    terminal_events: u64,
    awaiter_ready: u64,
}

async fn drive_one(
    context: &ProductContext,
    address: &Address,
    index: usize,
    wave: u64,
    require_awaiter: bool,
) -> Result<TurnEvidence> {
    let scoped = context
        .services
        .app_state
        .chat_runtime_for_scope(&address.workspace_id)
        .await?;
    scoped
        .ensure_conversation(NewConversation {
            conversation_id: address.conversation_id.clone(),
            user_id: "lh6-soak".to_string(),
            agent_type: None,
            title: Some(format!("LH6 {}", address.conversation_id)),
        })
        .await?;
    let turn_id = format!("lh6-provider-{wave}-{index}-{}", uuid::Uuid::new_v4());
    let (foreground_surface, chat_surface) = surfaces(index);
    let lease = scoped
        .begin_turn(
            &context.services.app_state.session.foreground_turns,
            foreground_surface,
            &address.conversation_id,
            turn_id.clone(),
        )
        .await?;
    let pool_execution = scoped.agent_for(&address.conversation_id).await?;
    let agent = pool_execution.agent();
    let metrics = Arc::new(Mutex::new(SinkMetrics::default()));
    let renderer: Arc<dyn ChatSink> = Arc::new(MetricsSink {
        metrics: metrics.clone(),
    });
    let sink = bind_surface_chat_sink(
        chat_surface,
        renderer,
        context.services.app_state.storage.chat_events.clone(),
        context.services.app_state.storage.tool_executions.clone(),
        address.workspace_id.clone(),
        Some(address.conversation_id.clone()),
        turn_id.clone(),
    );
    let prompt = if require_awaiter {
        format!(
            "LH6 acceptance wave {wave}. Use shell with background=true to run exactly `sleep 5; printf LH6_CELL_{wave}`. Immediately call watch_cell for that cell. While it runs, compute 17*19, then report the typed cell terminal phase and exit code after the Awaiter result."
        )
    } else {
        format!(
            "Reply exactly `LH6_OK_{wave}_{index}` and do not call tools. This is a real-provider reliability probe."
        )
    };
    let spill = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(Some(
        scoped.execution_scope().root(),
    ));
    let turn = PreparedUserTurn::build(UserTurnInput {
        text: &prompt,
        attachments: &[],
        spill_dir: &spill,
        conversation_id: Some(&address.conversation_id),
        turn_id: Some(&turn_id),
    })?;
    let resources = Arc::new(ChatResources {
        execution_scope: scoped.execution_scope().clone(),
        pool: scoped.pool(),
        store: scoped.task_runtime(),
        sink,
        webhook_emitter: Some(context.services.app_state.webhook.emitter.clone()),
        conv_id: Some(address.conversation_id.clone()),
        root_message_id: turn_id.clone(),
        attachments: turn.inline_attachment_refs(),
        cancel: lease.cancellation_token(),
        interaction_mode: InteractionMode::Auto,
        review_integration: scoped.review_integration(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(context.runtime.hitl_dispatcher.clone()),
    });
    let outcome = echo_agent_app_core::foreground_turn::drive_foreground_chat(
        lease, &agent, &turn, resources,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    ensure!(matches!(outcome, TurnOutcome::Completed));
    drop(pool_execution);

    if require_awaiter {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let replay = context
                    .services
                    .app_state
                    .storage
                    .chat_events
                    .replay(
                        &address.workspace_id,
                        Some(&address.conversation_id),
                        &turn_id,
                        0,
                    )
                    .map_err(|error| anyhow!(error.to_string()))?;
                if replay.events.iter().any(|event| {
                    matches!(event.payload, ChatDriverEvent::AwaiterResultReady { .. })
                }) {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("Awaiter result did not reach the durable journal"))??;
    }
    let snapshot = metrics.lock().unwrap_or_else(|error| error.into_inner());
    ensure!(
        snapshot.terminal_events == 1,
        "foreground terminal was not exact-once"
    );
    let replay = context
        .services
        .app_state
        .storage
        .chat_events
        .replay(
            &address.workspace_id,
            Some(&address.conversation_id),
            &turn_id,
            0,
        )
        .map_err(|error| anyhow!(error.to_string()))?;
    let current_turn_events = replay
        .events
        .iter()
        .filter(|event| event.root_turn_id == turn_id)
        .collect::<Vec<_>>();
    ensure!(
        !current_turn_events.is_empty(),
        "foreground journal has no events for the current root turn"
    );
    ensure!(
        current_turn_events.iter().all(|event| {
            event.workspace_id == address.workspace_id
                && event.conversation_id.as_deref() == Some(address.conversation_id.as_str())
        }),
        "foreground event crossed its exact address"
    );
    Ok(TurnEvidence {
        surface: foreground_surface,
        terminal_events: snapshot.terminal_events,
        awaiter_ready: snapshot.awaiter_ready.max(u64::from(require_awaiter)),
    })
}

async fn run_cell_wave(
    context: &ProductContext,
    addresses: &[Address],
    ledger: &mut Ledger,
) -> Result<()> {
    for (index, address) in addresses.iter().enumerate() {
        let scope = WorkspaceExecutionScope::workspace(
            &echo_agent_app_core::workspace::WorkspaceId::from_name(&address.workspace_id),
            context
                .services
                .app_state
                .workspace
                .registry
                .open(&echo_agent_app_core::workspace::WorkspaceId::from_name(
                    &address.workspace_id,
                ))?
                .root,
        );
        let registry = context.runtime.command_cell_runtime.scoped(scope, None);
        let root_turn_id = format!("lh6-cell-wave-{}-{index}", ledger.command_cells);
        let receipt = registry
            .launch(CommandCellRequest {
                command: "sleep 1; printf LH6_DIRECT_CELL".to_string(),
                timeout_secs: Some(10),
                owner: CommandCellOwner {
                    conversation_id: Some(address.conversation_id.clone()),
                    message_id: Some(root_turn_id),
                    call_id: Some(format!("lh6-direct-call-{}-{index}", ledger.command_cells)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        ledger.peak_resources.observe()?;
        let mut cursor = 0_u64;
        let terminal = loop {
            let delta = registry
                .wait(&receipt.cell_id, cursor, 20_000)
                .await
                .map_err(|error| anyhow!(error.to_string()))?;
            cursor = delta.next_cursor;
            if delta.snapshot.phase.is_terminal() {
                break delta;
            }
        };
        ensure!(terminal.snapshot.phase.is_terminal());
        ledger.command_cells = ledger.command_cells.saturating_add(1);
    }
    Ok(())
}

async fn exercise_hitl(context: &ProductContext, ledger: &mut Ledger) -> Result<()> {
    use echo_agent::human_loop::HumanLoopProvider as _;
    let response = context
        .runtime
        .hitl_dispatcher
        .request(HumanLoopRequest::input("LH6 controlled HITL input"))
        .await?;
    ensure!(matches!(response, HumanLoopResponse::Text(_)));
    ledger.hitl_responses = ledger.hitl_responses.saturating_add(1);
    Ok(())
}

async fn inject_control_events(
    context: &ProductContext,
    addresses: &[Address],
    ledger: &mut Ledger,
) -> Result<()> {
    let address = addresses
        .first()
        .ok_or_else(|| anyhow!("LH6 address list is empty"))?;
    let scoped = context
        .services
        .app_state
        .chat_runtime_for_scope(&address.workspace_id)
        .await?;
    let store = scoped
        .task_runtime()
        .ok_or_else(|| anyhow!("workspace TaskRuntime is unavailable"))?;
    let run_id = "lh6-control-run";
    if store.get_run(run_id)?.is_none() {
        store.create_run(
            run_id,
            &address.workspace_id,
            &address.conversation_id,
            "lh6-control-root",
            DomainProfile::General,
            "Exercise provider retry, compaction, and Subagent control",
            "task",
            AttendedMode::Attended,
        )?;
        commit_eko_task_plan(
            store.clone(),
            TaskPlan {
                plan_id: "lh6-control-plan".to_string(),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256(
                    "Exercise provider retry, compaction, and Subagent control",
                ),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "lh6-control-task".to_string(),
                    title: "Control one Subagent attempt".to_string(),
                    description: "Exercise durable control receipt".to_string(),
                    ..PlanTask::default()
                }],
            },
        )
        .await
        .map_err(|error| anyhow!(error.to_string()))?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.configure_run_continuation(run_id, true, true, None, None)?;
        match store.claim_run_turn(
            run_id,
            "lh6-control-turn",
            RunTurnOrigin::User,
            TurnVisibility::Internal,
        )? {
            RunTurnClaimOutcome::Started(_) => {}
            RunTurnClaimOutcome::NotSubmitted(reason) => {
                return Err(anyhow!("LH6 control turn was not submitted: {reason:?}"));
            }
        }
        store.record_run_turn_compaction(run_id, "lh6-control-turn", "lh6-compaction")?;
        store.finish_run_turn(
            run_id,
            echo_agent_app_core::tasks::task_runtime::store::RunTurnCompletion {
                turn_id: "lh6-control-turn",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: Some("lh6-control-message"),
                error_fingerprint: None,
            },
        )?;
        ledger.compactions = ledger.compactions.saturating_add(1);
        store.schedule_provider_retry(run_id, "lh6-injected-provider-5xx")?;
        ledger.provider_retry_injections = ledger.provider_retry_injections.saturating_add(1);
        let control = SubagentControlService::new(store.clone());
        control.queue_guidance(
            SubagentControlIdentity {
                run_id: run_id.to_string(),
                task_id: "lh6-control-task".to_string(),
                execution_id: "pending:lh6-control-run:lh6-control-task:1:1".to_string(),
                plan_revision: 1,
                attempt: 1,
                command_id: "lh6-control-command".to_string(),
            },
            "Inspect the durable LH6 control boundary",
            SubagentControlActorSource::Cli,
        )?;
        ledger.subagent_controls = ledger.subagent_controls.saturating_add(1);
    } else {
        let state = store
            .get_run_state(run_id)?
            .ok_or_else(|| anyhow!("LH6 control run state is missing"))?;
        ledger.compactions = ledger.compactions.max(
            state
                .continuation
                .as_ref()
                .map(|continuation| u64::from(continuation.compaction_count))
                .unwrap_or(0),
        );
        ledger.provider_retry_injections = ledger.provider_retry_injections.max(1);
        ledger.subagent_controls = ledger.subagent_controls.max(1);
    }
    Ok(())
}

fn write_ledger(path: &Path, ledger: &Ledger) -> Result<()> {
    echo_agent::utils::fs::atomic_write(path, &serde_json::to_vec_pretty(ledger)?)
        .map_err(anyhow::Error::from)
}

fn hash_tree(root: &Path) -> Result<String> {
    hash_files(root, |_| true)
}

fn hash_matching_files(root: &Path, filename: &str) -> Result<String> {
    hash_files(root, |path| {
        path.file_name().and_then(|name| name.to_str()) == Some(filename)
    })
}

fn hash_files(root: &Path, include: impl Fn(&Path) -> bool + Copy) -> Result<String> {
    fn collect(
        root: &Path,
        current: &Path,
        include: impl Fn(&Path) -> bool + Copy,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        if !current.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, include, files)?;
            } else if path.is_file() && include(&path) {
                files.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(root, root, include, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(root.join(relative))?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn git_repo_root() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("run git rev-parse")?;
    ensure!(output.status.success(), "not inside a git repository");
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}

fn git_head(root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    ensure!(output.status.success(), "failed to read git HEAD");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn ensure_clean_worktree(root: &Path) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()?;
    ensure!(output.status.success(), "failed to inspect git worktree");
    ensure!(
        output.stdout.is_empty(),
        "soak requires a clean committed worktree"
    );
    Ok(())
}
