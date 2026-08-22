//! LH0 source-reachability contracts for the long-horizon closure.
//!
//! These tests freeze the reviewed baseline without making the normal suite
//! red. Each repair slice replaces its matching failure assertion with a
//! reachability assertion for the corrected authority.

const APP_COMMAND_CELLS: &str = include_str!("command_cells.rs");
const APP_EXECUTOR: &str = include_str!("executor.rs");
const APP_FILE_STORE: &str = include_str!("file_store.rs");
const APP_INFRA: &str = include_str!("../../infra.rs");
const APP_SERVICE: &str = include_str!("../service.rs");
const APP_STORE: &str = include_str!("store.rs");
const APP_TYPES: &str = include_str!("types.rs");
const APP_TAURI: &str = include_str!("../../../../src/tauri/mod.rs");
const FRAMEWORK_AGENT_DISPATCH: &str =
    include_str!("../../../../../echo-agent/src/tools/builtin/agent_dispatch.rs");
const FRAMEWORK_CELL_CONTRACT: &str =
    include_str!("../../../../../echo-agent/echo-core/src/tools/cell.rs");
const FRAMEWORK_CELL_RUNTIME: &str =
    include_str!("../../../../../echo-agent/echo-orchestration/src/tasks/command_cell.rs");
const STORE_SOAK: &str = include_str!("../../../examples/task_runtime_soak.rs");

fn require(source: &str, needle: &str, failure: &str) -> Result<(), String> {
    if source.contains(needle) {
        Ok(())
    } else {
        Err(failure.to_string())
    }
}

fn require_absent(source: &str, needle: &str, failure: &str) -> Result<(), String> {
    if source.contains(needle) {
        Err(failure.to_string())
    } else {
        Ok(())
    }
}

fn ordered(source: &str, first: &str, second: &str, failure: &str) -> Result<(), String> {
    let first_index = source
        .find(first)
        .ok_or_else(|| format!("baseline marker missing: {first}"))?;
    let second_index = source
        .find(second)
        .ok_or_else(|| format!("baseline marker missing: {second}"))?;
    if first_index < second_index {
        Ok(())
    } else {
        Err(failure.to_string())
    }
}

fn between<'a>(source: &'a str, start: &str, end: &str, failure: &str) -> Result<&'a str, String> {
    let start_index = source
        .find(start)
        .ok_or_else(|| format!("baseline section start missing: {start}"))?;
    let rest = source
        .get(start_index..)
        .ok_or_else(|| failure.to_string())?;
    let end_offset = rest
        .find(end)
        .ok_or_else(|| format!("baseline section end missing: {end}"))?;
    rest.get(..end_offset).ok_or_else(|| failure.to_string())
}

#[test]
fn lh_f01_boot_resume_is_still_background_service_only() -> Result<(), String> {
    require(
        APP_SERVICE,
        ".filter(|run| run.conversation_id.starts_with(\"background:\"))",
        "LH-F01 baseline changed: background-only boot filter is no longer reachable",
    )
}

#[test]
fn lh_f02_watch_cell_still_routes_through_handle_dropping_agent_tool() -> Result<(), String> {
    require(
        APP_COMMAND_CELLS,
        ".execute_tool_with_context(\"agent_tool\", dispatch, context)",
        "LH-F02 baseline changed: watch_cell no longer delegates through agent_tool",
    )?;
    let background_branch = between(
        FRAMEWORK_AGENT_DISPATCH,
        "if run_background {",
        "} else {",
        "LH-F02 background agent_tool branch could not be isolated",
    )?;
    require(
        background_branch,
        "executor.dispatch_background(req).await",
        "LH-F02 baseline changed: agent_tool no longer dispatches a background handle",
    )?;
    require_absent(
        background_branch,
        ".join().await",
        "LH-F02 baseline changed: agent_tool now owns the background result",
    )
}

#[test]
fn lh_f03_tauri_still_suppresses_every_run_owned_framework_subagent() -> Result<(), String> {
    let predicate = between(
        APP_TAURI,
        "fn framework_subagent_event_needs_app_projection",
        "pub fn build_tauri_app",
        "LH-F03 Tauri projection predicate could not be isolated",
    )?;
    require(
        predicate,
        "run_id.is_none()",
        "LH-F03 baseline changed: run-owned events are no longer suppressed by the generic predicate",
    )
}

#[test]
fn lh_f04_terminal_projection_keeps_an_owned_repair_loop() -> Result<(), String> {
    require_absent(
        APP_COMMAND_CELLS,
        "for attempt in 1..=3_u8",
        "LH-F04 repair regressed: fixed terminal retry count returned",
    )?;
    require(
        APP_COMMAND_CELLS,
        "delay = delay.saturating_mul(2).min(Duration::from_secs(30))",
        "LH-F04 repair regressed: capped-backoff repair is missing",
    )?;
    ordered(
        APP_COMMAND_CELLS,
        "observe_terminal_cell(\n",
        "service.forget(&scope, &cell_id)",
        "LH-F04 repair regressed: ownership is released before terminal repair settles",
    )
}

#[test]
fn lh_f05_background_cell_projection_is_typed_and_complete() -> Result<(), String> {
    let state = between(
        APP_TYPES,
        "pub struct BackgroundCellState",
        "impl BackgroundCellState",
        "LH-F05 BackgroundCellState could not be isolated",
    )?;
    require(
        state,
        "pub phase: BackgroundCellPhase",
        "LH-F05 repair regressed: cell phase is not typed",
    )?;
    require(
        state,
        "terminal_cause",
        "LH-F05 repair regressed: terminal cause is not projected",
    )?;
    require(
        state,
        "artifact_status",
        "LH-F05 repair regressed: artifact status is not projected",
    )
}

#[test]
fn lh_f06_hot_state_still_bypasses_existing_checkpoint_read() -> Result<(), String> {
    let hot_read = between(
        APP_STORE,
        "pub fn get_run_state",
        "pub fn configure_run_continuation",
        "LH-F06 get_run_state could not be isolated",
    )?;
    require(
        hot_read,
        concat!("let events = self.", "list_", "events(run_id, 0)?;"),
        "LH-F06 baseline changed: get_run_state no longer performs full replay",
    )?;
    require(
        APP_FILE_STORE,
        "fn read_run_state_resilient",
        "LH-F06 duplicate-search authority disappeared: checkpoint-backed read is missing",
    )
}

#[test]
fn lh_f07_framework_now_publishes_before_start_supervision() -> Result<(), String> {
    require(
        FRAMEWORK_CELL_RUNTIME,
        "pub async fn prepare_launch",
        "LH-F07 repair regressed: prepared launch is missing",
    )?;
    ordered(
        FRAMEWORK_CELL_RUNTIME,
        "self.cells.insert(cell_id.clone(), handle.clone())",
        "self.tasks.spawn_on(",
        "LH-F07 repair regressed: supervision starts before registry publication",
    )
}

#[test]
fn lh_f08_historical_soak_still_exercises_only_the_store_core() -> Result<(), String> {
    require(
        STORE_SOAK,
        "TaskRuntimeStore",
        "LH-F08 historical soak no longer exercises TaskRuntimeStore",
    )?;
    require_absent(
        STORE_SOAK,
        "drive_chat",
        "LH-F08 baseline changed: historical soak now drives the real Agent path",
    )?;
    require_absent(
        STORE_SOAK,
        "watch_cell",
        "LH-F08 baseline changed: historical soak now drives Awaiter",
    )
}

#[test]
fn lh_f09_eko_persists_started_before_process_execution() -> Result<(), String> {
    let launch = between(
        APP_COMMAND_CELLS,
        "impl CommandCellRegistry for ScopedCommandCellRegistry",
        "fn wait(",
        "LH-F09 EKO launch adapter could not be isolated",
    )?;
    ordered(
        launch,
        "prepare_launch(request).await?",
        "store.record_background_cell_started(",
        "LH-F09 repair regressed: Started is attempted before reservation",
    )?;
    ordered(
        launch,
        "store.record_background_cell_started(",
        "start_prepared(reservation).await",
        "LH-F09 repair regressed: process execution can precede durable Started",
    )
}

#[test]
fn lh_f10_command_cells_share_the_process_shell_governor() -> Result<(), String> {
    require(
        APP_EXECUTOR,
        "static PROCESS_EXECUTION_GOVERNOR",
        "LH-F10 process governor authority is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "process_execution_governor()",
        "LH-F10 repair regressed: command cells bypass the process shell governor",
    )
}

#[test]
fn lh_f11_fast_model_still_rewrites_only_the_parent_model_name() -> Result<(), String> {
    require(
        APP_INFRA,
        "Some(\"fast\") => std::env::var(\"EKO_FAST_MODEL\")",
        "LH-F11 raw fast model alias is no longer reachable",
    )?;
    require(
        APP_INFRA,
        "config.model = model;",
        "LH-F11 baseline changed: fixed Subagent binding no longer rewrites the cloned config model",
    )
}

#[test]
fn lh_f12_observation_lease_now_spans_multiple_wait_rounds() -> Result<(), String> {
    require(
        FRAMEWORK_CELL_CONTRACT,
        "pub struct CommandCellObservationLease",
        "LH-F12 repair regressed: the cross-round observation lease is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "let observation = self.service.inner.observe(&cell_id)?;",
        "LH-F12 repair regressed: EKO terminal observer does not retain the cell",
    )
}

#[test]
fn lh_f13_owner_identity_routes_ordinary_chat_to_its_exact_journal() -> Result<(), String> {
    let owner = between(
        FRAMEWORK_CELL_CONTRACT,
        "pub struct CommandCellOwner",
        "pub struct CommandCellRequest",
        "LH-F13 CommandCellOwner could not be isolated",
    )?;
    require(
        owner,
        "conversation_id",
        "LH-F13 owner repair regressed: conversation identity is missing",
    )?;
    require(
        owner,
        "message_id",
        "LH-F13 owner repair regressed: root message identity is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "ordinary Chat cell requires conversation identity",
        "LH-F13 repair regressed: ordinary Chat admits missing conversation identity",
    )?;
    require(
        APP_COMMAND_CELLS,
        "ordinary Chat cell requires root message identity",
        "LH-F13 repair regressed: ordinary Chat admits missing root identity",
    )?;
    require(
        APP_COMMAND_CELLS,
        "append_chat_cell_fact",
        "LH-F13 repair regressed: ordinary Chat cell is not durably journaled",
    )
}
