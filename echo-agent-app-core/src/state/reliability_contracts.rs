//! M0 failing contract tests for the 2026-08-21 reliability spec (F01-F18).
//!
//! Every `#[ignore]` test pins the *corrected* contract from
//! `design/specs/runtime-reliability.md` and is
//! expected to fail on the pre-repair implementation. The failing baseline is
//! recorded by running `cargo test -p echo-agent-app-core -- --ignored`; each
//! milestone (M1-M8) flips its tests back on and must leave them green.
//! Tests without `#[ignore]` are positive baselines: they pin behavior that
//! already matches the spec and must never regress.
//!
//! This module is a child of `state` so the fixtures can reuse the private
//! workspace-runtime resolution paths exactly like production code.

use std::sync::Arc;

use echo_agent::agent::ReactAgentBuilder;
use echo_agent::memory::NewConversation;
use echo_agent::testing::MockLlmClient;

use super::AppState;
use super::WorkspaceRegistry;
use crate::agent_handle::AgentHandle;
use crate::workspace::WorkspaceKind;

type Fixture = (tempfile::TempDir, Arc<AppState>);

async fn isolated_app_state() -> anyhow::Result<Fixture> {
    let temp = tempfile::tempdir()?;
    let registry = Arc::new(WorkspaceRegistry::with_base_dir(
        temp.path().join("workspaces"),
    )?);
    let primary = ReactAgentBuilder::new()
        .llm_client(Arc::new(MockLlmClient::new()))
        .system_prompt("reliability contract fixture")
        .build()
        .map(AgentHandle::new)?;
    let seed_pool = Arc::new(
        crate::agent_pool::AgentPool::new_for_test(primary.clone(), None, None, 4, false).await,
    );
    let mcp = Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
        temp.path().join("mcp.json"),
        Default::default(),
    ));
    let mut state = AppState::from_shared(
        primary,
        None,
        Arc::new(crate::hitl::HitlDispatcher::new()),
        None,
        None,
        Default::default(),
        mcp,
    )?;
    state.storage.chat_events = Arc::new(crate::chat_event_log::ChatEventLog::open(
        temp.path().join("chat-events"),
        crate::chat_event_log::ChatEventRetention::default(),
    )?);
    state.workspace.registry = registry;
    state.agent_router = Arc::new(crate::agent_router::AgentRouter::new(
        temp.path().join("agent-router"),
    ));
    state.agent_deliveries = Arc::new(crate::agent_router::AgentDeliverySupervisor::default());
    state.set_pool(seed_pool);
    state.tasks.runtime = Some(Arc::new(
        crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()?,
    ));
    Ok((temp, Arc::new(state)))
}

/// F03/M3: one `AgentAddress` admits at most one user foreground turn across
/// GUI/TUI/CLI/channel surfaces. Today `ForegroundTurnKey` includes the
/// surface, so a TUI turn can run concurrently with the same conversation's
/// GUI turn and both write the same transcript.
#[test]
fn f03_one_user_turn_per_address_across_surfaces() {
    use crate::foreground_turn::{
        ForegroundTurnControl, ForegroundTurnError, ForegroundTurnSurface,
    };

    let control = ForegroundTurnControl::default();
    let gui_lease = control
        .begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Gui,
            "conversation-shared",
            "gui-root",
        )
        .expect("the first GUI turn must be admitted");

    let second = control.begin_scoped(
        "workspace-a",
        ForegroundTurnSurface::Tui,
        "conversation-shared",
        "tui-root",
    );

    assert!(
        matches!(second, Err(ForegroundTurnError::Busy { .. })),
        "a TUI turn for the same workspace conversation must conflict with the live GUI turn"
    );
    gui_lease.settle(crate::chat_driver::TurnOutcome::Completed);
}

/// Positive baseline (spec invariant 3): the same conversation id in two
/// different workspaces is two independent addresses and must run
/// concurrently. This already works and must keep working.
#[test]
fn same_conversation_id_in_different_workspaces_admits_concurrently() {
    use crate::foreground_turn::ForegroundTurnSurface;

    let control = crate::foreground_turn::ForegroundTurnControl::default();
    let lease_a = control
        .begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Gui,
            "conversation-shared",
            "turn-a",
        )
        .expect("workspace A must admit its own turn");
    let lease_b = control
        .begin_scoped(
            "workspace-b",
            ForegroundTurnSurface::Gui,
            "conversation-shared",
            "turn-b",
        )
        .expect("workspace B must admit the same conversation id concurrently");
    lease_a.settle(crate::chat_driver::TurnOutcome::Completed);
    lease_b.settle(crate::chat_driver::TurnOutcome::Completed);
}

/// M6: conversation deletion suspension must be workspace-qualified. Today
/// `suspended_conversations` is keyed by bare conversation id, so deleting a
/// conversation in one workspace blocks foreground admission for every other
/// workspace that happens to reuse the same conversation id.
#[test]
fn m6_conversation_suspension_is_workspace_qualified() {
    use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

    let control = ForegroundTurnControl::default();
    let _suspension = control
        .suspend_conversation_admission_if_idle_scoped("workspace-a", "conversation-shared")
        .expect("suspending the workspace A conversation must succeed while idle");

    let other_workspace_turn = control.begin_scoped(
        "workspace-b",
        ForegroundTurnSurface::Gui,
        "conversation-shared",
        "turn-b",
    );

    match other_workspace_turn {
        Ok(lease) => {
            lease.settle(crate::chat_driver::TurnOutcome::Completed);
        }
        Err(error) => panic!(
            "workspace B admission must survive a same-id suspension in workspace A: {error}"
        ),
    }
}

/// F09/M6: deleting a workspace conversation must retire the workspace-owned
/// TaskRuntime records for that conversation. Today the delete path passes the
/// process-global pool/TaskRuntime, so workspace runs survive their own
/// conversation deletion.
#[tokio::test]
async fn f09_workspace_conversation_delete_removes_workspace_task_runs() -> anyhow::Result<()> {
    use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

    let (temp, state) = isolated_app_state().await?;
    let workspace = state.workspace.registry.create_at(
        "alpha",
        WorkspaceKind::General,
        temp.path().join("alpha"),
    )?;

    state.switch_workspace(workspace.clone()).await?;
    let runtime = state.current_chat_runtime().await?;
    runtime
        .ensure_conversation(NewConversation {
            conversation_id: "contract-conversation".to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("Deletion contract".to_string()),
        })
        .await?;

    let workspace_store = runtime
        .task_runtime()
        .ok_or_else(|| anyhow::anyhow!("focused workspace must expose its TaskRuntime store"))?;
    workspace_store.create_run(
        "run-delete-contract",
        workspace.id.as_str(),
        "contract-conversation",
        "root-message",
        DomainProfile::General,
        "contract goal",
        "agent_task_plan",
        AttendedMode::Attended,
    )?;
    workspace_store.transition_run("run-delete-contract", TaskRunStatus::Running)?;

    state
        .delete_conversation_owned("contract-conversation")
        .await?;

    let surviving = workspace_store.get_run("run-delete-contract")?;
    assert!(
        surviving.is_none(),
        "workspace TaskRuntime records for the deleted conversation must be retired"
    );
    Ok(())
}

/// F11/M2: chat events for a workspace conversation must carry workspace
/// identity. Today `ChatEventEnvelope` has no workspace id, so the GUI cannot
/// bucket events per address and cross-workspace conversation-id reuse can
/// interleave two projects into one projection.
#[test]
fn f11_workspace_chat_events_carry_workspace_identity() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let log = crate::chat_event_log::ChatEventLog::open(
        temp.path().join("chat-events"),
        crate::chat_event_log::ChatEventRetention::default(),
    )?;

    let envelope = log.append(
        "workspace-contract",
        Some("contract-conversation"),
        "contract-root-turn",
        crate::chat_driver::ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        },
    )?;

    let encoded = serde_json::to_value(&envelope)?;
    let workspace_id = encoded
        .get("workspace_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        !workspace_id.is_empty(),
        "a workspace conversation chat event must carry a non-empty workspace_id"
    );
    Ok(())
}

/// F08/M7: live Agent delivery must not be terminally `Delivered` on steer
/// acceptance. Delivery is terminal only after the target turn reaches its
/// transcript safe point. Today steer acceptance immediately writes
/// `Delivered`, so a later cancel or crash leaves a false success receipt.
#[tokio::test]
async fn f08_live_steer_delivery_is_not_terminal_before_target_settlement() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let source_workspace = state.workspace.registry.create_at(
        "source",
        WorkspaceKind::General,
        temp.path().join("source"),
    )?;
    let target_workspace = state.workspace.registry.create_at(
        "target",
        WorkspaceKind::General,
        temp.path().join("target"),
    )?;

    for workspace in [&source_workspace, &target_workspace] {
        let host = state
            .workspace
            .runtimes
            .get_or_open(workspace.clone())
            .await?;
        host.resources()
            .conversation_store()
            .ensure_conversation(NewConversation {
                conversation_id: "contract-conversation".to_string(),
                user_id: "default".to_string(),
                agent_type: None,
                title: Some("Delivery contract".to_string()),
            })
            .await?;
    }

    let seed_pool = state
        .connection
        .pool
        .clone()
        .ok_or_else(|| anyhow::anyhow!("fixture pool missing"))?;
    seed_pool
        .set_llm_client_override_for_test(Arc::new(
            MockLlmClient::new()
                .with_responses(["active turn draft", "active turn after steer"])
                .with_delay(std::time::Duration::from_secs(2)),
        ))
        .await;

    let target =
        crate::agent_router::AgentAddress::new(target_workspace.id, "contract-conversation");
    let runtime = state.chat_runtime_for_agent(&target).await?;
    let lease = runtime
        .begin_turn(
            &state.session.foreground_turns,
            crate::foreground_turn::ForegroundTurnSurface::Gui,
            &target.conversation_id,
            "active-target-turn",
        )
        .await?;
    let execution = runtime.agent_for(&target.conversation_id).await?;
    let active_agent = execution.agent();
    let spill_dir =
        crate::prepared_turn::resolve_user_input_spill_dir(Some(runtime.execution_scope().root()));
    let active_turn =
        crate::prepared_turn::PreparedUserTurn::build(crate::prepared_turn::UserTurnInput {
            text: "Start a delayed target turn",
            attachments: &[],
            spill_dir: &spill_dir,
            conversation_id: Some(&target.conversation_id),
            turn_id: Some("active-target-turn"),
        })?;
    let sink: Arc<dyn crate::chat_driver::ChatSink> = Arc::new(NoopChatSink);
    let resources = Arc::new(crate::chat_resources::ChatResources {
        execution_scope: runtime.execution_scope().clone(),
        pool: runtime.pool(),
        store: runtime.task_runtime(),
        sink: sink.clone(),
        webhook_emitter: Some(state.webhook.emitter.clone()),
        conv_id: Some(target.conversation_id.clone()),
        root_message_id: "active-target-turn".to_string(),
        attachments: Vec::new(),
        cancel: lease.cancellation_token(),
        interaction_mode: crate::tasks::task_runtime::InteractionMode::Auto,
        review_integration: runtime.review_integration(),
        layer_manager: None,
        memory_generation: None,
        human_loop_provider: Some(Arc::new(crate::hitl::HitlDispatcher::new())),
    });
    let drive_agent = active_agent.clone();
    let active_task = tokio::spawn(async move {
        crate::foreground_turn::drive_foreground_chat(lease, &drive_agent, &active_turn, resources)
            .await
    });
    // Give the driver a bounded window to admit and enter its model call. The
    // mock client delays every response, so the turn is provably still active
    // while the delivery below is steered into it.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !active_task.is_finished(),
        "fixture requires the target turn to still be running when the delivery arrives"
    );

    let source =
        crate::agent_router::AgentAddress::new(source_workspace.id, "contract-conversation");
    let mut message =
        crate::agent_router::AgentMessage::user_text(Some(source), target.clone(), "Steer it");
    message.message_id = "contract-live-steer".to_string();
    state.send_agent_message_owned(message).await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = state.agent_router.records(&target).await?;
        if let Some(record) = records
            .iter()
            .find(|record| record.message_id == "contract-live-steer")
            && (record.status != crate::agent_router::AgentDeliveryStatus::Queued
                || record.next_attempt_at.is_some())
        {
            assert_ne!(
                record.status,
                crate::agent_router::AgentDeliveryStatus::Delivered,
                "live steer acceptance must not be a terminal Delivered receipt while the \
                 target turn is still executing"
            );
            break;
        }
        if active_task.is_finished() {
            anyhow::bail!(
                "target turn settled before a non-terminal delivery receipt was persisted; records={records:?}"
            );
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("live delivery was never attempted within the fixture window");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let waiter = state.session.foreground_turns.request_root_cancel_scoped(
        target.workspace_id.as_str(),
        crate::foreground_turn::ForegroundTurnSurface::Gui,
        &target.conversation_id,
        "active-target-turn",
    )?;
    let _ = waiter.wait().await?;
    let _ = active_task.await?;

    let failure_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let records = state.agent_router.records(&target).await?;
        if let Some(record) = records
            .iter()
            .find(|record| record.message_id == "contract-live-steer")
            && record.status != crate::agent_router::AgentDeliveryStatus::Delivered
            && (record.next_attempt_at.is_some() || record.attempt >= 2)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= failure_deadline {
            anyhow::bail!(
                "cancelled live delivery was not retained for retry; records={records:?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

struct NoopChatSink;

impl crate::chat_driver::ChatSink for NoopChatSink {
    fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
        true
    }
}
