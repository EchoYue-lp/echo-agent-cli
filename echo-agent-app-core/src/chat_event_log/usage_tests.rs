use super::*;
use echo_agent::agent::{
    AgentEvent, EventEnvelope, EventIdentity, ModelCallObservation, ModelCallOutcome,
};

fn usage(
    call: &str,
    attempt: &str,
    (input, output): (usize, usize),
    cached: usize,
    duration: u64,
    outcome: ModelCallOutcome,
    reported: bool,
) -> AgentEvent {
    AgentEvent::LlmUsage {
        model: "observed-model".into(),
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: input.saturating_add(output),
        cached_prompt_tokens: cached,
        cache_creation_prompt_tokens: 23,
        usage_reported: reported,
        observation: Some(ModelCallObservation {
            call_id: call.into(),
            attempt_id: attempt.into(),
            outcome,
            request_duration_ms: duration.saturating_add(900),
            first_output_ms: Some(800),
            last_output_ms: Some(800_u64.saturating_add(duration)),
            output_samples: 2,
            timing_comparable: true,
        }),
    }
}

fn append(log: &ChatEventLog, seq: u64, payload: AgentEvent) -> Result<(), String> {
    let identity = EventIdentity::for_chat(Some("conversation".into()), "turn", "turn", None)
        .and_then(|identity| identity.with_execution_id("execution"))
        .map_err(|error| error.to_string())?;
    let event =
        EventEnvelope::new(&identity, seq, None, payload).map_err(|error| error.to_string())?;
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Agent(Box::new(event)),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn configuration(log: &ChatEventLog) -> Result<(), String> {
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::TurnConfiguration {
            permission_mode: "default".into(),
            approval_policy: "default".into(),
            attachments: Vec::new(),
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn snapshot(log: &ChatEventLog) -> Result<ConversationUsageSnapshot, String> {
    loop {
        match log.conversation_usage("workspace", "conversation") {
            Err(ChatEventLogError::NativeHistoryPending { .. }) => continue,
            value => return value.map_err(|error| error.to_string()),
        }
    }
}

#[test]
fn original_usage_retry_duplicates_failure_and_weighted_timing() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    configuration(&log)?;
    let first = usage(
        "call-1",
        "attempt-1",
        (1000, 100),
        300,
        1000,
        ModelCallOutcome::Completed,
        true,
    );
    append(&log, 1, first.clone())?;
    append(
        &log,
        2,
        usage(
            "call-2",
            "attempt-2",
            (2000, 400),
            1900,
            2000,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    append(
        &log,
        3,
        usage(
            "call-2",
            "attempt-3",
            (500, 20),
            100,
            5,
            ModelCallOutcome::Failed,
            true,
        ),
    )?;
    // 重复运输可有不同 outer sequence；同一原 call/attempt 仍只计一次。
    append(&log, 4, first)?;
    append(
        &log,
        5,
        usage(
            "call-3",
            "attempt-4",
            (0, 0),
            0,
            0,
            ModelCallOutcome::Cancelled,
            false,
        ),
    )?;
    configuration(&log)?;
    let value = snapshot(&log)?;
    assert!(value.prefix_complete);
    assert_eq!(value.turns, 1);
    assert_eq!(
        (
            value.main.steps,
            value.main.attempts,
            value.main.missing_usage
        ),
        (3, 4, 1)
    );
    assert_eq!(
        (
            value.main.input_tokens,
            value.main.output_tokens,
            value.main.cached_tokens
        ),
        (3500, 520, 2300)
    );
    assert_eq!(value.main.cache_write_tokens, 69);
    assert_eq!(value.main.latest_speed, Some(200.0));
    assert_eq!(
        (
            value.main.timed_output_tokens,
            value.main.output_duration_ms
        ),
        (500, 3000)
    );
    assert_eq!(value.subagent.input_tokens, 0);
    // Fork 目标容器不扫描源 canonical 消息；目标实际原 journal 缺席就保持未知。
    let fork = log
        .conversation_usage("workspace", "fork-target")
        .map_err(|error| error.to_string())?;
    assert_eq!(fork.main.input_tokens, 0);
    assert!(!fork.prefix_complete);
    assert!(fork.journal_identity.is_none());
    Ok(())
}

#[test]
fn original_usage_recovers_anchor_before_coverage_crash_cut() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    configuration(&log)?;
    snapshot(&log)?;
    let stream =
        stream_id("workspace", Some("conversation"), "turn").map_err(|error| error.to_string())?;
    let path = log
        .stream_dir(&stream)
        .join(NATIVE_HISTORY_DIRECTORY)
        .join("coverage.json");
    let store = FileCheckpointStore::<NativePublicationCoverage>::open(&path);
    let before = store
        .load()
        .map_err(|error| error.to_string())?
        .ok_or("coverage missing")?;
    append(
        &log,
        1,
        usage(
            "call",
            "attempt",
            (300, 100),
            200,
            1000,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    let after = snapshot(&log)?;
    // 事实锚已落盘，但累计 coverage 仍是上次确认的状态。
    store
        .save(&before.journal_identity, &before.state, before.sequence)
        .map_err(|error| error.to_string())?;
    drop(log);
    let cold = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    let recovered = snapshot(&cold)?;
    assert_eq!(recovered.main, after.main);
    assert_eq!(snapshot(&cold)?.main.attempts, 1);
    Ok(())
}

#[test]
fn original_usage_survives_more_than_4096_events_prune_and_cold_reopen() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let retention = ChatEventRetention {
        segment_rollover_bytes: 1,
        max_segments: 2,
        max_replay_events: 4,
    };
    let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    configuration(&log)?;
    append(
        &log,
        1,
        usage(
            "call",
            "attempt",
            (1000, 100),
            800,
            1000,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    for _ in 0..4100 {
        log.append(
            "workspace",
            Some("conversation"),
            "turn",
            ChatDriverEvent::TurnStatus {
                status: "completed".into(),
            },
        )
        .map_err(|error| error.to_string())?;
    }
    let tail = log
        .replay("workspace", Some("conversation"), "turn", 0)
        .map_err(|error| error.to_string())?;
    assert!(tail.retained_earliest_cursor.is_some_and(|floor| floor > 2));
    assert!(tail.events.len() <= 4);
    drop(log);
    let cold = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    let recovered = snapshot(&cold)?;
    assert_eq!(recovered.main.input_tokens, 1000);
    assert_eq!(recovered.main.cached_tokens, 800);
    assert_eq!(recovered.turns, 1);
    assert!(recovered.prefix_complete);
    // 旧 publication coverage 没有 usage：不能凭旧 prefix 证明已裁的收费前缀。
    let stream =
        stream_id("workspace", Some("conversation"), "turn").map_err(|error| error.to_string())?;
    let path = cold
        .stream_dir(&stream)
        .join(NATIVE_HISTORY_DIRECTORY)
        .join("coverage.json");
    let store = FileCheckpointStore::<NativePublicationCoverage>::open(&path);
    let mut old = store
        .load()
        .map_err(|error| error.to_string())?
        .ok_or("coverage missing")?;
    old.state.usage = None;
    store
        .save(&old.journal_identity, &old.state, old.sequence)
        .map_err(|error| error.to_string())?;
    let partial = snapshot(&cold)?;
    assert!(!partial.prefix_complete);
    assert!(
        partial
            .first_observed_sequence
            .is_some_and(|floor| floor > 2)
    );
    assert_eq!(partial.main.input_tokens, 0);
    Ok(())
}

#[test]
fn original_usage_missing_timing_and_single_packet_are_unknown() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    let mut old = usage(
        "call",
        "attempt",
        (50, 10),
        5,
        0,
        ModelCallOutcome::Completed,
        true,
    );
    if let AgentEvent::LlmUsage { observation, .. } = &mut old {
        *observation = None;
    }
    append(&log, 1, old)?;
    let value = snapshot(&log)?;
    assert_eq!(value.main.unidentified_steps, 1);
    assert_eq!(value.main.latest_speed, None);
    assert_eq!(value.main.input_tokens, 50);
    Ok(())
}

#[test]
fn original_usage_tracks_an_unreported_inflight_call_without_fabricating_cost() -> Result<(), String>
{
    use echo_agent::agent::{AgentPhase, ExecutionStatus, ExecutionStatusKind};
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    configuration(&log)?;
    append(
        &log,
        1,
        AgentEvent::ExecutionStatus(ExecutionStatus {
            adapter_generated: false,
            execution_id: "execution".into(),
            request_id: Some("call".into()),
            sequence: 1,
            phase: AgentPhase::Thinking,
            status: ExecutionStatusKind::Waiting,
            reason: None,
        }),
    )?;
    let pending = snapshot(&log)?;
    assert_eq!(
        (
            pending.main.steps,
            pending.main.attempts,
            pending.main.pending_calls
        ),
        (1, 0, 1)
    );
    assert_eq!(pending.main.latest_speed, None);
    append(
        &log,
        2,
        usage(
            "call",
            "attempt",
            (100, 20),
            50,
            1000,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    let settled = snapshot(&log)?;
    assert_eq!(
        (
            settled.main.steps,
            settled.main.attempts,
            settled.main.pending_calls
        ),
        (1, 1, 0)
    );
    assert_eq!(settled.main.latest_speed, Some(20.0));
    Ok(())
}

#[test]
fn original_usage_separates_subagent_from_main_and_ignores_run_total_snapshots()
-> Result<(), String> {
    use crate::tasks::task_runtime::RuntimeEventKind;
    use crate::tasks::task_runtime::executor::ExecEvent;
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    let payload = serde_json::json!({"model":"child-model", "prompt_tokens":70, "completion_tokens":30,
        "total_tokens":100, "cached_prompt_tokens":20, "cache_creation_prompt_tokens":5,
        "usage_reported":true, "usage_event_id":"original-child-event"});
    let event = ExecEvent::subagent(
        "workspace",
        "conversation",
        "run",
        "task",
        "subagent-run",
        RuntimeEventKind::Usage,
        payload,
    );
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Execution(Box::new(event.clone())),
    )
    .map_err(|error| error.to_string())?;
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Execution(Box::new(event)),
    )
    .map_err(|error| error.to_string())?;
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Execution(Box::new(ExecEvent::run(
            "workspace",
            "conversation",
            "run",
            RuntimeEventKind::Usage,
            serde_json::json!({"total_tokens":100}),
        ))),
    )
    .map_err(|error| error.to_string())?;
    let value = snapshot(&log)?;
    assert_eq!(value.main.attempts, 0);
    assert_eq!(
        (
            value.subagent.attempts,
            value.subagent.input_tokens,
            value.subagent.output_tokens
        ),
        (1, 70, 30)
    );
    assert!(value.models.is_empty());
    Ok(())
}

#[test]
fn original_usage_compression_invalidates_context_but_never_refunds_cost() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let retention = ChatEventRetention::default();
    let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    configuration(&log)?;
    append(
        &log,
        1,
        usage(
            "first",
            "first-attempt",
            (1000, 100),
            400,
            1000,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    assert_eq!(snapshot(&log)?.main.latest_input_tokens, Some(1000));
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::ContextCompressed {
            before_count: 10,
            after_count: 3,
            before_tokens: 1000,
            after_tokens: 200,
        },
    )
    .map_err(|error| error.to_string())?;
    snapshot(&log)?;
    drop(log);
    let cold = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    let value = snapshot(&cold)?;
    assert_eq!(value.main.latest_input_tokens, None);
    assert_eq!(
        (value.main.input_tokens, value.main.output_tokens),
        (1000, 100)
    );
    append(
        &cold,
        2,
        usage(
            "second",
            "second-attempt",
            (0, 0),
            0,
            0,
            ModelCallOutcome::Completed,
            true,
        ),
    )?;
    assert_eq!(snapshot(&cold)?.main.latest_input_tokens, Some(0));
    Ok(())
}
