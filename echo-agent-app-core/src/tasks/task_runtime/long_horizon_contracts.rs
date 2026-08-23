//! LH0 source-reachability contracts for the long-horizon closure.
//!
//! These tests freeze the reviewed baseline without making the normal suite
//! red. Each repair slice replaces its matching failure assertion with a
//! reachability assertion for the corrected authority.

const APP_COMMAND_CELLS: &str = include_str!("command_cells.rs");
const APP_AGENT_POOL: &str = include_str!("../../agent_pool.rs");
const APP_CHAT_DRIVER: &str = include_str!("../../chat_driver.rs");
const APP_EXECUTOR: &str = include_str!("executor.rs");
const APP_FILE_STORE: &str = include_str!("file_store.rs");
const APP_COMPLETION_GATE: &str = include_str!("completion_gate.rs");
const APP_INFRA: &str = include_str!("../../infra.rs");
const APP_STATE: &str = include_str!("../../state.rs");
const APP_STORE: &str = include_str!("store.rs");
const APP_SUBAGENT_CONTROL: &str = include_str!("subagent_control.rs");
const APP_TYPES: &str = include_str!("types.rs");
const APP_TAURI: &str = include_str!("../../../../src/tauri/mod.rs");
const FRAMEWORK_CELL_CONTRACT: &str =
    include_str!("../../../../../echo-agent/echo-core/src/tools/cell.rs");
const FRAMEWORK_CELL_RUNTIME: &str =
    include_str!("../../../../../echo-agent/echo-orchestration/src/tasks/command_cell.rs");
const STORE_SOAK: &str = include_str!("../../../examples/task_runtime_soak.rs");
const LH6_CONCURRENCY_SOAK: &str = include_str!("../../../examples/lh6_concurrency_soak.rs");
const LH6_PRODUCT_SOAK: &str = include_str!("../../../../examples/lh6_product_soak.rs");
const FRONTEND_CHAT_CONTRACT: &str =
    include_str!("../../../../web-frontend/src/hooks/chatEventHandler.contract.test.ts");

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
fn lh_f01_boot_resume_accepts_normal_conversation_runs() -> Result<(), String> {
    let reconciler = between(
        APP_STATE,
        "async fn reconcile_task_runs_at_boot",
        "/// 启动后台任务服务",
        "LH-F01 app-core boot reconciler could not be isolated",
    )?;
    require_absent(
        reconciler,
        "starts_with(\"background:\")",
        "LH-F01 repair regressed: app boot resume is restricted to background conversations",
    )?;
    require(
        APP_STATE,
        "reconcile_task_runs_at_boot",
        "LH-F01 repair regressed: app-core boot reconciler is not wired",
    )?;
    require(
        include_str!("boot_reconciler.rs"),
        "pub struct TaskRunBootReconciler",
        "LH-F01 repair regressed: store-scoped boot owner is missing",
    )
}

#[test]
fn lh_f02_watch_cell_owns_the_direct_controlled_background_handle() -> Result<(), String> {
    require_absent(
        APP_COMMAND_CELLS,
        ".execute_tool_with_context(\"agent_tool\", dispatch, context)",
        "LH-F02 repair regressed: watch_cell delegates through handle-dropping agent_tool",
    )?;
    require(
        APP_COMMAND_CELLS,
        ".dispatch_background_attempt(request, identity)",
        "LH-F02 repair regressed: exact controlled dispatch is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "let join_result = handle.join().await;",
        "LH-F02 repair regressed: app-core does not own Awaiter settlement",
    )
}

#[test]
fn lh_f03_tauri_excludes_awaiter_and_formal_subagents_from_generic_projection() -> Result<(), String>
{
    let predicate = between(
        APP_TAURI,
        "fn framework_subagent_event_needs_app_projection",
        "pub fn build_tauri_app",
        "LH-F03 Tauri projection predicate could not be isolated",
    )?;
    require(
        predicate,
        "run_id.is_none()",
        "LH-F03 repair regressed: formal TaskRuntime events reach the generic bridge",
    )?;
    require(
        predicate,
        "agent != \"awaiter\"",
        "LH-F03 repair regressed: Awaiter reaches the generic Tauri bridge",
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
fn lh_f06_hot_state_uses_the_existing_checkpoint_read_authority() -> Result<(), String> {
    let hot_read = between(
        APP_STORE,
        "pub fn get_run_state",
        "pub fn configure_run_continuation",
        "LH-F06 get_run_state could not be isolated",
    )?;
    require_absent(
        hot_read,
        concat!("let events = self.", "list_", "events(run_id, 0)?;"),
        "LH-F06 repair regressed: get_run_state performs full replay",
    )?;
    require(
        APP_FILE_STORE,
        "pub fn get_run_state",
        "LH-F06 repair regressed: FileTaskStore wrapper is missing",
    )?;
    require(
        hot_read,
        ".get_run_state(run_id)",
        "LH-F06 repair regressed: TaskRuntimeStore bypasses FileTaskStore",
    )?;
    let cell_read = between(
        APP_STORE,
        "pub fn list_background_cells",
        "pub fn record_background_cell_started",
        "LH-F06 background cell read could not be isolated",
    )?;
    require_absent(
        cell_read,
        "list_events(run_id, 0)",
        "LH-F06 repair regressed: background cells perform full replay",
    )
}

#[test]
fn lh5_full_scan_allowlist_is_explicit_and_bounded() -> Result<(), String> {
    let production_store = APP_STORE
        .split("// The compile-time test that proves the transaction invariant:")
        .next()
        .ok_or_else(|| "store production section missing".to_string())?;
    let production_control = APP_SUBAGENT_CONTROL
        .split("#[cfg(test)]")
        .next()
        .ok_or_else(|| "Subagent control production section missing".to_string())?;
    let production_completion = APP_COMPLETION_GATE
        .split("#[cfg(test)]")
        .next()
        .ok_or_else(|| "completion production section missing".to_string())?;
    let scans = production_store.matches("list_events(run_id, 0)").count()
        + production_control
            .matches("list_events(&target.run_id, 0)")
            .count()
        + production_control
            .matches("list_events(&identity.run_id, 0)")
            .count()
        + production_completion
            .matches("list_events(run_id, 0)")
            .count();
    if scans != 7 {
        return Err(format!(
            "LH5 full-scan allowlist changed without review: expected 7, found {scans}"
        ));
    }
    let comments = production_store.matches("Audit allowlist:").count()
        + production_control.matches("Audit allowlist:").count()
        + production_completion.matches("Audit allowlist:").count();
    if comments != scans {
        return Err(format!(
            "LH5 full-scan comments do not cover the allowlist: {comments}/{scans}"
        ));
    }
    Ok(())
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
    )?;
    require(
        APP_COMMAND_CELLS,
        "self.governor.subagent_semaphore().acquire_owned()",
        "LH-F10 repair regressed: Awaiter bypasses the process Subagent governor",
    )
}

#[test]
fn lh_f11_fast_model_resolves_one_complete_configured_profile() -> Result<(), String> {
    require_absent(
        APP_INFRA,
        "config.model = model;",
        "LH-F11 repair regressed: fixed role rewrites only the parent model name",
    )?;
    require(
        APP_INFRA,
        "model_config::resolve_runtime_model_selector(app_config, Some(&selector))",
        "LH-F11 repair regressed: configured model profile resolver is bypassed",
    )?;
    require(
        APP_INFRA,
        "prepare_runtime_llm(&runtime)",
        "LH-F11 repair regressed: resolved Provider profile is not prepared as one generation",
    )?;
    require(
        APP_INFRA,
        "ShellTool::new_permissive()",
        "LH6 repair regressed: EKO still applies a fixed shell whitelist after PermissionService",
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

#[test]
fn lh6_fault_matrix_has_automated_evidence_for_every_row() -> Result<(), String> {
    let cases = [
        (
            "publication race",
            FRAMEWORK_CELL_RUNTIME,
            "launch_publishes_handle_before_fast_terminal_settlement",
        ),
        (
            "Started append failure",
            APP_COMMAND_CELLS,
            "started_append_failure_executes_no_process_and_leaves_no_active_cell",
        ),
        (
            "Started projection failure",
            APP_COMMAND_CELLS,
            "committed_start_with_degraded_projection_aborts_and_repairs_terminal",
        ),
        (
            "tracked capacity",
            FRAMEWORK_CELL_RUNTIME,
            "total_tracked_capacity_backpressures_prepared_launches",
        ),
        (
            "UTF-8 pipe split",
            FRAMEWORK_CELL_RUNTIME,
            "artifact_decoder_preserves_utf8_split_across_pipe_reads",
        ),
        (
            "artifact writer failure",
            FRAMEWORK_CELL_RUNTIME,
            "artifact_write_failure_has_typed_status",
        ),
        (
            "artifact finalizer deadline",
            FRAMEWORK_CELL_RUNTIME,
            "shutdown_aborts_blocking_artifact_finalizer_at_deadline",
        ),
        (
            "terminal append retry",
            APP_COMMAND_CELLS,
            "terminal_persistence_failure_retains_owner_until_retry_succeeds",
        ),
        (
            "receiver lag replay",
            FRONTEND_CHAT_CONTRACT,
            "renders runtime cell truth from an Awaiter Ready fact after turn settlement",
        ),
        (
            "Awaiter Provider failure",
            APP_COMMAND_CELLS,
            "awaiter_provider_failure_preserves_cell_truth",
        ),
        (
            "complete fast Provider profile",
            APP_INFRA,
            "configured_subagent_selector_resolves_the_complete_profile",
        ),
        (
            "three-workspace shared governor",
            LH6_CONCURRENCY_SOAK,
            "const WORKSPACE_COUNT: usize = 3",
        ),
        (
            "main turn before Awaiter",
            FRONTEND_CHAT_CONTRACT,
            "after its foreground turn is terminal",
        ),
        (
            "process restart cell closure",
            APP_STORE,
            "boot_recovery_closes_orphan_cell_without_replaying_it",
        ),
        (
            "Provider retry",
            APP_CHAT_DRIVER,
            "typed_retryable_llm_failure_schedules_without_persisting_provider_message",
        ),
        (
            "checkpoint corruption",
            APP_STORE,
            "checkpoint_state_is_canonical_byte_equivalent_to_full_replay_and_repairs_corruption",
        ),
        (
            "workspace corruption isolation",
            APP_STATE,
            "corrupt_workspace_does_not_block_healthy_global_boot_recovery",
        ),
        (
            "committed projection degradation",
            APP_COMMAND_CELLS,
            "cell start committed but projection degraded",
        ),
    ];
    if cases.len() != 18 {
        return Err(format!(
            "LH6 fault matrix expected 18 rows, found {}",
            cases.len()
        ));
    }
    for (name, source, evidence) in cases {
        require(
            source,
            evidence,
            &format!("LH6 fault matrix lost automated evidence for {name}"),
        )?;
    }
    Ok(())
}

#[test]
fn lh6_soaks_use_real_product_authorities_and_self_retire() -> Result<(), String> {
    for (source, name) in [
        (LH6_CONCURRENCY_SOAK, "concurrency"),
        (LH6_PRODUCT_SOAK, "real-product"),
    ] {
        require(
            source,
            "\"passed\".to_string()",
            &format!("LH6 {name} soak does not self-retire as passed"),
        )?;
        require_absent(
            source,
            "loop { std::process::Command",
            &format!("LH6 {name} soak contains a binary restart loop"),
        )?;
    }
    require(
        LH6_PRODUCT_SOAK,
        "drive_foreground_chat(",
        "LH6 product soak bypasses the real foreground Agent driver",
    )?;
    require(
        LH6_PRODUCT_SOAK,
        "ChatSurface::Gui",
        "LH6 product soak omits the GUI surface",
    )?;
    require(
        LH6_PRODUCT_SOAK,
        "HumanLoopRequest::input",
        "LH6 product soak omits HITL",
    )?;
    require(
        LH6_PRODUCT_SOAK,
        "watch_cell",
        "LH6 product soak omits Awaiter dispatch",
    )?;
    require(
        APP_AGENT_POOL,
        "PROCESS_AGENT_EXECUTION",
        "LH6 product soak cannot enforce a process Agent execution bound",
    )
}
