use super::*;
use echo_agent::agent::{AssistantMessageBlock, AssistantPhase, EventEnvelope, EventIdentity};

fn native_block(phase: AssistantPhase, delta: &str, ordinal: u64) -> AgentEvent {
    AgentEvent::AssistantMessage {
        block: AssistantMessageBlock {
            provider_source: None,
            block_id: format!("block-{ordinal}"),
            request_id: "request-native".into(),
            sequence: ordinal,
            phase,
            delta: delta.into(),
            complete: true,
            provider: Some("canonical-fixture".into()),
            reasoning: None,
        },
    }
}

async fn original_sink(
    store: Arc<TaskRuntimeStore>,
) -> anyhow::Result<(
    EkoAgentTurnSink,
    TaskRun,
    PlanTask,
    String,
    Arc<std::sync::Mutex<Vec<ExecEvent>>>,
)> {
    let run_id = "native-projection-run";
    store.create_run(
        run_id,
        "native-workspace",
        "native-conversation",
        "native-message",
        DomainProfile::General,
        "original goal",
        "",
        AttendedMode::Attended,
    )?;
    let task = PlanTask {
        id: "native-task".into(),
        title: "inspect".into(),
        agent_role: "primary".into(),
        ..Default::default()
    };
    store.attach_plan_for_test(&TaskPlan {
        domain_binding: None,
        plan_id: "native-plan".into(),
        run_id: run_id.into(),
        revision: 1,
        domain_profile: DomainProfile::General,
        goal_revision: 1,
        goal_sha256: crate::tasks::task_runtime::task_goal_sha256("original goal"),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![task.clone()],
    })?;
    store.transition_run(run_id, TaskRunStatus::Running)?;
    let execution = format!("{run_id}:{}:1:1", task.id);
    store.record_subagent_assigned(
        run_id, &task.id, &execution, "primary", "inspect", 1, 1, false, false,
    )?;
    let run = store
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("run missing"))?;
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = observed.clone();
    let trace: ExecSink = Arc::new(move |event| {
        capture
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event)
    });
    let sink = EkoAgentTurnSink::for_primary_task(
        &run,
        &task,
        &execution,
        TaskRuntimeOperation::new(store),
        HashSet::new(),
        Some(trace),
    );
    Ok((sink, run, task, execution, observed))
}

#[tokio::test]
async fn native_sink_preserves_envelope_order_and_reopens_original_journal_without_settling_task()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let store = Arc::new(TaskRuntimeStore::open_for_workspace(
        root.path(),
        "native-workspace",
    )?);
    let resources = store.test_resource_scope();
    let (sink, run, task, execution, observed) = original_sink(store.clone()).await?;
    let identity = EventIdentity::for_chat(
        Some(run.conversation_id.clone()),
        &execution,
        "native-message",
        Some(run.run_id.clone()),
    )?
    .with_execution_id(&execution)?;
    let mut reasoning = native_block(AssistantPhase::Reasoning, "", 2);
    if let AgentEvent::AssistantMessage { block } = &mut reasoning {
        block.reasoning = Some(echo_agent::llm::types::ReasoningBlock::Signed {
            thinking: "原生思考".into(),
            signature: "original-signature".into(),
        });
    }
    let payloads = vec![
        native_block(AssistantPhase::Commentary, "开始检查", 1),
        reasoning,
        AgentEvent::ToolCall {
            call_id: "native-call".into(),
            invocation: echo_agent::agent::ToolInvocation {
                requested_name: "read_file".into(),
                requested_args: serde_json::json!({"path":"README.md"}),
                name: "read_file".into(),
                args: serde_json::json!({"path":"README.md"}),
                rewrites: Vec::new(),
            },
        },
        native_block(AssistantPhase::PartialAnswer, "已找到边界", 4),
        AgentEvent::FinalAnswer("原执行确认回答".into()),
    ];
    let mut expected = Vec::new();
    for (index, payload) in payloads.into_iter().enumerate() {
        let envelope = EventEnvelope::new(
            &identity,
            u64::try_from(index)?.saturating_add(1),
            None,
            payload,
        )?;
        if !matches!(&envelope.payload, AgentEvent::ToolCall { .. }) {
            expected.push(serde_json::to_value(&envelope)?);
        }
        sink.on_event(envelope.clone()).await?;
        if index == 0 {
            // Lose the in-memory dedup cache after the durable commit: a new
            // original sink must not append a second journal observation.
            let resumed = EkoAgentTurnSink::for_primary_task(
                &run,
                &task,
                &execution,
                TaskRuntimeOperation::new(store.clone()),
                HashSet::new(),
                None,
            );
            resumed.on_event(envelope.clone()).await?;
            let mut conflicting = envelope.clone();
            conflicting.timestamp = conflicting
                .timestamp
                .checked_add_signed(chrono::Duration::seconds(1))
                .ok_or_else(|| anyhow::anyhow!("fixture timestamp overflow"))?;
            assert!(sink.on_event(conflicting.clone()).await.is_err());
            let rebuilt = EkoAgentTurnSink::for_primary_task(
                &run,
                &task,
                &execution,
                TaskRuntimeOperation::new(store.clone()),
                HashSet::new(),
                None,
            );
            assert!(rebuilt.on_event(conflicting).await.is_err());
            sink.on_event(envelope).await?;
        }
    }
    let live = observed
        .lock()
        .map_err(|_| anyhow::anyhow!("observer poisoned"))?
        .clone();
    assert_eq!(
        live.iter().map(|event| event.event).collect::<Vec<_>>(),
        vec![
            RuntimeEventKind::Note,
            RuntimeEventKind::Note,
            RuntimeEventKind::ToolStarted,
            RuntimeEventKind::Note,
            RuntimeEventKind::Note
        ]
    );
    let notes = live
        .iter()
        .filter(|event| event.event == RuntimeEventKind::Note)
        .map(|event| {
            event
                .payload
                .get("agent_envelope")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("native envelope lost"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert_eq!(notes, expected);
    assert_eq!(
        store
            .get_run(&run.run_id)?
            .ok_or_else(|| anyhow::anyhow!("run lost"))?
            .status,
        TaskRunStatus::Running
    );
    assert!(
        live.iter()
            .all(|event| event.scope == ExecEventScope::Subagent
                && event.task_id.as_deref() == Some(task.id.as_str())
                && event.subagent_run_id.as_deref() == Some(execution.as_str()))
    );
    drop(sink);
    drop(store);
    drop(resources);
    let reopened = TaskRuntimeStore::open_for_workspace(root.path(), "native-workspace")?;
    let replay = reopened.list_events(&run.run_id, 0)?;
    let durable = replay
        .iter()
        .filter(|event| {
            event.event_type == RuntimeEventKind::Note
                && event
                    .payload
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("native_agent_event")
        })
        .map(|event| {
            event
                .payload
                .get("agent_envelope")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("replayed envelope lost"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert_eq!(durable, expected);
    Ok(())
}

#[tokio::test]
async fn native_sink_status_error_cancel_and_wrong_owner_never_promote_original_lifecycle()
-> anyhow::Result<()> {
    let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
    let _resources = store.test_resource_scope();
    let (sink, run, _, execution, observed) = original_sink(store.clone()).await?;
    let identity = EventIdentity::for_chat(
        Some(run.conversation_id.clone()),
        &execution,
        "native-message",
        Some(run.run_id.clone()),
    )?
    .with_execution_id(&execution)?;
    let events = [
        AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
            execution_id: execution.clone(),
            request_id: None,
            sequence: 1,
            phase: AgentEvent::Cancelled.phase(),
            status: echo_agent::agent::ExecutionStatusKind::Settled,
            reason: None,
            adapter_generated: false,
        }),
        AgentEvent::Cancelled,
        AgentEvent::error_message("native", "unsettled failure"),
    ];
    for (index, payload) in events.into_iter().enumerate() {
        sink.on_event(EventEnvelope::new(
            &identity,
            u64::try_from(index)?.saturating_add(1),
            None,
            payload,
        )?)
        .await?;
    }
    let wrong = EventIdentity::for_chat(
        Some(run.conversation_id.clone()),
        "other-turn",
        "native-message",
        Some("other-run".into()),
    )?
    .with_execution_id(&execution)?;
    assert!(
        sink.on_event(EventEnvelope::new(
            &wrong,
            1,
            None,
            native_block(AssistantPhase::Commentary, "must reject", 1)
        )?)
        .await
        .is_err()
    );
    for missing_run in [true, false] {
        let mut missing = EventEnvelope::new(
            &identity,
            10,
            None,
            native_block(AssistantPhase::Commentary, "missing owner", 10),
        )?;
        if missing_run {
            missing.run_id = None;
        } else {
            missing.conversation_id = None;
        }
        assert!(sink.on_event(missing).await.is_err());
    }
    let wrong_execution = EventIdentity::for_chat(
        Some(run.conversation_id.clone()),
        "wrong-turn",
        "native-message",
        Some(run.run_id.clone()),
    )?
    .with_execution_id("foreign-execution")?;
    assert!(
        sink.on_event(EventEnvelope::new(
            &wrong_execution,
            11,
            None,
            native_block(AssistantPhase::Commentary, "wrong execution", 11)
        )?)
        .await
        .is_err()
    );
    assert_eq!(
        observed
            .lock()
            .map_err(|_| anyhow::anyhow!("observer poisoned"))?
            .len(),
        3
    );
    assert_eq!(
        store
            .get_run(&run.run_id)?
            .ok_or_else(|| anyhow::anyhow!("run lost"))?
            .status,
        TaskRunStatus::Running
    );
    Ok(())
}
