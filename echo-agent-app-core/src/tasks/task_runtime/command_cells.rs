//! Thin EKO adapter over the framework command-cell registry.
//!
//! Process execution, output retention, waiting, cancellation and sandboxing
//! remain authoritative in `BackgroundCommandManager`. This adapter only
//! projects lifecycle facts into the owning TaskRuntime event stream.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use echo_agent::sandbox::{SandboxExecutor, SandboxManager};
use echo_agent::tasks::{BackgroundCommandManager, BackgroundCommandManagerConfig};
use echo_agent::tools::cell::{
    CommandCellDelta, CommandCellError, CommandCellLaunchReceipt, CommandCellObservationLease,
    CommandCellRegistry, CommandCellRequest, CommandCellSnapshot,
};
use echo_agent::tools::{Tool, ToolContext, ToolParameters, ToolResult};
use futures::future::BoxFuture;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::store::TaskRuntimeStore;
use super::types::{
    BackgroundCellArtifactStatus, BackgroundCellPhase, BackgroundCellState,
    BackgroundCellTerminalCause,
};

const OBSERVER_YIELD_MS: u64 = 30_000;
const OUTPUT_EXCERPT_CHARS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RunCellScope {
    workspace_id: String,
    run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChatCellScope {
    workspace_id: String,
    conversation_id: String,
    root_turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCellProjectionDiagnostic {
    pub cell_id: String,
    pub message: String,
}

pub struct CommandCellRuntimeService {
    inner: Arc<BackgroundCommandManager>,
    run_cells: RwLock<HashMap<RunCellScope, HashSet<String>>>,
    chat_cells: RwLock<HashMap<ChatCellScope, HashSet<String>>>,
    stores_by_workspace: RwLock<HashMap<String, Weak<TaskRuntimeStore>>>,
    projection_degraded: RwLock<HashMap<String, CommandCellProjectionDiagnostic>>,
    governor: Arc<super::executor::ProcessExecutionGovernor>,
    observers: TaskTracker,
    shutdown: CancellationToken,
    chat_events: Arc<crate::chat_event_log::ChatEventLog>,
}

impl CommandCellRuntimeService {
    pub fn new(
        sandbox: Arc<SandboxManager>,
        chat_events: Arc<crate::chat_event_log::ChatEventLog>,
    ) -> Result<Arc<Self>, String> {
        let executor: Arc<dyn SandboxExecutor> = sandbox;
        Ok(Arc::new(Self {
            inner: Arc::new(BackgroundCommandManager::new_with_sandbox(
                BackgroundCommandManagerConfig::default(),
                executor,
            )?),
            run_cells: RwLock::new(HashMap::new()),
            chat_cells: RwLock::new(HashMap::new()),
            stores_by_workspace: RwLock::new(HashMap::new()),
            projection_degraded: RwLock::new(HashMap::new()),
            governor: super::executor::process_execution_governor(),
            observers: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            chat_events,
        }))
    }

    pub fn chat_events(&self) -> Arc<crate::chat_event_log::ChatEventLog> {
        self.chat_events.clone()
    }

    pub fn projection_diagnostics(&self) -> Vec<CommandCellProjectionDiagnostic> {
        self.projection_degraded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn mark_projection_degraded(&self, cell_id: &str, message: impl Into<String>) {
        self.projection_degraded
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                cell_id.to_string(),
                CommandCellProjectionDiagnostic {
                    cell_id: cell_id.to_string(),
                    message: message.into(),
                },
            );
    }

    fn clear_projection_degraded(&self, cell_id: &str) {
        self.projection_degraded
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(cell_id);
    }

    pub fn scoped(
        self: &Arc<Self>,
        execution_scope: crate::workspace::WorkspaceExecutionScope,
        store: Option<Arc<TaskRuntimeStore>>,
    ) -> Arc<dyn CommandCellRegistry> {
        if let Some(store) = store.as_ref() {
            self.bind_store(store);
        }
        Arc::new(ScopedCommandCellRegistry {
            service: self.clone(),
            execution_scope,
        })
    }

    pub fn bind_store(self: &Arc<Self>, store: &Arc<TaskRuntimeStore>) {
        store.bind_command_cell_runtime(Arc::downgrade(self));
        self.stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(store.active_workspace_id(), Arc::downgrade(store));
    }

    fn store_for_workspace(&self, workspace_id: &str) -> Option<Arc<TaskRuntimeStore>> {
        let mut stores = self
            .stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let store = stores.get(workspace_id).and_then(Weak::upgrade);
        if store.is_none() {
            stores.remove(workspace_id);
        }
        store
    }

    pub(crate) fn rebind_store_workspace(&self, previous: &str, current: &str) {
        let mut stores = self
            .stores_by_workspace
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(store) = stores.remove(previous) {
            stores.insert(current.to_string(), store);
        }
    }

    fn track(&self, scope: &RunCellScope, cell_id: &str) {
        self.run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(scope.clone())
            .or_default()
            .insert(cell_id.to_string());
    }

    fn forget(&self, scope: &RunCellScope, cell_id: &str) {
        let mut run_cells = self
            .run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let remove_run = run_cells.get_mut(scope).is_some_and(|cells| {
            cells.remove(cell_id);
            cells.is_empty()
        });
        if remove_run {
            run_cells.remove(scope);
        }
    }

    fn track_chat(&self, scope: &ChatCellScope, cell_id: &str) {
        self.chat_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(scope.clone())
            .or_default()
            .insert(cell_id.to_string());
    }

    fn forget_chat(&self, scope: &ChatCellScope, cell_id: &str) {
        let mut chat_cells = self
            .chat_cells
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let remove_scope = chat_cells.get_mut(scope).is_some_and(|cells| {
            cells.remove(cell_id);
            cells.is_empty()
        });
        if remove_scope {
            chat_cells.remove(scope);
        }
    }

    fn append_chat_cell_fact(
        &self,
        scope: &ChatCellScope,
        cell: &BackgroundCellState,
        settled: bool,
    ) -> Result<(), String> {
        let replay = self
            .chat_events
            .replay(
                &scope.workspace_id,
                Some(&scope.conversation_id),
                &scope.root_turn_id,
                0,
            )
            .map_err(|error| error.to_string())?;
        if let Some(existing) = find_chat_cell_fact(&replay.events, &cell.cell_id, settled) {
            return if existing == cell {
                Ok(())
            } else {
                Err(format!(
                    "conflicting ordinary-chat command-cell fact for {}",
                    cell.cell_id
                ))
            };
        }
        let event = if settled {
            crate::chat_driver::ChatDriverEvent::CommandCellSettled {
                cell: Box::new(cell.clone()),
            }
        } else {
            crate::chat_driver::ChatDriverEvent::CommandCellStarted {
                cell: Box::new(cell.clone()),
            }
        };
        match self.chat_events.append(
            &scope.workspace_id,
            Some(&scope.conversation_id),
            &scope.root_turn_id,
            event,
        ) {
            Ok(_) => Ok(()),
            Err(append_error) => {
                let repaired = self
                    .chat_events
                    .replay(
                        &scope.workspace_id,
                        Some(&scope.conversation_id),
                        &scope.root_turn_id,
                        0,
                    )
                    .map_err(|repair_error| {
                        format!("{append_error}; journal repair failed: {repair_error}")
                    })?;
                match find_chat_cell_fact(&repaired.events, &cell.cell_id, settled) {
                    Some(existing) if existing == cell => Ok(()),
                    Some(_) => Err(format!(
                        "conflicting ordinary-chat command-cell fact after repair for {}",
                        cell.cell_id
                    )),
                    None => Err(append_error.to_string()),
                }
            }
        }
    }

    pub(crate) fn stop_run(&self, workspace_id: &str, run_id: &str) -> usize {
        let scope = RunCellScope {
            workspace_id: workspace_id.to_string(),
            run_id: run_id.to_string(),
        };
        let cells = self
            .run_cells
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&scope)
            .unwrap_or_default();
        cells
            .into_iter()
            .filter(|cell_id| self.inner.stop(cell_id))
            .count()
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.shutdown.cancel();
        self.observers.close();
        let shutdown_result = self
            .inner
            .shutdown()
            .await
            .map_err(|error| error.to_string());
        self.observers.wait().await;
        shutdown_result
    }
}

struct ScopedCommandCellRegistry {
    service: Arc<CommandCellRuntimeService>,
    execution_scope: crate::workspace::WorkspaceExecutionScope,
}

impl ScopedCommandCellRegistry {
    #[allow(clippy::too_many_arguments)]
    fn spawn_observer(
        &self,
        observation: CommandCellObservationLease,
        store: Arc<TaskRuntimeStore>,
        scope: RunCellScope,
        cell_id: String,
        name: String,
        call_id: Option<String>,
        shell_permit: Option<OwnedSemaphorePermit>,
    ) -> Result<(), CommandCellError> {
        if self.service.shutdown.is_cancelled() {
            return Err(CommandCellError::Shutdown);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| CommandCellError::Runtime {
                message: format!("Tokio runtime is unavailable: {error}"),
            })?;
        let service = self.service.clone();
        let registry = service.inner.clone();
        let observers = service.observers.clone();
        drop(observers.spawn_on(
            async move {
                let _observation = observation;
                let _shell_permit = shell_permit;
                observe_terminal_cell(
                    registry,
                    store,
                    scope.run_id.clone(),
                    cell_id.clone(),
                    name,
                    call_id,
                    service.clone(),
                )
                .await;
                service.forget(&scope, &cell_id);
            },
            &runtime,
        ));
        Ok(())
    }

    fn spawn_chat_observer(
        &self,
        observation: CommandCellObservationLease,
        scope: ChatCellScope,
        started: BackgroundCellState,
        shell_permit: Option<OwnedSemaphorePermit>,
    ) -> Result<(), CommandCellError> {
        if self.service.shutdown.is_cancelled() {
            return Err(CommandCellError::Shutdown);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| CommandCellError::Runtime {
                message: format!("Tokio runtime is unavailable: {error}"),
            })?;
        let service = self.service.clone();
        let registry = service.inner.clone();
        let observers = service.observers.clone();
        drop(observers.spawn_on(
            async move {
                let _observation = observation;
                let _shell_permit = shell_permit;
                let cell_id = started.cell_id.clone();
                observe_chat_terminal_cell(registry, service.clone(), scope.clone(), started).await;
                service.forget_chat(&scope, &cell_id);
            },
            &runtime,
        ));
        Ok(())
    }

    async fn launch_chat(
        &self,
        request: CommandCellRequest,
        name: String,
        command_hash: String,
    ) -> Result<CommandCellLaunchReceipt, CommandCellError> {
        let owner = request.owner.clone();
        let conversation_id =
            owner
                .conversation_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "ordinary Chat cell requires conversation identity".to_string(),
                })?;
        let root_turn_id =
            owner
                .message_id
                .clone()
                .ok_or_else(|| CommandCellError::Validation {
                    message: "ordinary Chat cell requires root message identity".to_string(),
                })?;
        let reservation = self.service.inner.prepare_launch(request).await?;
        let receipt = reservation.receipt().clone();
        let cell_id = receipt.cell_id.clone();
        let observation = self.service.inner.observe(&cell_id)?;
        let scope = ChatCellScope {
            workspace_id: self.execution_scope.workspace_id().to_string(),
            conversation_id,
            root_turn_id,
        };
        let started = BackgroundCellState {
            cell_id: cell_id.clone(),
            name,
            command_hash,
            turn_id: owner.turn_id.clone(),
            execution_id: owner.execution_id.clone(),
            call_id: owner.call_id.clone(),
            phase: BackgroundCellPhase::Prepared,
            terminal_cause: None,
            terminal_message: None,
            exit_code: None,
            artifact_status: BackgroundCellArtifactStatus::NotRequested,
            artifact_message: None,
            total_output_bytes: 0,
            output_truncated: false,
            output_excerpt: None,
            artifact_path: None,
            artifact_sha256: None,
            started_at: receipt.accepted_at,
            finished_at: None,
        };
        if let Err(error) = self.service.append_chat_cell_fact(&scope, &started, false) {
            let _ = self
                .service
                .inner
                .abort_prepared(reservation, format!("Started persistence failed: {error}"));
            return Err(CommandCellError::Runtime {
                message: format!("ordinary Chat cell start could not be persisted: {error}"),
            });
        }
        self.service.track_chat(&scope, &cell_id);

        let deadline = (receipt.deadline - chrono::Utc::now())
            .to_std()
            .unwrap_or_default();
        let shell = self.service.governor.shell_semaphore().acquire_owned();
        let shell_permit = tokio::select! {
            _ = self.service.shutdown.cancelled() => Err(CommandCellError::Shutdown),
            result = tokio::time::timeout(deadline, shell) => result
                .map_err(|_| CommandCellError::CapacityDeadline)
                .and_then(|permit| permit.map_err(|_| CommandCellError::Shutdown)),
        };
        let shell_permit = match shell_permit {
            Ok(permit) => permit,
            Err(error) => {
                let _ = self.service.inner.abort_prepared(
                    reservation,
                    format!("process shell admission failed: {error}"),
                );
                self.spawn_chat_observer(observation, scope, started, None)?;
                return Err(error);
            }
        };
        let start_result = self.service.inner.start_prepared(reservation).await;
        if let Err(error) = start_result {
            self.spawn_chat_observer(observation, scope, started, Some(shell_permit))?;
            return Err(error);
        }
        self.spawn_chat_observer(observation, scope, started, Some(shell_permit))?;
        Ok(receipt)
    }
}

/// Install the Task/Auto-safe awaiter dispatch surface. It delegates to the
/// already-registered `agent_tool` internally, so awaiter remains an ephemeral
/// Subagent and never becomes a second TaskRuntime task relation.
pub(crate) fn install_watch_cell_tool(
    agent: &mut echo_agent::agent::ReactAgent,
    registry: Arc<dyn CommandCellRegistry>,
) {
    agent.add_tool(Box::new(WatchCellTool {
        registry,
        tool_manager: Arc::downgrade(agent.tool_manager()),
    }));
}

struct WatchCellTool {
    registry: Arc<dyn CommandCellRegistry>,
    tool_manager: std::sync::Weak<echo_agent::tools::ToolManager>,
}

impl Tool for WatchCellTool {
    fn name(&self) -> &str {
        "watch_cell"
    }

    fn description(&self) -> &str {
        "Dispatch the dedicated low-reasoning awaiter Subagent to watch one running background command cell. Returns immediately; the current agent can continue other work."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "cell_id": {
                    "type": "string",
                    "description": "Running cell ID returned by shell(background=true)"
                }
            },
            "required": ["cell_id"]
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        let registry = Arc::clone(&self.registry);
        let manager = self.tool_manager.clone();
        Box::pin(async move {
            let cell_id = parameters
                .get("cell_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    echo_agent::error::ToolError::MissingParameter("cell_id".to_string())
                })?;
            let snapshot = registry.wait(cell_id, 0, 0).await.map_err(|error| {
                echo_agent::error::ToolError::ExecutionFailed {
                    tool: "watch_cell".to_string(),
                    message: error.to_string(),
                }
            })?;
            if snapshot.snapshot.phase.is_terminal() {
                return Ok(ToolResult::error(format!(
                    "cell {cell_id} is already {}; use wait to read its terminal result",
                    snapshot.snapshot.phase.as_str()
                )));
            }
            let Some(manager) = manager.upgrade() else {
                return Ok(ToolResult::error(
                    "awaiter dispatch runtime is no longer available",
                ));
            };
            let mut dispatch = ToolParameters::new();
            dispatch.insert("agent_name".to_string(), json!("awaiter"));
            dispatch.insert(
                "task".to_string(),
                json!(format!(
                    "Watch command cell {cell_id} until it reaches a terminal state and all output is drained."
                )),
            );
            dispatch.insert("background".to_string(), json!(true));
            manager
                .execute_tool_with_context("agent_tool", dispatch, context)
                .await
        })
    }
}

impl CommandCellRegistry for ScopedCommandCellRegistry {
    fn launch(
        &self,
        request: CommandCellRequest,
    ) -> BoxFuture<'_, Result<CommandCellLaunchReceipt, CommandCellError>> {
        Box::pin(async move {
            let owner = request.owner.clone();
            let name = request.command.chars().take(80).collect::<String>();
            let command_hash = format!("{:x}", Sha256::digest(request.command.as_bytes()));
            let Some(run_id) = owner.run_id.clone() else {
                return self.launch_chat(request, name, command_hash).await;
            };
            let store = self
                .service
                .store_for_workspace(self.execution_scope.workspace_id())
                .ok_or_else(|| CommandCellError::Validation {
                    message: "run-owned cell requires the scoped TaskRuntimeStore".to_string(),
                })?;
            let run = store
                .get_run(&run_id)
                .map_err(|error| CommandCellError::Runtime {
                    message: error.to_string(),
                })?
                .ok_or_else(|| CommandCellError::Validation {
                    message: format!("run '{run_id}' does not exist in the scoped store"),
                })?;
            if run.workspace_id != self.execution_scope.workspace_id() {
                return Err(CommandCellError::Validation {
                    message: format!(
                        "run workspace '{}' does not match scoped workspace '{}'",
                        run.workspace_id,
                        self.execution_scope.workspace_id()
                    ),
                });
            }
            if owner.conversation_id.as_deref() != Some(run.conversation_id.as_str()) {
                return Err(CommandCellError::Validation {
                    message: "cell conversation does not match its TaskRun".to_string(),
                });
            }

            let reservation = self.service.inner.prepare_launch(request).await?;
            let receipt = reservation.receipt().clone();
            let cell_id = receipt.cell_id.clone();
            let observation = self.service.inner.observe(&cell_id)?;
            let scope = RunCellScope {
                workspace_id: run.workspace_id.clone(),
                run_id: run_id.clone(),
            };
            let start_commit = store.record_background_cell_started(
                &run_id,
                &cell_id,
                &name,
                &command_hash,
                owner.turn_id.as_deref(),
                owner.execution_id.as_deref(),
                owner.call_id.as_deref(),
            );
            match start_commit {
                Ok(super::store::BackgroundCellStartCommit::Durable) => {
                    self.service.track(&scope, &cell_id);
                }
                Ok(super::store::BackgroundCellStartCommit::CommittedProjectionDegraded {
                    detail,
                }) => {
                    self.service.track(&scope, &cell_id);
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("Started projection degraded: {detail}"),
                    );
                    self.spawn_observer(
                        observation,
                        store,
                        scope,
                        cell_id,
                        name,
                        owner.call_id,
                        None,
                    )?;
                    return Err(CommandCellError::Runtime {
                        message: format!("cell start committed but projection degraded: {detail}"),
                    });
                }
                Err(error) => {
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("Started persistence failed: {error}"),
                    );
                    return Err(CommandCellError::Runtime {
                        message: format!("cell start event could not be persisted: {error}"),
                    });
                }
            }

            let deadline = (receipt.deadline - chrono::Utc::now())
                .to_std()
                .unwrap_or_default();
            let shell = self.service.governor.shell_semaphore().acquire_owned();
            let shell_permit = tokio::select! {
                _ = self.service.shutdown.cancelled() => Err(CommandCellError::Shutdown),
                result = tokio::time::timeout(deadline, shell) => result
                    .map_err(|_| CommandCellError::CapacityDeadline)
                    .and_then(|permit| permit.map_err(|_| CommandCellError::Shutdown)),
            };
            let shell_permit = match shell_permit {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = self.service.inner.abort_prepared(
                        reservation,
                        format!("process shell admission failed: {error}"),
                    );
                    self.spawn_observer(
                        observation,
                        store,
                        scope,
                        cell_id,
                        name,
                        owner.call_id,
                        None,
                    )?;
                    return Err(error);
                }
            };
            let start_result = self.service.inner.start_prepared(reservation).await;
            if let Err(error) = start_result {
                self.spawn_observer(
                    observation,
                    store,
                    scope,
                    cell_id,
                    name,
                    owner.call_id,
                    Some(shell_permit),
                )?;
                return Err(error);
            }
            self.spawn_observer(
                observation,
                store,
                scope,
                cell_id,
                name,
                owner.call_id,
                Some(shell_permit),
            )?;

            Ok(receipt)
        })
    }

    fn wait(
        &self,
        cell_id: &str,
        cursor: u64,
        yield_ms: u64,
    ) -> BoxFuture<'_, Result<CommandCellDelta, CommandCellError>> {
        self.service.inner.wait(cell_id, cursor, yield_ms)
    }

    fn observe(&self, cell_id: &str) -> Result<CommandCellObservationLease, CommandCellError> {
        self.service.inner.observe(cell_id)
    }

    fn stop(&self, cell_id: &str) -> bool {
        self.service.inner.stop(cell_id)
    }

    fn list(&self) -> BoxFuture<'_, Vec<CommandCellSnapshot>> {
        self.service.inner.list()
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), CommandCellError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn observe_terminal_cell(
    registry: Arc<BackgroundCommandManager>,
    store: Arc<TaskRuntimeStore>,
    run_id: String,
    cell_id: String,
    name: String,
    call_id: Option<String>,
    service: Arc<CommandCellRuntimeService>,
) {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    loop {
        let delta = match registry.wait(&cell_id, cursor, OBSERVER_YIELD_MS).await {
            Ok(delta) => delta,
            Err(error) => {
                let error_message = error.to_string();
                let persisted = persist_terminal_with_retry(
                    &store,
                    &run_id,
                    &cell_id,
                    &name,
                    BackgroundCellPhase::Failed,
                    Some(BackgroundCellTerminalCause::ObserverFailed),
                    Some(&error_message),
                    None,
                    BackgroundCellArtifactStatus::NotRequested,
                    None,
                    0,
                    false,
                    None,
                    None,
                    None,
                    call_id.as_deref(),
                    &service,
                )
                .await;
                if persisted {
                    super::continuation::wake_after_cell_terminal(&store, &run_id);
                }
                return;
            }
        };
        push_tail(&mut excerpt, &delta.new_output, OUTPUT_EXCERPT_CHARS);
        cursor = delta.next_cursor;
        if !delta.snapshot.phase.is_terminal() || cursor < delta.snapshot.total_output_bytes {
            continue;
        }
        let artifact_path = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string());
        let artifact_sha256 = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.sha256.clone());
        let persisted = persist_terminal_with_retry(
            &store,
            &run_id,
            &cell_id,
            &name,
            project_phase(delta.snapshot.phase),
            delta.snapshot.terminal_cause.map(project_terminal_cause),
            delta.snapshot.terminal_message.as_deref(),
            delta.snapshot.exit_code,
            project_artifact_status(&delta.snapshot.artifact_status),
            delta.snapshot.artifact_message.as_deref(),
            delta.snapshot.total_output_bytes,
            delta.snapshot.output_truncated,
            (!excerpt.is_empty()).then_some(excerpt.as_str()),
            artifact_path.as_deref(),
            artifact_sha256.as_deref(),
            call_id.as_deref(),
            &service,
        )
        .await;
        if persisted {
            super::continuation::wake_after_cell_terminal(&store, &run_id);
        }
        return;
    }
}

async fn observe_chat_terminal_cell(
    registry: Arc<BackgroundCommandManager>,
    service: Arc<CommandCellRuntimeService>,
    scope: ChatCellScope,
    mut cell: BackgroundCellState,
) {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    loop {
        let delta = match registry
            .wait(&cell.cell_id, cursor, OBSERVER_YIELD_MS)
            .await
        {
            Ok(delta) => delta,
            Err(error) => {
                cell.phase = BackgroundCellPhase::Failed;
                cell.terminal_cause = Some(BackgroundCellTerminalCause::ObserverFailed);
                cell.terminal_message = Some(error.to_string());
                cell.finished_at = Some(chrono::Utc::now());
                persist_chat_terminal_with_retry(&service, &scope, &cell).await;
                return;
            }
        };
        push_tail(&mut excerpt, &delta.new_output, OUTPUT_EXCERPT_CHARS);
        cursor = delta.next_cursor;
        if !delta.snapshot.phase.is_terminal() || cursor < delta.snapshot.total_output_bytes {
            continue;
        }
        cell.phase = project_phase(delta.snapshot.phase);
        cell.terminal_cause = delta.snapshot.terminal_cause.map(project_terminal_cause);
        cell.terminal_message = delta.snapshot.terminal_message;
        cell.exit_code = delta.snapshot.exit_code;
        cell.artifact_status = project_artifact_status(&delta.snapshot.artifact_status);
        cell.artifact_message = delta.snapshot.artifact_message;
        cell.total_output_bytes = delta.snapshot.total_output_bytes;
        cell.output_truncated = delta.snapshot.output_truncated;
        cell.output_excerpt = (!excerpt.is_empty()).then_some(excerpt);
        cell.artifact_path = delta
            .snapshot
            .output_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string());
        cell.artifact_sha256 = delta
            .snapshot
            .output_artifact
            .map(|artifact| artifact.sha256);
        cell.finished_at = Some(chrono::Utc::now());
        persist_chat_terminal_with_retry(&service, &scope, &cell).await;
        return;
    }
}

async fn persist_chat_terminal_with_retry(
    service: &CommandCellRuntimeService,
    scope: &ChatCellScope,
    cell: &BackgroundCellState,
) {
    let mut delay = Duration::from_millis(50);
    let mut attempt = 0_u64;
    loop {
        attempt = attempt.saturating_add(1);
        match service.append_chat_cell_fact(scope, cell, true) {
            Ok(()) => {
                service.clear_projection_degraded(&cell.cell_id);
                return;
            }
            Err(error) => {
                service.mark_projection_degraded(&cell.cell_id, error.clone());
                tracing::warn!(
                    workspace_id = scope.workspace_id,
                    conversation_id = scope.conversation_id,
                    root_turn_id = scope.root_turn_id,
                    cell_id = cell.cell_id,
                    attempt,
                    %error,
                    "retrying ordinary-chat command-cell terminal persistence"
                );
                if service.shutdown.is_cancelled() {
                    return;
                }
                tokio::select! {
                    _ = service.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = delay.saturating_mul(2).min(Duration::from_secs(30));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_terminal_with_retry(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    cell_id: &str,
    name: &str,
    phase: BackgroundCellPhase,
    terminal_cause: Option<BackgroundCellTerminalCause>,
    terminal_message: Option<&str>,
    exit_code: Option<i32>,
    artifact_status: BackgroundCellArtifactStatus,
    artifact_message: Option<&str>,
    total_output_bytes: u64,
    output_truncated: bool,
    output_excerpt: Option<&str>,
    artifact_path: Option<&str>,
    artifact_sha256: Option<&str>,
    call_id: Option<&str>,
    service: &CommandCellRuntimeService,
) -> bool {
    let mut delay = Duration::from_millis(50);
    let mut attempt = 0_u64;
    loop {
        attempt = attempt.saturating_add(1);
        match store.record_background_cell_finished(
            run_id,
            cell_id,
            name,
            phase,
            terminal_cause,
            terminal_message,
            exit_code,
            artifact_status,
            artifact_message,
            total_output_bytes,
            output_truncated,
            output_excerpt,
            artifact_path,
            artifact_sha256,
            call_id,
        ) {
            Ok(()) => {
                service.clear_projection_degraded(cell_id);
                return true;
            }
            Err(error) => {
                service.mark_projection_degraded(cell_id, error.to_string());
                tracing::warn!(
                    run_id,
                    cell_id,
                    attempt,
                    %error,
                    "retrying terminal command-cell event persistence"
                );
                if service.shutdown.is_cancelled() {
                    return false;
                }
                tokio::select! {
                    _ = service.shutdown.cancelled() => return false,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = delay.saturating_mul(2).min(Duration::from_secs(30));
            }
        }
    }
}

fn project_phase(phase: echo_agent::tools::cell::CommandCellPhase) -> BackgroundCellPhase {
    use echo_agent::tools::cell::CommandCellPhase;
    match phase {
        CommandCellPhase::Prepared => BackgroundCellPhase::Prepared,
        CommandCellPhase::Queued => BackgroundCellPhase::Queued,
        CommandCellPhase::Running => BackgroundCellPhase::Running,
        CommandCellPhase::Succeeded => BackgroundCellPhase::Succeeded,
        CommandCellPhase::Failed => BackgroundCellPhase::Failed,
        CommandCellPhase::Cancelled => BackgroundCellPhase::Cancelled,
        CommandCellPhase::LaunchFailed => BackgroundCellPhase::LaunchFailed,
    }
}

fn project_terminal_cause(
    cause: echo_agent::tools::cell::CommandCellTerminalCause,
) -> BackgroundCellTerminalCause {
    use echo_agent::tools::cell::CommandCellTerminalCause;
    match cause {
        CommandCellTerminalCause::Exited => BackgroundCellTerminalCause::Exited,
        CommandCellTerminalCause::TimedOut => BackgroundCellTerminalCause::TimedOut,
        CommandCellTerminalCause::Cancelled => BackgroundCellTerminalCause::Cancelled,
        CommandCellTerminalCause::LaunchFailed => BackgroundCellTerminalCause::LaunchFailed,
        CommandCellTerminalCause::WaitFailed => BackgroundCellTerminalCause::WaitFailed,
        CommandCellTerminalCause::OutputDrainFailed => {
            BackgroundCellTerminalCause::OutputDrainFailed
        }
    }
}

fn project_artifact_status(
    status: &echo_agent::tools::cell::CommandCellArtifactStatus,
) -> BackgroundCellArtifactStatus {
    use echo_agent::tools::cell::CommandCellArtifactStatus;
    match status {
        CommandCellArtifactStatus::NotRequested => BackgroundCellArtifactStatus::NotRequested,
        CommandCellArtifactStatus::Writing => BackgroundCellArtifactStatus::Writing,
        CommandCellArtifactStatus::BelowThreshold => BackgroundCellArtifactStatus::BelowThreshold,
        CommandCellArtifactStatus::Available => BackgroundCellArtifactStatus::Available,
        CommandCellArtifactStatus::Failed => BackgroundCellArtifactStatus::Failed,
    }
}

fn find_chat_cell_fact<'a>(
    events: &'a [crate::chat_event_log::ChatEventEnvelope],
    cell_id: &str,
    settled: bool,
) -> Option<&'a BackgroundCellState> {
    events.iter().find_map(|event| match &event.payload {
        crate::chat_driver::ChatDriverEvent::CommandCellStarted { cell }
            if !settled && cell.cell_id == cell_id =>
        {
            Some(cell.as_ref())
        }
        crate::chat_driver::ChatDriverEvent::CommandCellSettled { cell }
            if settled && cell.cell_id == cell_id =>
        {
            Some(cell.as_ref())
        }
        _ => None,
    })
}

fn push_tail(target: &mut String, chunk: &str, max_chars: usize) {
    target.push_str(chunk);
    if target.chars().count() <= max_chars {
        return;
    }
    let mut tail = target.chars().rev().take(max_chars).collect::<Vec<_>>();
    tail.reverse();
    *target = tail.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::cell::CommandCellOwner;

    use crate::tasks::task_runtime::compact_context::build_runtime_recovery_capsule;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, RuntimeEventKind, TaskRunStatus,
    };

    fn test_service(root: &std::path::Path) -> Result<Arc<CommandCellRuntimeService>, String> {
        let chat_events = crate::chat_event_log::ChatEventLog::open(
            root.join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Arc::new(CommandCellRuntimeService {
            inner: Arc::new(BackgroundCommandManager::default()),
            run_cells: RwLock::new(HashMap::new()),
            chat_cells: RwLock::new(HashMap::new()),
            stores_by_workspace: RwLock::new(HashMap::new()),
            projection_degraded: RwLock::new(HashMap::new()),
            governor: super::super::executor::process_execution_governor(),
            observers: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            chat_events: Arc::new(chat_events),
        }))
    }

    #[tokio::test]
    async fn task_runtime_store_fallback_records_one_start_and_one_finish() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cell-run",
                "workspace",
                "conversation",
                "message",
                DomainProfile::AiCoding,
                "run a background check",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cell-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "echo projected-cell-result".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("cell-run".to_string()),
                    turn_id: Some("turn-1".to_string()),
                    message_id: Some("message".to_string()),
                    execution_id: Some("execution-1".to_string()),
                    call_id: Some("call-1".to_string()),
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let cells = store.list_background_cells("cell-run").unwrap_or_default();
                if cells
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "cell projection did not reach terminal state".to_string())?;

        let events = store
            .list_events("cell-run", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellStarted)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellFinished)
                .count(),
            1
        );
        let capsule = build_runtime_recovery_capsule(&store, "cell-run")
            .ok_or_else(|| "cell result missing from recovery capsule".to_string())?;
        assert!(capsule.contains("projected-cell-result"));
        assert!(capsule.contains(&cell_id));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_run_cancel_stops_owned_cells_without_turn_token_coupling()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cancel-owned-cells",
                "workspace",
                "conversation",
                "message",
                DomainProfile::AiCoding,
                "cancel a background cell",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cancel-owned-cells", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "sleep 30".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("cancel-owned-cells".to_string()),
                    message_id: Some("message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        assert_eq!(service.stop_run("workspace", "cancel-owned-cells"), 1);
        let terminal = service
            .inner
            .wait(&cell_id, 0, 5_000)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            terminal.snapshot.phase,
            echo_agent::tools::cell::CommandCellPhase::Cancelled
        );
        Ok(())
    }

    #[tokio::test]
    async fn ordinary_chat_cell_uses_exact_conversation_and_root_message_journal()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::global(temp.path()),
            None,
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "printf ordinary-chat-result".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation-a".to_string()),
                    turn_id: Some("turn-a".to_string()),
                    message_id: Some("root-message-a".to_string()),
                    call_id: Some("call-a".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;

        let settled = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let replay = service
                    .chat_events
                    .replay("global", Some("conversation-a"), "root-message-a", 0)
                    .map_err(|error| error.to_string())?;
                if let Some(cell) = find_chat_cell_fact(&replay.events, &cell_id, true) {
                    return Ok::<BackgroundCellState, String>(cell.clone());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "ordinary Chat cell did not settle in its journal".to_string())??;
        assert_eq!(settled.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(
            settled.terminal_cause,
            Some(BackgroundCellTerminalCause::Exited)
        );
        assert_eq!(
            settled.artifact_status,
            BackgroundCellArtifactStatus::NotRequested
        );
        assert!(
            settled
                .output_excerpt
                .as_deref()
                .is_some_and(|output| output.contains("ordinary-chat-result"))
        );
        let wrong_conversation = service
            .chat_events
            .replay("global", Some("conversation-b"), "root-message-a", 0)
            .map_err(|error| error.to_string())?;
        assert!(wrong_conversation.events.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_run_ids_in_two_workspaces_cannot_cross_write() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = test_service(temp.path())?;
        let store_a = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks-a"), "workspace-a")
                .map_err(|error| error.to_string())?,
        );
        let store_b = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks-b"), "workspace-b")
                .map_err(|error| error.to_string())?,
        );
        for (store, workspace, conversation) in [
            (&store_a, "workspace-a", "conversation-a"),
            (&store_b, "workspace-b", "conversation-b"),
        ] {
            store
                .create_run(
                    "duplicate-run",
                    workspace,
                    conversation,
                    "root-message",
                    DomainProfile::AiCoding,
                    "verify exact scope",
                    "task",
                    AttendedMode::Attended,
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run("duplicate-run", TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
        }
        let workspace_a = crate::workspace::WorkspaceId::from_name("workspace-a");
        let registry_a = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_a, temp.path()),
            Some(store_a.clone()),
        );
        let workspace_b = crate::workspace::WorkspaceId::from_name("workspace-b");
        let _registry_b = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_b, temp.path()),
            Some(store_b.clone()),
        );
        let cell_id = registry_a
            .launch(CommandCellRequest {
                command: "printf workspace-a-only".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation-a".to_string()),
                    run_id: Some("duplicate-run".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store_a
                    .list_background_cells("duplicate-run")
                    .unwrap_or_default()
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "workspace A cell did not settle".to_string())?;
        assert!(
            store_b
                .list_background_cells("duplicate-run")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn started_append_failure_executes_no_process_and_leaves_no_active_cell()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "start-failure",
                "workspace",
                "conversation",
                "root-message",
                DomainProfile::AiCoding,
                "prove no side effect before Started",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("start-failure", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_next_cell_started_for_test();
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let side_effect = temp.path().join("must-not-exist");
        let result = registry
            .launch(CommandCellRequest {
                command: format!("touch {}", side_effect.display()),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("start-failure".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await;
        assert!(matches!(result, Err(CommandCellError::Runtime { .. })));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!side_effect.exists());
        assert!(
            store
                .list_background_cells("start-failure")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_persistence_failure_retains_owner_until_retry_succeeds() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(temp.path().join("tasks"), "workspace")
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "terminal-repair",
                "workspace",
                "conversation",
                "root-message",
                DomainProfile::AiCoding,
                "repair terminal persistence",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("terminal-repair", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_cell_terminal_writes_for_test(5);
        let service = test_service(temp.path())?;
        let workspace_id = crate::workspace::WorkspaceId::from_name("workspace");
        let registry = service.scoped(
            crate::workspace::WorkspaceExecutionScope::workspace(&workspace_id, temp.path()),
            Some(store.clone()),
        );
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "printf repaired".to_string(),
                owner: CommandCellOwner {
                    conversation_id: Some("conversation".to_string()),
                    run_id: Some("terminal-repair".to_string()),
                    message_id: Some("root-message".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map(|receipt| receipt.cell_id)
            .map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if service
                    .projection_diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.cell_id == cell_id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "projection degradation was not exposed".to_string())?;
        assert_eq!(
            service
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .filter(|cells| cells.contains(&cell_id))
                .count(),
            1
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store
                    .list_background_cells("terminal-repair")
                    .unwrap_or_default()
                    .iter()
                    .any(|cell| cell.cell_id == cell_id && !cell.is_active())
                    && service
                        .run_cells
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .values()
                        .all(|cells| !cells.contains(&cell_id))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| "terminal repair did not recover after the old retry window".to_string())?;
        assert!(service.projection_diagnostics().is_empty());
        assert!(
            service
                .run_cells
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .values()
                .all(|cells| !cells.contains(&cell_id))
        );
        Ok(())
    }
}
