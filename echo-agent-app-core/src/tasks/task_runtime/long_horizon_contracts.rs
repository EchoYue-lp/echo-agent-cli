//! LH0 source-reachability contracts for the long-horizon closure.
//!
//! These tests freeze the reviewed baseline without making the normal suite
//! red. Each repair slice replaces its matching failure assertion with a
//! reachability assertion for the corrected authority.

const APP_COMMAND_CELLS: &str = include_str!("command_cells.rs");
const APP_CONTINUATION: &str = include_str!("continuation.rs");
const APP_COMPACT_CONTEXT: &str = include_str!("compact_context.rs");
const APP_TASK_TOOLS: &str = include_str!("task_tools.rs");
const APP_CONVERSATION_DELETION: &str = include_str!("../../conversation_deletion.rs");
const APP_AGENT_POOL: &str = include_str!("../../agent_pool.rs");
const APP_AGENT_ROUTER: &str = include_str!("../../agent_router.rs");
const APP_CHAT_DRIVER: &str = include_str!("../../chat_driver.rs");
const APP_TURN_CONTEXT: &str = include_str!("../../turn_context.rs");
const APP_MANUAL_COMPRESSION: &str = include_str!("../../manual_compression.rs");
const APP_PRODUCT_DATA_IO: &str = include_str!("../../product_data_io.rs");
const APP_ANALYSIS_RUNTIME: &str = include_str!("../../analysis_runtime.rs");
const APP_ANALYSIS: &str = include_str!("../../analysis.rs");
const APP_RESEARCH_CONNECTORS: &str = include_str!("../../research_connectors.rs");
const APP_RESEARCH_TOOL: &str = include_str!("../../research_tool.rs");
const APP_RUNTIME: &str = include_str!("../../runtime.rs");
const APP_WORKSPACE_RUNTIME: &str = include_str!("../../workspace/runtime.rs");
const APP_EXECUTOR: &str = include_str!("executor.rs");
const APP_BOOT_RECONCILER: &str = include_str!("boot_reconciler.rs");
const APP_TASK_RUNTIME_MOD: &str = include_str!("mod.rs");
const APP_FILE_SHADOW: &str = include_str!("file_shadow.rs");
const APP_FILE_STORE: &str = include_str!("file_store.rs");
const APP_COMPLETION_GATE: &str = include_str!("completion_gate.rs");
const APP_INFRA: &str = include_str!("../../infra.rs");
const APP_STATE: &str = include_str!("../../state.rs");
const APP_TASK_SERVICE: &str = include_str!("../service.rs");
const APP_STORE: &str = include_str!("store.rs");
const APP_SUBAGENT_CONTROL: &str = include_str!("subagent_control.rs");
const APP_REVISION_ADAPTER: &str = include_str!("revisioned_adapter.rs");
const APP_TYPES: &str = include_str!("types.rs");
const APP_TAURI: &str = include_str!("../../../../src/tauri/mod.rs");
const APP_TAURI_TASK_RUNTIME: &str = include_str!("../../../../src/tauri/commands/task_runtime.rs");
const APP_TAURI_TASKS: &str = include_str!("../../../../src/tauri/commands/tasks.rs");
const APP_TAURI_CHAT: &str = include_str!("../../../../src/tauri/commands/chat.rs");
const APP_TAURI_FILES: &str = include_str!("../../../../src/tauri/commands/files.rs");
const APP_TAURI_RESEARCH: &str = include_str!("../../../../src/tauri/commands/research.rs");
const APP_TAURI_WORKSPACE: &str = include_str!("../../../../src/tauri/commands/workspace.rs");
const APP_TUI_EVENTS: &str = include_str!("../../../../src/tui/events.rs");
const APP_CLI_TASKS: &str = include_str!("../../../../src/cli/cmd_impls/tasks_ext.rs");
const APP_CLI_REPL: &str = include_str!("../../../../src/cli/repl.rs");
const APP_CHANNELS: &str = include_str!("../../../../src/cli/channels.rs");
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

fn before_test_module(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests {")
        .map_or(source, |(production, _)| production)
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
    require(
        reconciler,
        "run.conversation_id.starts_with(\"background:\")",
        "LH-F01 repair regressed: global background launcher ownership is not separated",
    )?;
    require(
        APP_STATE,
        "reconcile_task_runs_at_boot",
        "LH-F01 repair regressed: app-core boot reconciler is not wired",
    )?;
    require_absent(
        reconciler,
        "transition.write().await",
        "LH-F01 boot recovery holds workspace transition admission across recovery",
    )?;
    require(
        include_str!("boot_reconciler.rs"),
        "pub struct TaskRunBootReconciler",
        "LH-F01 repair regressed: store-scoped boot owner is missing",
    )
}

#[test]
fn boot_and_inbox_authorities_remain_cancellation_safe_and_bounded() -> Result<(), String> {
    require(
        include_str!("boot_reconciler.rs"),
        "tokio::sync::watch::channel(None)",
        "boot recovery no longer owns a cancellation-safe singleflight receipt",
    )?;
    require(
        APP_AGENT_ROUTER,
        "CheckpointedReducer<SegmentedFileEventJournal<AgentInboxEvent>",
        "AgentRouter no longer uses the framework checkpointed journal authority",
    )?;
    require(
        APP_AGENT_ROUTER,
        "AgentDeliveryStatus::InjectionStarted",
        "Agent delivery lost its pre-side-effect durable boundary",
    )?;
    require_absent(
        APP_AGENT_ROUTER,
        "fn read_events(",
        "AgentRouter regressed to full journal replay per operation",
    )?;
    require(
        APP_WORKSPACE_RUNTIME,
        "run(\"prepare workspace runtime file stores\"",
        "workspace file resource preparation returned to a Tokio executor thread",
    )?;
    require(
        APP_COMMAND_CELLS,
        "AwaiterResultDeliveryStarted",
        "Awaiter lost its no-duplicate delivery boundary",
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
fn lh_f04_terminal_projection_keeps_bounded_owned_repair_debt() -> Result<(), String> {
    require_absent(
        APP_COMMAND_CELLS,
        "for attempt in 1..=3_u8",
        "LH-F04 repair regressed: fixed terminal retry count returned",
    )?;
    require(
        APP_COMMAND_CELLS,
        "const MAX_PROJECTION_REPAIR_ATTEMPTS: u64 = 8",
        "LH-F04 repair regressed: bounded repair budget is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "delay = delay.saturating_mul(2).min(Duration::from_secs(1))",
        "LH-F04 repair regressed: capped-backoff repair is missing",
    )?;
    require(
        APP_COMMAND_CELLS,
        "record_lifecycle_debt(",
        "LH-F04 repair regressed: exhausted terminal repair is not lifecycle debt",
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
        "pub(crate) fn get_run_state",
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
fn lh_f06_raw_file_authority_is_not_a_public_bypass() -> Result<(), String> {
    require(
        APP_TASK_RUNTIME_MOD,
        "pub(crate) mod file_shadow;",
        "LH-F06 raw shadow module became externally public",
    )?;
    require(
        APP_TASK_RUNTIME_MOD,
        "pub(crate) mod file_store;",
        "LH-F06 raw file store module became externally public",
    )?;
    require(
        APP_FILE_SHADOW,
        "pub(crate) struct FileTaskShadow",
        "LH-F06 FileTaskShadow visibility widened",
    )?;
    require(
        APP_FILE_SHADOW,
        "pub(crate) fn append_event_line",
        "LH-F06 raw append bypass visibility widened",
    )
}

#[test]
fn lh5_full_scan_allowlist_is_explicit_and_bounded() -> Result<(), String> {
    let production_store = APP_STORE
        .split("// The compile-time test that proves the transaction invariant:")
        .next()
        .ok_or_else(|| "store production section missing".to_string())?;
    let production_control = before_test_module(APP_SUBAGENT_CONTROL);
    let production_completion = before_test_module(APP_COMPLETION_GATE);
    let scans = production_store.matches("list_events(run_id, 0)").count()
        + production_store
            .matches("let after_sequence = i64::try_from(expected.journal_sequence)")
            .count()
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
fn taskruntime_mutation_projection_refresh_has_one_typed_owner() -> Result<(), String> {
    let production_store = APP_STORE
        .split("// The compile-time test that proves the transaction invariant:")
        .next()
        .ok_or_else(|| "store production section missing".to_string())?;
    for required in [
        "fn commit_runtime_event",
        "fn commit_runtime_events",
        "fn commit_runtime_events_with_receipt",
        "fn refresh_committed_projection",
        "ProjectionCommitReceipt::CommittedProjectionDegraded",
    ] {
        require(
            production_store,
            required,
            "TaskRuntime lost its typed mutation projection owner",
        )?;
    }
    let refreshes = production_store.matches("rewrite_plan(").count();
    if refreshes != 2 {
        return Err(format!(
            "TaskRuntime production rewrite_plan ownership changed: expected typed owner + boot repair, found {refreshes}"
        ));
    }
    Ok(())
}

#[test]
fn task_runtime_async_boundaries_keep_file_io_behind_the_bounded_adapter() -> Result<(), String> {
    let chat_preparation = between(
        APP_CHAT_DRIVER,
        "async fn prepare_chat_execution",
        "async fn drive_prepared_chat",
        "chat preparation boundary could not be isolated",
    )?;
    require(
        chat_preparation,
        "run_owned(\"prepare and claim chat TaskRun\"",
        "chat preparation bypasses the bounded TaskRuntime adapter",
    )?;

    let background_submit = between(
        APP_TASK_SERVICE,
        "async fn submit_prompt_run",
        "pub async fn submit_dag",
        "background submit boundary could not be isolated",
    )?;
    require(
        background_submit,
        "run_owned(\"prepare background TaskRun\"",
        "background submit bypasses the bounded TaskRuntime adapter",
    )?;
    require(
        APP_TASK_SERVICE,
        "run_store(\"cancel background TaskRun\"",
        "background cancellation runs file I/O on a Tokio async executor thread",
    )?;
    require(
        APP_TASK_SERVICE,
        "run_store(\"load pending background TaskRuns\"",
        "background recovery discovery runs file I/O on a Tokio async executor thread",
    )?;
    let dependency_poll = between(
        APP_TASK_SERVICE,
        "async fn wait_for_dependencies",
        "fn transition_to_running",
        "background dependency boundary could not be isolated",
    )?;
    require(
        dependency_poll,
        "run_store(\"poll background dependencies\"",
        "background dependency polling runs file I/O on a Tokio async executor thread",
    )?;

    require(
        APP_REVISION_ADAPTER,
        "blocking: TaskRuntimeBlockingAdapter",
        "revision adapter lost its bounded file-I/O owner",
    )?;
    for (source, operation) in [
        (APP_TASK_SERVICE, "prepare background TaskRun DAG"),
        (APP_TASK_SERVICE, "pause background TaskRun"),
        (APP_TASK_SERVICE, "load background recovery blockers"),
        (APP_TASK_SERVICE, "resolve background recovery task"),
        (APP_TASK_SERVICE, "list background TaskRuns"),
        (APP_TASK_SERVICE, "load background TaskRun progress"),
        (APP_BOOT_RECONCILER, "recover TaskRuntime at boot"),
        (APP_BOOT_RECONCILER, "list boot recovery candidates"),
        (APP_BOOT_RECONCILER, "decide boot auto-resume admission"),
        (APP_BOOT_RECONCILER, "commit boot auto-resume admission"),
        (APP_TUI_EVENTS, "resolve TUI TaskRun"),
        (APP_TUI_EVENTS, "refresh TUI TaskRuntime projection"),
        (APP_TUI_EVENTS, "resolve TUI recovery task"),
        (APP_TUI_EVENTS, "list TUI unattended worktrees"),
        (APP_CLI_TASKS, "load CLI TaskRun control"),
        (APP_CLI_TASKS, "update CLI TaskRun Goal"),
        (APP_CHANNELS, "load channel TaskRun"),
        (APP_CHANNELS, "update channel TaskRun Goal"),
        (APP_CHAT_DRIVER, "observe chat execution path"),
        (APP_CHAT_DRIVER, "resolve previous continuation driver"),
        (APP_CHAT_DRIVER, "project EKO turn TaskRuntime event"),
        (APP_SUBAGENT_CONTROL, "deliver live Subagent guidance"),
        (APP_SUBAGENT_CONTROL, "interrupt exact Subagent attempt"),
        (APP_COMMAND_CELLS, "record command-cell start"),
        (APP_COMMAND_CELLS, "record command-cell terminal"),
        (APP_COMMAND_CELLS, "observe command-cell terminal"),
        (
            APP_COMMAND_CELLS,
            "observe ordinary-chat command-cell terminal",
        ),
        (APP_COMMAND_CELLS, "load Awaiter TaskRun cell"),
        (APP_COMMAND_CELLS, "load Awaiter ordinary-chat cell"),
        (APP_COMMAND_CELLS, "persist ordinary-chat command cell"),
        (APP_CONTINUATION, "load continuation eligibility"),
        (APP_CONTINUATION, "cancel continuation TaskRun"),
        (APP_COMPACT_CONTEXT, "project TaskRuntime model context"),
        (APP_TASK_TOOLS, "check TaskRun status tool"),
        (APP_TASK_TOOLS, "cancel TaskRun tool"),
        (APP_CONVERSATION_DELETION, "cancel conversation TaskRuns"),
        (APP_CONVERSATION_DELETION, "remove conversation TaskRuns"),
        (APP_STORE, "drive registered TaskRun"),
        (APP_TURN_CONTEXT, "project_pending_awaiter_results"),
    ] {
        require(
            source,
            operation,
            &format!("TaskRuntime async production inventory lost operation '{operation}'"),
        )?;
    }
    require(
        APP_EXECUTOR,
        "pub async fn run_async_owned",
        "TaskRuntime multi-stage operation ownership is missing",
    )?;
    require_absent(
        APP_EXECUTOR,
        "fn register_accepted",
        "TaskRuntime settlement reservation can bypass sealed admission",
    )?;
    require(
        APP_RUNTIME,
        "store.shutdown_operations().await",
        "application lifecycle does not join TaskRuntime operations",
    )?;
    require(
        APP_STATE,
        "begin_task_runtime_operation_shutdown",
        "application phase one does not close workspace TaskRuntime operation admission",
    )?;
    require(
        APP_WORKSPACE_RUNTIME,
        "active_task_runtime_operations",
        "workspace teardown does not treat TaskRuntime operations as busy",
    )?;
    require(
        APP_WORKSPACE_RUNTIME,
        "shutdown_settlement: std::sync::Mutex<Option<WorkspaceShutdownSettlement>>",
        "workspace shutdown does not retain one state-owned shared settlement",
    )?;
    require(
        APP_WORKSPACE_RUNTIME,
        "runtime.spawn(settlement.clone())",
        "workspace shutdown settlement is still driven only by the eviction caller",
    )?;
    ordered(
        APP_WORKSPACE_RUNTIME,
        "closing.commit();",
        "host.shutdown_runtime().await?;",
        "workspace eviction can reopen a generation after irreversible shutdown starts",
    )?;
    require(
        APP_COMMAND_CELLS,
        ".boxed()\n            .shared()",
        "command-cell shutdown callers do not share one stable framework settlement",
    )?;
    require_absent(
        APP_COMMAND_CELLS,
        "Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>",
        "command-cell shutdown can overwrite and detach a framework JoinHandle",
    )?;
    require_absent(
        APP_CHAT_DRIVER,
        "receiver.blocking_recv()",
        "per-turn projector still retains a blocking thread for its full lifetime",
    )?;
    let awaiter_publish = between(
        APP_COMMAND_CELLS,
        "async fn publish_awaiter_result",
        "async fn persist_awaiter_delivery_fact",
        "Awaiter Ready publication boundary could not be isolated",
    )?;
    require(
        awaiter_publish,
        ".run(\"persist Awaiter Ready fact\"",
        "Awaiter Ready fact bypasses bounded product-data I/O",
    )?;
    require_absent(
        awaiter_publish,
        "self.chat_events.append(",
        "Awaiter Ready fact directly appends on a Tokio executor thread",
    )?;
    let tui_dispatch = between(
        APP_TUI_EVENTS,
        "async fn dispatch_turn",
        "fn run_turn_binding_for_queued_turn",
        "TUI turn-dispatch boundary could not be isolated",
    )?;
    require_absent(
        tui_dispatch,
        "validate_resumable(",
        "TUI resume surface pre-validates a stale journal sequence before store authority",
    )?;
    require_absent(
        tui_dispatch,
        "get_run_state(",
        "TUI resume surface performs a non-authoritative TaskRuntime read",
    )?;
    let cli_prepare = between(
        APP_CLI_REPL,
        "async fn prepare_repl_turn_start",
        "fn spawn_prepared_repl_turn",
        "CLI REPL turn-preparation boundary could not be isolated",
    )?;
    require_absent(
        cli_prepare,
        "validate_resumable(",
        "CLI resume surface pre-validates a stale journal sequence before store authority",
    )?;
    require_absent(
        cli_prepare,
        "get_run_state(",
        "CLI resume surface performs synchronous TaskRuntime file I/O",
    )?;
    let tauri_cancel = between(
        APP_TAURI_CHAT,
        "pub async fn cancel_chat",
        "fn validate_hitl_response_scope",
        "Tauri cancel boundary could not be isolated",
    )?;
    require(
        tauri_cancel,
        "append_chat_projection(",
        "Tauri cancel orphan projection bypasses bounded product-data I/O",
    )?;
    let tauri_orphan = between(
        APP_TAURI_CHAT,
        "async fn settle_orphaned_hitl_projection",
        "pub async fn send_approval_response",
        "Tauri HITL orphan boundary could not be isolated",
    )?;
    require(
        tauri_orphan,
        "append_chat_projection(",
        "Tauri HITL orphan projection bypasses bounded product-data I/O",
    )?;
    let tauri_append = between(
        APP_TAURI_CHAT,
        "async fn append_chat_projection",
        "pub async fn cancel_chat",
        "Tauri chat-projection boundary could not be isolated",
    )?;
    require(
        tauri_append,
        ".run(\"append GUI chat projection\"",
        "Tauri chat projection bypasses bounded product-data I/O",
    )?;
    require(
        tauri_append,
        "workspace_io_receipt()",
        "Tauri chat projection does not retain workspace generation ownership",
    )?;
    let manual_compression = between(
        APP_MANUAL_COMPRESSION,
        "pub async fn compress_conversation_owned",
        "#[cfg(test)]",
        "manual-compression production boundary could not be isolated",
    )?;
    require(
        manual_compression,
        "let appended = flow",
        "manual compression safe point bypasses bounded product-data I/O",
    )?;
    require(
        manual_compression,
        "persist manual compression safe point",
        "manual compression safe-point operation is missing",
    )?;
    require(
        manual_compression,
        "workspace_io_receipt()",
        "manual compression safe point does not retain workspace generation ownership",
    )?;
    require_absent(
        manual_compression,
        "self.storage.chat_events.append(",
        "manual compression directly appends on a Tokio executor thread",
    )?;
    require(
        APP_TAURI_TASKS,
        "service.list_unified(None).await",
        "Tauri background-task projection bypasses its async service boundary",
    )?;
    require(
        APP_REVISION_ADAPTER,
        "run_store(\"commit revisioned task graph\"",
        "revision compare-and-commit bypasses the bounded adapter",
    )?;
    require(
        APP_TAURI_TASK_RUNTIME,
        "async fn task_runtime_io",
        "Tauri TaskRuntime commands lost their shared async I/O boundary",
    )?;
    let tauri_io_boundaries = APP_TAURI_TASK_RUNTIME
        .split("#[cfg(test)]")
        .next()
        .ok_or_else(|| "Tauri TaskRuntime production section missing".to_string())?
        .matches("task_runtime_io(")
        .count();
    if tauri_io_boundaries != 23 {
        return Err(format!(
            "Tauri TaskRuntime blocking-boundary inventory changed without review: expected 23, found {tauri_io_boundaries}"
        ));
    }
    require_absent(
        APP_TAURI_TASK_RUNTIME,
        "Result<serde_json::Value, IpcError>",
        "TaskRuntime mutation IPC regressed to an untyped JSON receipt",
    )?;
    require_absent(
        APP_TAURI_TASK_RUNTIME,
        "Result<u8, IpcError>",
        "TaskRuntime interaction-mode IPC regressed to a numeric contract",
    )
}

#[test]
fn async_product_data_io_requires_an_application_owned_service() -> Result<(), String> {
    for (name, source) in [
        ("product_data_io", APP_PRODUCT_DATA_IO),
        ("analysis_runtime", APP_ANALYSIS_RUNTIME),
        ("analysis", APP_ANALYSIS),
        ("research_connectors", APP_RESEARCH_CONNECTORS),
        ("research_tool", APP_RESEARCH_TOOL),
        ("command_cells", APP_COMMAND_CELLS),
        ("manual_compression", APP_MANUAL_COMPRESSION),
        ("runtime", APP_RUNTIME),
        ("state", APP_STATE),
        ("workspace_runtime", APP_WORKSPACE_RUNTIME),
        ("infra", APP_INFRA),
        ("channels", APP_CHANNELS),
        ("tauri_chat", APP_TAURI_CHAT),
        ("tauri_files", APP_TAURI_FILES),
        ("tauri_research", APP_TAURI_RESEARCH),
        ("tauri_workspace", APP_TAURI_WORKSPACE),
    ] {
        require_absent(
            before_test_module(source),
            "product_data_io::run(",
            &format!("{name} uses process-global product-data I/O without lifecycle settlement"),
        )?;
    }
    require(
        APP_CONVERSATION_DELETION,
        "begin_owned_flow(\"delete conversation aggregate\")",
        "conversation deletion is not owned across surface caller drop",
    )?;
    require(
        APP_CONVERSATION_DELETION,
        "DeletionIo::Flow(flow)",
        "conversation deletion cannot continue nested I/O after service seal",
    )?;
    Ok(())
}

#[test]
fn accepted_multi_stage_product_data_uses_flow_scoped_nested_io() -> Result<(), String> {
    for (name, source, marker) in [
        (
            "manual compression",
            APP_MANUAL_COMPRESSION,
            "begin_owned_flow(\"manual context compression\")",
        ),
        (
            "analytics runtime",
            APP_ANALYSIS_RUNTIME,
            "begin_owned_flow(\"prepare analytics runtime\")",
        ),
        (
            "research connectors",
            APP_RESEARCH_CONNECTORS,
            "begin_owned_flow(\"automatic research ingest\")",
        ),
        ("command cells", APP_COMMAND_CELLS, "product_data_flow"),
        (
            "workspace runtime",
            APP_WORKSPACE_RUNTIME,
            "begin_owned_flow(\"prepare workspace runtime resources\")",
        ),
        (
            "channel turn preparation",
            APP_CHANNELS,
            "begin_owned_flow(\"prepare channel user turn\")",
        ),
    ] {
        require(
            before_test_module(source),
            marker,
            &format!("{name} does not admit product-data before its awaited producer phase"),
        )?;
    }
    require(
        APP_ANALYSIS,
        "Option<&crate::product_data_io::ProductDataIoFlow>",
        "analysis late persistence does not reuse the supervisor-owned flow",
    )?;
    require(
        APP_RUNTIME,
        "product_data_io.begin_shutdown()",
        "application phase one does not seal new product-data admission",
    )?;
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
        "with_command_policy(Arc::new(crate::permission::EkoCommandPolicy))",
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
            "committed but projection degraded",
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
