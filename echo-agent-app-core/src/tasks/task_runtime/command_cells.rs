//! Thin EKO adapter over the framework command-cell registry.
//!
//! Process execution, output retention, waiting, cancellation and sandboxing
//! remain authoritative in `BackgroundCommandManager`. This adapter only
//! projects lifecycle facts into the owning TaskRuntime event stream.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, OnceLock, RwLock, Weak};
use std::time::Duration;

use echo_agent::sandbox::{SandboxExecutor, SandboxManager};
use echo_agent::tasks::{BackgroundCommandManager, BackgroundCommandManagerConfig};
use echo_agent::tools::cell::{
    CommandCellDelta, CommandCellRegistry, CommandCellRequest, CommandCellSnapshot,
};
use echo_agent::tools::{Tool, ToolContext, ToolParameters, ToolResult};
use futures::future::BoxFuture;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::compact_context::task_runtime_projection_registry;
use super::store::TaskRuntimeStore;

const OBSERVER_YIELD_MS: u64 = 30_000;
const OUTPUT_EXCERPT_CHARS: usize = 1_000;

static TASK_RUNTIME_STORES: LazyLock<RwLock<Vec<Weak<TaskRuntimeStore>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Register a store without extending its lifetime. Background TaskRuns can
/// outlive the chat-turn projection registration that originally dispatched
/// them, so cell event persistence needs a process-level lookup fallback.
pub(crate) fn register_task_runtime_store(store: &Arc<TaskRuntimeStore>) {
    let mut stores = TASK_RUNTIME_STORES
        .write()
        .unwrap_or_else(|error| error.into_inner());
    stores.retain(|candidate| candidate.strong_count() > 0);
    if stores
        .iter()
        .filter_map(Weak::upgrade)
        .any(|candidate| Arc::ptr_eq(&candidate, store))
    {
        return;
    }
    stores.push(Arc::downgrade(store));
}

fn store_for_run(run_id: &str) -> Option<Arc<TaskRuntimeStore>> {
    if let Some(store) = task_runtime_projection_registry().store_for_run(run_id) {
        return Some(store);
    }
    let candidates = TASK_RUNTIME_STORES
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    candidates
        .into_iter()
        .find(|store| store.get_run(run_id).ok().flatten().is_some())
}

pub(crate) struct EkoCommandCellRegistry {
    inner: Arc<BackgroundCommandManager>,
    cells_by_run: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}

impl EkoCommandCellRegistry {
    fn new(sandbox: Arc<SandboxManager>) -> Self {
        let executor: Arc<dyn SandboxExecutor> = sandbox;
        Self {
            inner: Arc::new(BackgroundCommandManager::new_with_sandbox(
                BackgroundCommandManagerConfig::default(),
                executor,
            )),
            cells_by_run: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn track(&self, run_id: &str, cell_id: &str) {
        self.cells_by_run
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(run_id.to_string())
            .or_default()
            .insert(cell_id.to_string());
    }

    fn forget(&self, run_id: &str, cell_id: &str) {
        let mut cells_by_run = self
            .cells_by_run
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let remove_run = cells_by_run.get_mut(run_id).is_some_and(|cells| {
            cells.remove(cell_id);
            cells.is_empty()
        });
        if remove_run {
            cells_by_run.remove(run_id);
        }
    }

    fn stop_run(&self, run_id: &str) -> usize {
        let cells = self
            .cells_by_run
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(run_id)
            .unwrap_or_default();
        cells
            .into_iter()
            .filter(|cell_id| self.inner.stop(cell_id))
            .count()
    }
}

static SHARED_COMMAND_CELLS: OnceLock<Arc<EkoCommandCellRegistry>> = OnceLock::new();

/// One process-scoped registry shared by every primary/subagent generation.
/// Model or workspace rebinding must not invalidate already-running cells.
pub(crate) fn shared_command_cells(sandbox: Arc<SandboxManager>) -> Arc<dyn CommandCellRegistry> {
    SHARED_COMMAND_CELLS
        .get_or_init(|| Arc::new(EkoCommandCellRegistry::new(sandbox)))
        .clone()
}

/// Explicit TaskRuntime cancellation kills every process-scoped cell owned by
/// the run. Pause intentionally does not call this path.
pub(crate) fn stop_cells_for_run(run_id: &str) -> usize {
    SHARED_COMMAND_CELLS
        .get()
        .map(|registry| registry.stop_run(run_id))
        .unwrap_or(0)
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
                    message: error,
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

impl CommandCellRegistry for EkoCommandCellRegistry {
    fn launch(&self, request: CommandCellRequest) -> Result<String, String> {
        let owner = request.owner.clone();
        let name = request.command.chars().take(80).collect::<String>();
        let command_hash = format!("{:x}", Sha256::digest(request.command.as_bytes()));
        let store = owner.run_id.as_deref().and_then(store_for_run);
        let cell_id = self.inner.launch(request)?;

        if let Some(run_id) = owner.run_id.as_deref() {
            self.track(run_id, &cell_id);
        }

        if let (Some(run_id), Some(store)) = (owner.run_id.clone(), store) {
            if let Err(error) = store.record_background_cell_started(
                &run_id,
                &cell_id,
                &name,
                &command_hash,
                owner.turn_id.as_deref(),
                owner.execution_id.as_deref(),
                owner.call_id.as_deref(),
            ) {
                self.inner.stop(&cell_id);
                self.forget(&run_id, &cell_id);
                return Err(format!(
                    "cell launched but TaskRuntime start event could not be persisted: {error}"
                ));
            }
            let registry = Arc::clone(&self.inner);
            let cells_by_run = Arc::clone(&self.cells_by_run);
            let observed_cell_id = cell_id.clone();
            tokio::spawn(async move {
                let tracked_run_id = run_id.clone();
                let tracked_cell_id = observed_cell_id.clone();
                observe_terminal_cell(
                    registry,
                    store,
                    run_id,
                    observed_cell_id,
                    name,
                    owner.call_id,
                )
                .await;
                forget_cell(&cells_by_run, &tracked_run_id, &tracked_cell_id);
            });
        }

        Ok(cell_id)
    }

    fn wait(
        &self,
        cell_id: &str,
        cursor: u64,
        yield_ms: u64,
    ) -> BoxFuture<'_, Result<CommandCellDelta, String>> {
        self.inner.wait(cell_id, cursor, yield_ms)
    }

    fn stop(&self, cell_id: &str) -> bool {
        self.inner.stop(cell_id)
    }

    fn list(&self) -> BoxFuture<'_, Vec<CommandCellSnapshot>> {
        self.inner.list()
    }
}

fn forget_cell(
    cells_by_run: &RwLock<HashMap<String, HashSet<String>>>,
    run_id: &str,
    cell_id: &str,
) {
    let mut cells_by_run = cells_by_run
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let remove_run = cells_by_run.get_mut(run_id).is_some_and(|cells| {
        cells.remove(cell_id);
        cells.is_empty()
    });
    if remove_run {
        cells_by_run.remove(run_id);
    }
}

async fn observe_terminal_cell(
    registry: Arc<BackgroundCommandManager>,
    store: Arc<TaskRuntimeStore>,
    run_id: String,
    cell_id: String,
    name: String,
    call_id: Option<String>,
) {
    let mut cursor = 0_u64;
    let mut excerpt = String::new();
    loop {
        let delta = match registry.wait(&cell_id, cursor, OBSERVER_YIELD_MS).await {
            Ok(delta) => delta,
            Err(error) => {
                let persisted = persist_terminal_with_retry(
                    &store,
                    &run_id,
                    &cell_id,
                    &name,
                    "observer_failed",
                    None,
                    0,
                    false,
                    Some(&error),
                    None,
                    None,
                    call_id.as_deref(),
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
            delta.snapshot.phase.as_str(),
            delta.snapshot.exit_code,
            delta.snapshot.total_output_bytes,
            delta.snapshot.output_truncated,
            (!excerpt.is_empty()).then_some(excerpt.as_str()),
            artifact_path.as_deref(),
            artifact_sha256.as_deref(),
            call_id.as_deref(),
        )
        .await;
        if persisted {
            super::continuation::wake_after_cell_terminal(&store, &run_id);
        }
        return;
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_terminal_with_retry(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    cell_id: &str,
    name: &str,
    phase: &str,
    exit_code: Option<i32>,
    total_output_bytes: u64,
    output_truncated: bool,
    output_excerpt: Option<&str>,
    artifact_path: Option<&str>,
    artifact_sha256: Option<&str>,
    call_id: Option<&str>,
) -> bool {
    let mut delay = Duration::from_millis(50);
    for attempt in 1..=3_u8 {
        match store.record_background_cell_finished(
            run_id,
            cell_id,
            name,
            phase,
            exit_code,
            total_output_bytes,
            output_truncated,
            output_excerpt,
            artifact_path,
            artifact_sha256,
            call_id,
        ) {
            Ok(()) => return true,
            Err(error) if attempt < 3 => {
                tracing::warn!(
                    run_id,
                    cell_id,
                    attempt,
                    %error,
                    "retrying terminal command-cell event persistence"
                );
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(4);
            }
            Err(error) => {
                tracing::error!(
                    run_id,
                    cell_id,
                    %error,
                    "terminal command-cell event could not be persisted"
                );
            }
        }
    }
    false
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

    #[tokio::test]
    async fn task_runtime_store_fallback_records_one_start_and_one_finish() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
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
        register_task_runtime_store(&store);
        let registry = EkoCommandCellRegistry {
            inner: Arc::new(BackgroundCommandManager::default()),
            cells_by_run: Arc::new(RwLock::new(HashMap::new())),
        };
        let cell_id = registry
            .launch(CommandCellRequest {
                command: "echo projected-cell-result".to_string(),
                owner: CommandCellOwner {
                    run_id: Some("cell-run".to_string()),
                    turn_id: Some("turn-1".to_string()),
                    execution_id: Some("execution-1".to_string()),
                    call_id: Some("call-1".to_string()),
                },
                ..Default::default()
            })
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
        let registry = EkoCommandCellRegistry {
            inner: Arc::new(BackgroundCommandManager::default()),
            cells_by_run: Arc::new(RwLock::new(HashMap::new())),
        };
        let cell_id = registry.launch(CommandCellRequest {
            command: "sleep 30".to_string(),
            owner: CommandCellOwner {
                run_id: Some("cancel-owned-cells".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })?;

        assert_eq!(registry.stop_run("cancel-owned-cells"), 1);
        let terminal = registry.inner.wait(&cell_id, 0, 5_000).await?;
        assert_eq!(
            terminal.snapshot.phase,
            echo_agent::tools::cell::CommandCellPhase::Cancelled
        );
        Ok(())
    }
}
