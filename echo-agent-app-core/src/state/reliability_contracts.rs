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
use futures::future::BoxFuture;

use super::AppState;
use super::WorkspaceRegistry;
use crate::agent_handle::AgentHandle;
use crate::workspace::WorkspaceKind;

type Fixture = (tempfile::TempDir, Arc<AppState>);

struct CheckpointReadBarrier {
    inner: Arc<dyn echo_agent::state::RuntimeStateStore>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    fail: bool,
}

impl CheckpointReadBarrier {
    fn new(inner: Arc<dyn echo_agent::state::RuntimeStateStore>, fail: bool) -> Self {
        Self {
            inner,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            fail,
        }
    }

    async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

impl echo_agent::state::RuntimeStateStore for CheckpointReadBarrier {
    fn get_checkpoint<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<Option<echo_agent::state::AgentCheckpoint>>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            if self.fail {
                return Err(echo_agent::error::ReactError::Other(
                    "checkpoint barrier rejected context restoration".to_string(),
                ));
            }
            self.inner.get_checkpoint(conversation_id).await
        })
    }

    fn save_checkpoint<'a>(
        &'a self,
        checkpoint: &'a echo_agent::state::AgentCheckpoint,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        self.inner.save_checkpoint(checkpoint)
    }

    fn save_checkpoint_for_scope<'a>(
        &'a self,
        scope_id: &'a str,
        checkpoint: &'a echo_agent::state::AgentCheckpoint,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        self.inner.save_checkpoint_for_scope(scope_id, checkpoint)
    }

    fn runtime_state_ids<'a>(
        &'a self,
        scope_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<Vec<String>>> {
        self.inner.runtime_state_ids(scope_id)
    }

    fn clear_runtime_state<'a>(
        &'a self,
        scope_id: &'a str,
        runtime_state_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<echo_agent::state::RuntimeStateClearReceipt>> {
        self.inner.clear_runtime_state(scope_id, runtime_state_id)
    }

    fn clear_runtime_state_scope<'a>(
        &'a self,
        scope_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<echo_agent::state::RuntimeStateScopeClearReceipt>>
    {
        self.inner.clear_runtime_state_scope(scope_id)
    }

    fn clear_conversation<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        self.inner.clear_conversation(conversation_id)
    }
}

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
        crate::product_data_io::ProductDataIoService::new(),
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
fn f03_one_user_turn_per_address_across_surfaces() -> Result<(), String> {
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
        .map_err(|error| format!("the first GUI turn must be admitted: {error}"))?;

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
    Ok(())
}

/// Positive baseline (spec invariant 3): the same conversation id in two
/// different workspaces is two independent addresses and must run
/// concurrently. This already works and must keep working.
#[test]
fn same_conversation_id_in_different_workspaces_admits_concurrently() -> Result<(), String> {
    use crate::foreground_turn::ForegroundTurnSurface;

    let control = crate::foreground_turn::ForegroundTurnControl::default();
    let lease_a = control
        .begin_scoped(
            "workspace-a",
            ForegroundTurnSurface::Gui,
            "conversation-shared",
            "turn-a",
        )
        .map_err(|error| format!("workspace A must admit its own turn: {error}"))?;
    let lease_b = control
        .begin_scoped(
            "workspace-b",
            ForegroundTurnSurface::Gui,
            "conversation-shared",
            "turn-b",
        )
        .map_err(|error| {
            format!("workspace B must admit the same conversation id concurrently: {error}")
        })?;
    lease_a.settle(crate::chat_driver::TurnOutcome::Completed);
    lease_b.settle(crate::chat_driver::TurnOutcome::Completed);
    Ok(())
}

/// M6: conversation deletion suspension must be workspace-qualified. Today
/// `suspended_conversations` is keyed by bare conversation id, so deleting a
/// conversation in one workspace blocks foreground admission for every other
/// workspace that happens to reuse the same conversation id.
#[test]
fn m6_conversation_suspension_is_workspace_qualified() -> Result<(), String> {
    use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};

    let control = ForegroundTurnControl::default();
    let _suspension = control
        .suspend_conversation_admission_if_idle_scoped("workspace-a", "conversation-shared")
        .map_err(|error| {
            format!("suspending the workspace A conversation must succeed while idle: {error}")
        })?;

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
        Err(error) => {
            return Err(format!(
                "workspace B admission must survive a same-id suspension in workspace A: {error}"
            ));
        }
    }
    Ok(())
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
    let model = Arc::new(
        MockLlmClient::new()
            .with_responses(["active turn draft", "active turn after steer"])
            .with_delay(std::time::Duration::from_secs(2)),
    );
    seed_pool
        .set_llm_client_override_for_test(model.clone())
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
        workspace_io_receipt: Some(runtime.workspace_io_receipt()),
        pool: runtime.pool(),
        store: runtime.task_runtime(),
        sink: sink.clone(),
        webhook_emitter: Some(state.webhook.emitter.clone()),
        conv_id: Some(target.conversation_id.clone()),
        root_message_id: "active-target-turn".to_string(),
        attachments: Vec::new(),
        cancel: lease.cancellation_token(),
        review_integration: runtime.review_integration(),
        memory_generation: None,
        human_loop_provider: Some(Arc::new(crate::hitl::HitlDispatcher::new())),
    });
    let drive_agent = active_agent.clone();
    let active_task = tokio::spawn(async move {
        crate::foreground_turn::drive_foreground_chat(lease, &drive_agent, &active_turn, resources)
            .await
    });
    // Wait for the real driver to enter its provider call before sending the
    // tracked steer. A fixed sleep races AgentPool/configuration under a loaded
    // workspace test run and can observe the pre-steerable admission window.
    let provider_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while model.call_count() == 0 {
        if active_task.is_finished() {
            anyhow::bail!("fixture target turn settled before entering its provider call");
        }
        if tokio::time::Instant::now() >= provider_deadline {
            anyhow::bail!("fixture target turn did not enter its provider call");
        }
        tokio::task::yield_now().await;
    }

    let mut message =
        crate::agent_router::AgentMessage::user_text(None, target.clone(), "Steer it");
    message.message_id = "contract-live-steer".to_string();
    state.send_agent_message_owned(message).await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let records = state.agent_router.records(&target).await?;
        if let Some(record) = records
            .iter()
            .find(|record| record.message_id == "contract-live-steer")
        {
            if record.phase == crate::agent_router::AgentDeliveryPhase::Drained {
                assert_eq!(record.turn_id.as_deref(), Some("active-target-turn"));
                break;
            }
            if record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled {
                assert!(
                    !record.drained
                        && record.outcome
                            == Some(crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown)
                        && record.turn_id.as_deref() == Some("active-target-turn")
                        && record.reason.as_deref().is_some_and(|reason| {
                            reason.contains("outcome unknown")
                                && reason.contains("did not confirm consumption")
                        })
                        && record.next_attempt_at.is_none(),
                    "terminal-before-drain must remain an explicit non-replayable unknown delivery; record={record:?}"
                );
                let _ = active_task.await?;
                state.shutdown_agent_deliveries().await?;
                return Ok(());
            }
        }
        if active_task.is_finished() {
            let deferred_at_real_boundary = records.iter().any(|record| {
                record.message_id == "contract-live-steer"
                    && record.phase == crate::agent_router::AgentDeliveryPhase::Persisted
                    && record.reason.as_deref().is_some_and(|reason| {
                        reason.contains("not steerable") || reason.contains("no active turn")
                    })
            });
            if deferred_at_real_boundary {
                let _ = active_task.await?;
                state.shutdown_agent_deliveries().await?;
                return Ok(());
            }
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
            && record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled
            && record.outcome == Some(crate::agent_router::AgentDeliveryOutcome::Cancelled)
            && record.next_attempt_at.is_none()
            && record.turn_id.as_deref() == Some("active-target-turn")
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= failure_deadline {
            anyhow::bail!(
                "cancelled consumed delivery was not closed without replay; records={records:?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn typed_live_steer_rejections_defer_but_owner_loss_is_terminal() -> anyhow::Result<()> {
    let rejections = [
        echo_agent::agent::TurnSteerError::NoActiveTurn,
        echo_agent::agent::TurnSteerError::TurnMismatch {
            expected: "expected-turn".to_string(),
            actual: "actual-turn".to_string(),
        },
        echo_agent::agent::TurnSteerError::NotSteerable {
            turn_id: "busy-turn".to_string(),
        },
    ];
    for (index, rejection) in rejections.into_iter().enumerate() {
        assert!(super::is_explicit_live_steer_rejection(&rejection));
        let temp = tempfile::tempdir()?;
        let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
        let target = crate::agent_router::AgentAddress::new(
            crate::workspace::WorkspaceId::from_name("target"),
            format!("conversation-{index}"),
        );
        let mut message =
            crate::agent_router::AgentMessage::user_text(None, target.clone(), "typed rejection");
        message.message_id = format!("typed-rejection-{index}");
        router.enqueue(message).await?;
        let claim = router
            .claim_next(&target)
            .await?
            .ok_or_else(|| anyhow::anyhow!("typed-rejection claim is missing"))?;
        router.begin_injection(&claim, "candidate-turn").await?;
        router.defer(&claim, rejection.to_string()).await?;
        let record = router
            .records(&target)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("typed-rejection record is missing"))?;
        assert_eq!(
            record.phase,
            crate::agent_router::AgentDeliveryPhase::Persisted
        );
        assert_eq!(record.attempt, 1);
        assert!(record.turn_id.is_none());
    }

    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = crate::agent_router::AgentAddress::new(
        crate::workspace::WorkspaceId::from_name("target"),
        "owner-loss",
    );
    let mut message =
        crate::agent_router::AgentMessage::user_text(None, target.clone(), "owner loss");
    message.message_id = "owner-loss".to_string();
    router.enqueue(message).await?;
    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("owner-loss claim is missing"))?;
    router.begin_injection(&claim, "unknown-turn").await?;
    router
        .turn_settled(
            &claim,
            Some("unknown-turn".to_string()),
            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
            false,
            Some("owner lost after effect start".to_string()),
            false,
            None,
        )
        .await?;
    let record = router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("owner-loss record is missing"))?;
    assert_eq!(
        record.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert!(record.next_attempt_at.is_none());
    assert_eq!(record.turn_id.as_deref(), Some("unknown-turn"));
    assert_eq!(
        record.outcome,
        Some(crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown)
    );
    assert!(!record.drained);
    Ok(())
}

#[test]
fn live_delivery_requires_one_unambiguous_foreground_snapshot() {
    let stale = crate::foreground_turn::ForegroundTurnSnapshot {
        workspace_id: "target".to_string(),
        surface: crate::foreground_turn::ForegroundTurnSurface::Tui,
        conversation_id: "conversation".to_string(),
        root_turn_id: "stale-root".to_string(),
        active_turn_id: "stale-turn".to_string(),
        cancellation_requested: false,
    };
    let exact = crate::foreground_turn::ForegroundTurnSnapshot {
        workspace_id: "target".to_string(),
        surface: crate::foreground_turn::ForegroundTurnSurface::Gui,
        conversation_id: "conversation".to_string(),
        root_turn_id: "exact-root".to_string(),
        active_turn_id: "exact-turn".to_string(),
        cancellation_requested: false,
    };
    let active = vec![stale, exact.clone()];
    assert!(super::exact_live_delivery_candidate(&active).is_none());
    assert_eq!(
        super::exact_live_delivery_candidate(std::slice::from_ref(&exact)),
        Some(&exact)
    );

    let mut ambiguous = active;
    let mut duplicate = exact;
    duplicate.surface = crate::foreground_turn::ForegroundTurnSurface::Channel;
    ambiguous.push(duplicate);
    assert!(super::exact_live_delivery_candidate(&ambiguous).is_none());
}

#[tokio::test]
async fn delivery_shutdown_interrupts_pending_live_wait_and_preserves_injected()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = crate::agent_router::AgentAddress::new(
        crate::workspace::WorkspaceId::from_name("target"),
        "conversation",
    );
    let mut message =
        crate::agent_router::AgentMessage::user_text(None, target.clone(), "pending live delivery");
    message.message_id = "pending-live".to_string();
    router.enqueue(message).await?;
    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("delivery claim is missing"))?;
    router.begin_injection(&claim, "live-turn").await?;
    router.mailbox_accepted(&claim, "live-turn").await?;
    router.drained(&claim, "live-turn").await?;

    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::wait_for_live_delivery_or_shutdown(
            &shutdown,
            std::future::pending::<Result<(), String>>(),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("live delivery wait ignored supervisor cancellation"))?;
    assert!(outcome.is_none());

    let record = router
        .records(&target)
        .await?
        .into_iter()
        .find(|record| record.message_id == "pending-live")
        .ok_or_else(|| anyhow::anyhow!("injected record is missing"))?;
    assert_eq!(
        record.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );
    assert!(
        router
            .pending(&target)
            .await?
            .iter()
            .any(|message| message.message_id == "pending-live")
    );
    Ok(())
}

struct NoopChatSink;

impl crate::chat_driver::ChatSink for NoopChatSink {
    fn on_event(&self, _event: crate::chat_driver::ChatDriverEvent) -> bool {
        true
    }
}

fn f0_router_target() -> crate::agent_router::AgentAddress {
    crate::agent_router::AgentAddress::new(
        crate::workspace::WorkspaceId::from_name("f0-router-workspace"),
        "f0-router-conversation",
    )
}

fn f0_router_message(
    target: &crate::agent_router::AgentAddress,
    message_id: &str,
    text: &str,
) -> crate::agent_router::AgentMessage {
    let mut message = crate::agent_router::AgentMessage::user_text(None, target.clone(), text);
    message.message_id = message_id.to_string();
    message
}

#[tokio::test]
async fn f0_live_router_receipt_marks_injected_only_after_real_drain() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = f0_router_target();
    let message = f0_router_message(&target, "f0-live-drain", "live input");

    let accepted = router.enqueue(message).await?;
    assert_eq!(
        accepted.phase,
        crate::agent_router::AgentDeliveryPhase::Persisted
    );
    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("live delivery claim is missing"))?;
    assert_eq!(
        router
            .records(&target)
            .await?
            .first()
            .map(|record| record.phase),
        Some(crate::agent_router::AgentDeliveryPhase::Claimed)
    );
    router.begin_injection(&claim, "f0-live-turn").await?;

    let (drain_sender, drain_receiver) =
        tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::Accepted);
    let mut receipt = echo_agent::agent::AgentSteerReceipt::new(
        "f0-steer".to_string(),
        "f0-live-turn".to_string(),
        drain_receiver,
    );
    assert_eq!(
        receipt.state(),
        echo_agent::agent::AgentSteerState::Accepted
    );
    assert_eq!(
        router
            .records(&target)
            .await?
            .first()
            .map(|record| record.phase),
        Some(crate::agent_router::AgentDeliveryPhase::Claimed)
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            receipt.wait_for_drained()
        )
        .await
        .is_err(),
        "receipt must remain accepted until the real framework drain signal"
    );

    drain_sender
        .send(echo_agent::agent::AgentSteerState::Drained)
        .map_err(|error| anyhow::anyhow!("failed to publish drain state: {error}"))?;
    assert_eq!(
        receipt.wait_for_drained().await,
        echo_agent::agent::AgentSteerState::Drained
    );
    let _accepted = router.mailbox_accepted(&claim, receipt.turn_id()).await?;
    let injected = router.drained(&claim, receipt.turn_id()).await?;
    assert_eq!(
        injected.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );
    assert_eq!(
        router
            .records(&target)
            .await?
            .first()
            .map(|record| record.phase),
        Some(crate::agent_router::AgentDeliveryPhase::Drained)
    );
    Ok(())
}

#[tokio::test]
async fn f0_terminal_before_drain_is_not_injected() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = f0_router_target();
    let message = f0_router_message(&target, "f0-terminal-before-drain", "cancel me");
    router.enqueue(message).await?;
    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("terminal-before-drain claim is missing"))?;
    router.begin_injection(&claim, "f0-cancelled-turn").await?;

    let (terminal_sender, terminal_receiver) =
        tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::Accepted);
    let mut receipt = echo_agent::agent::AgentSteerReceipt::new(
        "f0-terminal-steer".to_string(),
        "f0-cancelled-turn".to_string(),
        terminal_receiver,
    );
    terminal_sender
        .send(echo_agent::agent::AgentSteerState::TurnSettled {
            outcome: echo_agent::agent::AgentSteerTurnOutcome::Cancelled,
            drained: false,
        })
        .map_err(|error| anyhow::anyhow!("failed to publish terminal state: {error}"))?;
    let terminal = receipt.wait_for_drained().await;
    assert_eq!(
        terminal,
        echo_agent::agent::AgentSteerState::TurnSettled {
            outcome: echo_agent::agent::AgentSteerTurnOutcome::Cancelled,
            drained: false,
        }
    );
    assert!(!terminal.was_drained());

    let failed = router
        .turn_settled(
            &claim,
            Some("f0-cancelled-turn".to_string()),
            crate::agent_router::AgentDeliveryOutcome::Cancelled,
            false,
            Some("target turn settled before drain".to_string()),
            false,
            None,
        )
        .await?;
    assert_eq!(
        failed.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert!(router.in_flight_claim(&target).await?.is_none());
    assert_eq!(
        router
            .records(&target)
            .await?
            .first()
            .map(|record| record.phase),
        Some(crate::agent_router::AgentDeliveryPhase::TurnSettled)
    );
    Ok(())
}

#[tokio::test]
async fn f0_restart_after_drain_before_terminal_preserves_injected_attempt() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let target = f0_router_target();
    let message = f0_router_message(&target, "f0-restart-after-drain", "recover terminal");
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    router.enqueue(message).await?;
    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("restart claim is missing"))?;
    router.begin_injection(&claim, "f0-restart-turn").await?;
    let (drain_sender, drain_receiver) =
        tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::Drained);
    let mut receipt = echo_agent::agent::AgentSteerReceipt::new(
        "f0-restart-steer".to_string(),
        "f0-restart-turn".to_string(),
        drain_receiver,
    );
    assert_eq!(
        receipt.wait_for_drained().await,
        echo_agent::agent::AgentSteerState::Drained
    );
    router.mailbox_accepted(&claim, receipt.turn_id()).await?;
    router.drained(&claim, receipt.turn_id()).await?;
    drop(drain_sender);
    drop(receipt);
    drop(router);

    let restarted = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    assert!(restarted.claim_next(&target).await?.is_none());
    let in_flight = restarted
        .in_flight_claim(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("injected attempt was not recovered"))?;
    assert_eq!(
        in_flight.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );
    assert_eq!(in_flight.claim.attempt_id, claim.attempt_id);
    assert_eq!(in_flight.claim.attempt, claim.attempt);
    assert_eq!(in_flight.turn_id, "f0-restart-turn");

    restarted
        .turn_settled(
            &in_flight.claim,
            Some(in_flight.turn_id.clone()),
            crate::agent_router::AgentDeliveryOutcome::OutcomeUnknown,
            true,
            Some("terminal outcome unknown after restart".to_string()),
            false,
            None,
        )
        .await?;
    let record = restarted
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("restarted terminal record is missing"))?;
    assert_eq!(
        record.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(record.attempt, 1);
    assert_eq!(record.turn_id.as_deref(), Some("f0-restart-turn"));
    Ok(())
}

#[tokio::test]
async fn f0_cold_and_live_terminal_records_have_parity() -> anyhow::Result<()> {
    async fn settle(live: bool) -> anyhow::Result<crate::agent_router::AgentDeliveryRecord> {
        let temp = tempfile::tempdir()?;
        let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
        let target = f0_router_target();
        let message = f0_router_message(
            &target,
            if live {
                "f0-live-parity"
            } else {
                "f0-cold-parity"
            },
            "terminal parity",
        );
        router.enqueue(message).await?;
        let claim = router
            .claim_next(&target)
            .await?
            .ok_or_else(|| anyhow::anyhow!("parity claim is missing"))?;
        router.begin_injection(&claim, "f0-parity-turn").await?;
        if live {
            let (drain_sender, drain_receiver) =
                tokio::sync::watch::channel(echo_agent::agent::AgentSteerState::Drained);
            let mut receipt = echo_agent::agent::AgentSteerReceipt::new(
                "f0-parity-steer".to_string(),
                "f0-parity-turn".to_string(),
                drain_receiver,
            );
            assert_eq!(
                receipt.wait_for_drained().await,
                echo_agent::agent::AgentSteerState::Drained
            );
            router.mailbox_accepted(&claim, receipt.turn_id()).await?;
            router.drained(&claim, receipt.turn_id()).await?;
            drop(drain_sender);
        } else {
            router.mailbox_accepted(&claim, "f0-parity-turn").await?;
            router.drained(&claim, "f0-parity-turn").await?;
        }
        router
            .turn_settled(
                &claim,
                Some("f0-parity-turn".to_string()),
                crate::agent_router::AgentDeliveryOutcome::Completed,
                true,
                None,
                false,
                None,
            )
            .await?;
        let record = router
            .records(&target)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("parity terminal record is missing"))?;
        assert!(router.pending(&target).await?.is_empty());
        assert!(router.in_flight_claim(&target).await?.is_none());
        Ok(record)
    }

    let live = settle(true).await?;
    let cold = settle(false).await?;
    assert_eq!(
        live.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(
        cold.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(live.attempt, cold.attempt);
    assert_eq!(live.turn_id, cold.turn_id);
    assert_eq!(live.reply_message_id, cold.reply_message_id);
    assert_eq!(live.outcome, cold.outcome);
    assert_eq!(live.drained, cold.drained);
    assert_eq!(live.reason, cold.reason);
    assert_eq!(live.next_attempt_at, cold.next_attempt_at);
    Ok(())
}

#[tokio::test]
async fn f0_duplicate_enqueue_returns_current_receipt_at_each_phase() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = f0_router_target();
    let message = f0_router_message(&target, "f0-duplicate", "idempotent input");

    let first = router.enqueue(message.clone()).await?;
    assert!(!first.duplicate);
    assert_eq!(
        first.phase,
        crate::agent_router::AgentDeliveryPhase::Persisted
    );
    let duplicate = router.enqueue(message.clone()).await?;
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.phase,
        crate::agent_router::AgentDeliveryPhase::Persisted
    );

    let claim = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("duplicate claim is missing"))?;
    let duplicate = router.enqueue(message.clone()).await?;
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.phase,
        crate::agent_router::AgentDeliveryPhase::Claimed
    );

    router.begin_injection(&claim, "f0-duplicate-turn").await?;
    let duplicate = router.enqueue(message.clone()).await?;
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.phase,
        crate::agent_router::AgentDeliveryPhase::Claimed
    );

    router.mailbox_accepted(&claim, "f0-duplicate-turn").await?;
    router.drained(&claim, "f0-duplicate-turn").await?;
    let duplicate = router.enqueue(message.clone()).await?;
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );

    router
        .turn_settled(
            &claim,
            Some("f0-duplicate-turn".to_string()),
            crate::agent_router::AgentDeliveryOutcome::Completed,
            true,
            None,
            false,
            None,
        )
        .await?;
    let duplicate = router.enqueue(message).await?;
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(
        duplicate.outcome,
        Some(crate::agent_router::AgentDeliveryOutcome::Completed)
    );
    assert!(duplicate.drained);
    Ok(())
}

#[tokio::test]
async fn f0_stale_router_attempt_cannot_cross_aba_generation() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let router = crate::agent_router::AgentRouter::new(temp.path().to_path_buf());
    let target = f0_router_target();
    let message = f0_router_message(&target, "f0-aba-attempt", "retry once");
    router.enqueue(message).await?;
    let stale = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stale attempt claim is missing"))?;
    router.defer(&stale, "first attempt is retryable").await?;
    let deadline = router
        .next_attempt_at(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("retry deadline is missing"))?;
    let delay = deadline
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or(std::time::Duration::ZERO);
    if !delay.is_zero() {
        tokio::time::sleep(delay.saturating_add(std::time::Duration::from_millis(5))).await;
    }
    let current = router
        .claim_next(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("replacement attempt claim is missing"))?;
    assert_eq!(current.attempt, stale.attempt.saturating_add(1));
    assert!(matches!(
        router.begin_injection(&stale, "stale-turn").await,
        Err(crate::agent_router::AgentRouterError::StaleClaim { .. })
    ));
    router.begin_injection(&current, "current-turn").await?;
    router.mailbox_accepted(&current, "current-turn").await?;
    router.drained(&current, "current-turn").await?;
    router
        .turn_settled(
            &current,
            Some("current-turn".to_string()),
            crate::agent_router::AgentDeliveryOutcome::Completed,
            true,
            None,
            false,
            None,
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn f0_idle_text_starts_a_cold_turn() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let target_workspace = state.workspace.registry.create_at(
        "f0-cold-target",
        WorkspaceKind::General,
        temp.path().join("f0-cold-target"),
    )?;
    let host = state
        .workspace
        .runtimes
        .get_or_open(target_workspace.clone())
        .await?;
    host.resources()
        .conversation_store()
        .ensure_conversation(NewConversation {
            conversation_id: "f0-cold-conversation".to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F0 cold turn".to_string()),
        })
        .await?;
    let target =
        crate::agent_router::AgentAddress::new(target_workspace.id, "f0-cold-conversation");
    let pool = state
        .chat_runtime_for_agent(&target)
        .await?
        .pool()
        .ok_or_else(|| anyhow::anyhow!("fixture AgentPool is missing"))?;
    pool.set_llm_client_override_for_test(Arc::new(
        MockLlmClient::new().with_responses(["cold turn preflight", "cold turn answer"]),
    ))
    .await;

    let message = f0_router_message(&target, "f0-idle-text", "start from idle");
    state.agent_router.enqueue(message.clone()).await?;
    assert!(
        state
            .deliver_agent_message_cold(&target, &tokio_util::sync::CancellationToken::new())
            .await?
    );
    let record = state
        .agent_router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("cold delivery record is missing"))?;
    assert_eq!(
        record.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(record.attempt, 1);
    assert_eq!(
        record.turn_id.as_deref(),
        Some(message.delivery_turn_id().as_str())
    );
    assert!(state.agent_router.pending(&target).await?.is_empty());
    assert!(state.agent_router.in_flight_claim(&target).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn f1_cold_drain_is_durable_before_terminal_and_reopen_visible() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let target_workspace = state.workspace.registry.create_at(
        "f1-cold-drain-target",
        WorkspaceKind::General,
        temp.path().join("f1-cold-drain-target"),
    )?;
    let host = state
        .workspace
        .runtimes
        .get_or_open(target_workspace.clone())
        .await?;
    host.resources()
        .conversation_store()
        .ensure_conversation(NewConversation {
            conversation_id: "f1-cold-drain-conversation".to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F1 cold drain".to_string()),
        })
        .await?;
    let target =
        crate::agent_router::AgentAddress::new(target_workspace.id, "f1-cold-drain-conversation");
    let pool = state
        .chat_runtime_for_agent(&target)
        .await?
        .pool()
        .ok_or_else(|| anyhow::anyhow!("fixture AgentPool is missing"))?;
    pool.set_llm_client_override_for_test(Arc::new(
        MockLlmClient::new()
            .with_responses(["cold drain preflight", "cold drain answer"])
            .with_delay(std::time::Duration::from_millis(200)),
    ))
    .await;

    let message = f0_router_message(&target, "f1-cold-drain", "observe initial input");
    state.agent_router.enqueue(message.clone()).await?;
    let delivery_state = Arc::clone(&state);
    let delivery_target = target.clone();
    let delivery = tokio::spawn(async move {
        delivery_state
            .deliver_agent_message_cold(
                &delivery_target,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = state.agent_router.records(&target).await?;
        if records
            .first()
            .is_some_and(|record| record.phase == crate::agent_router::AgentDeliveryPhase::Drained)
        {
            break;
        }
        if delivery.is_finished() {
            anyhow::bail!(
                "cold turn settled before the drain projection was observable; records={records:?}"
            );
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("cold input did not reach the framework drain boundary");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !delivery.is_finished(),
        "Injected must be projected from input drain, before turn settlement"
    );

    // Reopen the same journal root while the original owner is live. This
    // characterizes persisted visibility, not a simulated process restart.
    let reopened = crate::agent_router::AgentRouter::new(temp.path().join("agent-router"));
    let recovered = reopened
        .in_flight_claim(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("reopened router lost the drained attempt"))?;
    assert_eq!(
        recovered.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );
    assert_eq!(recovered.claim.message.message_id, message.message_id);
    assert_eq!(recovered.turn_id, message.delivery_turn_id());

    let delivered = delivery.await??;
    assert!(delivered);
    let terminal = state
        .agent_router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("cold terminal record is missing"))?;
    assert_eq!(
        terminal.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    Ok(())
}

#[tokio::test]
async fn f1_cold_owner_exit_after_drain_recovers_without_model_replay() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let target_workspace = state.workspace.registry.create_at(
        "f1-cold-owner-exit-target",
        WorkspaceKind::General,
        temp.path().join("f1-cold-owner-exit-target"),
    )?;
    let target = crate::agent_router::AgentAddress::new(
        target_workspace.id,
        "f1-cold-owner-exit-conversation",
    );
    let runtime = state.chat_runtime_for_agent(&target).await?;
    runtime
        .ensure_conversation(NewConversation {
            conversation_id: target.conversation_id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F1 cold owner exit".to_string()),
        })
        .await?;
    let pool = runtime
        .pool()
        .ok_or_else(|| anyhow::anyhow!("fixture AgentPool is missing"))?;
    let model = Arc::new(
        MockLlmClient::new()
            .with_responses(["owner exit preflight", "owner exit answer"])
            .with_delay(std::time::Duration::from_secs(30)),
    );
    pool.set_llm_client_override_for_test(model.clone()).await;

    let message = f0_router_message(&target, "f1-cold-owner-exit", "interrupt after drain");
    let expected_turn_id = message.delivery_turn_id();
    state.agent_router.enqueue(message.clone()).await?;
    let delivery_state = Arc::clone(&state);
    let delivery_target = target.clone();
    let delivery = tokio::spawn(async move {
        delivery_state
            .deliver_agent_message_cold(
                &delivery_target,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let injected = loop {
        if let Some(in_flight) = state.agent_router.in_flight_claim(&target).await?
            && in_flight.phase == crate::agent_router::AgentDeliveryPhase::Drained
        {
            break in_flight;
        }
        if delivery.is_finished() {
            anyhow::bail!("cold owner settled before the durable drain boundary");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("cold owner did not publish its drained attempt");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(injected.claim.message.message_id, message.message_id);
    assert_eq!(injected.claim.attempt, 1);
    assert_eq!(injected.turn_id, expected_turn_id);

    while model.call_count() == 0 {
        if delivery.is_finished() {
            anyhow::bail!("cold owner settled before entering the provider call");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("cold owner did not enter the provider call after drain");
        }
        tokio::task::yield_now().await;
    }
    let calls_before_owner_exit = model.call_count();
    assert_eq!(calls_before_owner_exit, 1);
    delivery.abort();
    let owner_exit = delivery.await;
    assert!(
        owner_exit.is_err_and(|error| error.is_cancelled()),
        "the simulated cold owner exit must abort the real delivery future"
    );
    assert!(
        state
            .session
            .foreground_turns
            .snapshots_for_conversation_scoped(
                target.workspace_id.as_str(),
                &target.conversation_id,
            )?
            .is_empty(),
        "aborting the cold owner must retire its foreground turn before recovery"
    );

    let abandoned = state
        .agent_router
        .in_flight_claim(&target)
        .await?
        .ok_or_else(|| anyhow::anyhow!("owner exit lost the drained attempt"))?;
    assert_eq!(
        abandoned.phase,
        crate::agent_router::AgentDeliveryPhase::Drained
    );
    assert_eq!(abandoned.claim.attempt_id, injected.claim.attempt_id);
    assert_eq!(abandoned.claim.attempt, injected.claim.attempt);
    assert_eq!(abandoned.turn_id, injected.turn_id);

    assert_eq!(state.recover_agent_deliveries().await?, 1);
    let recovery_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let recovered = loop {
        let record = state
            .agent_router
            .records(&target)
            .await?
            .into_iter()
            .find(|record| record.message_id == message.message_id)
            .ok_or_else(|| anyhow::anyhow!("recovery lost the cold delivery record"))?;
        if record.phase == crate::agent_router::AgentDeliveryPhase::TurnSettled {
            break record;
        }
        if tokio::time::Instant::now() >= recovery_deadline {
            anyhow::bail!("cold recovery did not settle the abandoned drained attempt");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(
        recovered.attempt_id.as_deref(),
        Some(injected.claim.attempt_id.as_str())
    );
    assert_eq!(recovered.attempt, injected.claim.attempt);
    assert_eq!(
        recovered.turn_id.as_deref(),
        Some(injected.turn_id.as_str())
    );
    assert!(recovered.next_attempt_at.is_none());
    assert!(
        recovered
            .reason
            .as_deref()
            .is_some_and(|error| error.contains("outcome unknown"))
    );
    assert_eq!(
        state
            .agent_router
            .event_phases_for_test(&target, &message.message_id)
            .await?,
        [
            "persisted",
            "claimed",
            "effect_started",
            "mailbox_accepted",
            "drained",
            "turn_settled"
        ],
        "recovery must settle the exact drained attempt without a second claim or injection"
    );
    assert_eq!(
        model.call_count(),
        calls_before_owner_exit,
        "recovery must not replay the model input after durable drain"
    );
    assert!(state.agent_router.in_flight_claim(&target).await?.is_none());
    state.shutdown_agent_deliveries().await?;
    Ok(())
}

#[tokio::test]
async fn f1_cold_acceptance_waits_for_real_context_drain() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let target_workspace = state.workspace.registry.create_at(
        "f1-cold-barrier-target",
        WorkspaceKind::General,
        temp.path().join("f1-cold-barrier-target"),
    )?;
    let target =
        crate::agent_router::AgentAddress::new(target_workspace.id, "f1-cold-barrier-conversation");
    let runtime = state.chat_runtime_for_agent(&target).await?;
    runtime
        .ensure_conversation(NewConversation {
            conversation_id: target.conversation_id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F1 cold context barrier".to_string()),
        })
        .await?;
    let state_store = runtime
        .runtime_state_store()
        .ok_or_else(|| anyhow::anyhow!("fixture RuntimeStateStore is missing"))?;
    let barrier = Arc::new(CheckpointReadBarrier::new(state_store, false));
    let pool = runtime
        .pool()
        .ok_or_else(|| anyhow::anyhow!("fixture AgentPool is missing"))?;
    pool.apply_state_store(barrier.clone()).await;
    pool.set_llm_client_override_for_test(Arc::new(
        MockLlmClient::new().with_responses(["barrier preflight", "barrier answer"]),
    ))
    .await;

    let message = f0_router_message(&target, "f1-cold-barrier", "wait for context");
    state.agent_router.enqueue(message).await?;
    let delivery_state = Arc::clone(&state);
    let delivery_target = target.clone();
    let delivery = tokio::spawn(async move {
        delivery_state
            .deliver_agent_message_cold(
                &delivery_target,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        barrier.wait_until_entered(),
    )
    .await?;

    let accepted = state
        .agent_router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("cold accepted record is missing"))?;
    assert_eq!(
        accepted.phase,
        crate::agent_router::AgentDeliveryPhase::Claimed,
        "AgentTurnDriver accepted the request before entering context restore, but the router must not project Drained yet"
    );
    barrier.release();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), delivery).await???);
    let terminal = state
        .agent_router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("cold barrier terminal is missing"))?;
    assert_eq!(
        terminal.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(
        terminal.outcome,
        Some(crate::agent_router::AgentDeliveryOutcome::Completed)
    );
    assert!(terminal.drained);
    Ok(())
}

#[tokio::test]
async fn f1_cold_terminal_before_drain_fails_without_drained_replay() -> anyhow::Result<()> {
    let (temp, state) = isolated_app_state().await?;
    let target_workspace = state.workspace.registry.create_at(
        "f1-cold-reject-target",
        WorkspaceKind::General,
        temp.path().join("f1-cold-reject-target"),
    )?;
    let target =
        crate::agent_router::AgentAddress::new(target_workspace.id, "f1-cold-reject-conversation");
    let runtime = state.chat_runtime_for_agent(&target).await?;
    runtime
        .ensure_conversation(NewConversation {
            conversation_id: target.conversation_id.clone(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("F1 cold rejected context".to_string()),
        })
        .await?;
    let state_store = runtime
        .runtime_state_store()
        .ok_or_else(|| anyhow::anyhow!("fixture RuntimeStateStore is missing"))?;
    let barrier = Arc::new(CheckpointReadBarrier::new(state_store, true));
    runtime
        .pool()
        .ok_or_else(|| anyhow::anyhow!("fixture AgentPool is missing"))?
        .apply_state_store(barrier.clone())
        .await;

    let message = f0_router_message(&target, "f1-cold-rejected", "reject context");
    state.agent_router.enqueue(message).await?;
    let delivery_state = Arc::clone(&state);
    let delivery_target = target.clone();
    let delivery = tokio::spawn(async move {
        delivery_state
            .deliver_agent_message_cold(
                &delivery_target,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        barrier.wait_until_entered(),
    )
    .await?;
    assert_eq!(
        state
            .agent_router
            .records(&target)
            .await?
            .first()
            .map(|record| record.phase),
        Some(crate::agent_router::AgentDeliveryPhase::Claimed)
    );
    barrier.release();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), delivery).await???);
    let failed = state
        .agent_router
        .records(&target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("cold rejection terminal is missing"))?;
    assert_eq!(
        failed.phase,
        crate::agent_router::AgentDeliveryPhase::TurnSettled
    );
    assert_eq!(
        failed.outcome,
        Some(crate::agent_router::AgentDeliveryOutcome::Failed)
    );
    assert!(!failed.drained);
    assert!(failed.next_attempt_at.is_none());
    assert!(
        failed
            .reason
            .as_deref()
            .is_some_and(|error| error.contains("before its input reached model context"))
    );
    assert_eq!(
        state
            .agent_router
            .event_phases_for_test(&target, "f1-cold-rejected")
            .await?,
        ["persisted", "claimed", "effect_started", "turn_settled"],
        "terminal-before-drain must not append a Drained fact"
    );
    assert!(state.agent_router.in_flight_claim(&target).await?.is_none());
    Ok(())
}
