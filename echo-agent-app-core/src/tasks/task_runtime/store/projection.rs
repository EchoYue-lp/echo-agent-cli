fn task_status_runtime_event(event: TaskStatusEvent<'_>) -> RuntimeJournalEvent {
    let TaskStatusEvent {
        run_id,
        task_id,
        task_subject,
        status,
        owner_agent,
        summary,
        claim,
    } = event;
    let now = echo_agent::utils::time::now_local().to_rfc3339();
    let started = status.is_running();
    let finished = status.is_terminal();
    let kind = runtime_task_event_kind(&status);
    let (status_name, status_detail) = task_status_wire(&status);
    RuntimeJournalEvent::for_append(
        run_id,
        Some(task_id),
        None,
        kind,
        serde_json::json!({
            "status": status_name,
            "status_detail": status_detail,
            "owner_agent": owner_agent,
            "title": task_subject,
            "summary": summary,
            "claim": claim,
            "started_at": if started { Some(now.as_str()) } else { None },
            "completed_at": if finished { Some(now.as_str()) } else { None },
        }),
    )
}

fn apply_subagent_projection_event(
    runs: &mut std::collections::BTreeMap<String, SubagentRunSnapshot>,
    run_id: &str,
    limit: usize,
    exact_execution_id: Option<&str>,
    event: RuntimeTaskEvent,
) {
    if let Some(recovery) = boot_recovery_payload(&event)
        && let Some(subagents) = recovery
            .get("subagents")
            .and_then(serde_json::Value::as_array)
    {
        for recovered in subagents {
            let Some(execution_id) = json_string(recovered, "execution_id") else {
                continue;
            };
            let Some(snapshot) = runs.get_mut(&execution_id) else {
                continue;
            };
            snapshot.run.status = json_string(recovered, "status")
                .as_deref()
                .and_then(|value| value.parse().ok())
                .unwrap_or(SubagentStatus::Failed);
        }
    }
    let Some(execution_id) = event.step_id.clone() else {
        return;
    };
    if exact_execution_id.is_some_and(|wanted| wanted != execution_id) {
        return;
    }
    if event.event_type == RuntimeEventKind::SubagentAssigned {
        let Some(task_id) = event.task_id.clone() else {
            return;
        };
        let Some(subagent_name) = json_string(&event.payload, "agent_name") else {
            return;
        };
        if exact_execution_id.is_none() && !runs.contains_key(&execution_id) && runs.len() >= limit
        {
            let Some(last_key) = runs.last_key_value().map(|(key, _)| key.clone()) else {
                return;
            };
            if execution_id >= last_key {
                return;
            }
            runs.remove(&last_key);
        }
        let attempt = event
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1);
        let plan_revision = event
            .payload
            .get("plan_revision")
            .and_then(serde_json::Value::as_u64);
        runs.insert(
            execution_id.clone(),
            SubagentRunSnapshot {
                run: SubagentRun::new(execution_id, run_id, task_id, subagent_name, attempt),
                plan_revision,
                latest_event: event,
            },
        );
        return;
    }
    let Some(snapshot) = runs.get_mut(&execution_id) else {
        return;
    };
    snapshot.latest_event = event.clone();
    match event.event_type {
        RuntimeEventKind::RunTurnUsageAccounted
            if json_string(&event.payload, "source_scope").as_deref() == Some("subagent") =>
        {
            let tokens = event
                .payload
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .saturating_add(
                    event
                        .payload
                        .get("output_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                );
            snapshot.run.usage.tokens_used = Some(
                snapshot
                    .run
                    .usage
                    .tokens_used
                    .unwrap_or(0)
                    .saturating_add(tokens),
            );
            let duration_ms = event
                .payload
                .get("duration_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            snapshot.run.usage.duration_ms = Some(
                snapshot
                    .run
                    .usage
                    .duration_ms
                    .unwrap_or(0)
                    .saturating_add(duration_ms),
            );
        }
        RuntimeEventKind::SubagentReleased => {
            if let Some(status) = json_string(&event.payload, "status")
                .as_deref()
                .and_then(|value| value.parse().ok())
            {
                snapshot.run.status = status;
            }
            if let Some(result) = event
                .payload
                .get("outcome")
                .cloned()
                .and_then(|value| serde_json::from_value::<SubagentOutcome>(value).ok())
            {
                snapshot.run.outcome = Some(result);
            }
            if let Some(usage) = event
                .payload
                .get("usage")
                .cloned()
                .and_then(|value| serde_json::from_value::<ExecutionUsage>(value).ok())
            {
                snapshot.run.usage = usage;
            }
        }
        _ => {}
    }
}

fn boot_recovery_payload(event: &RuntimeTaskEvent) -> Option<&serde_json::Value> {
    event.payload.get("recovery").filter(|recovery| {
        recovery.get("kind").and_then(serde_json::Value::as_str) == Some("boot_recovery")
    })
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

fn bounded_event_text(value: &str, max_chars: usize) -> String {
    let mut text = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        text.push_str("...");
    }
    text
}

fn validate_plan_goal_binding(run: &TaskRun, plan: &TaskPlan) -> Result<(), StoreError> {
    if plan.goal_revision == run.goal_revision && plan.goal_sha256 == run.goal_sha256 {
        return Ok(());
    }
    Err(StoreError::PlanGoalMismatch {
        run_id: run.run_id.clone(),
        plan_revision: plan.revision,
        plan_goal_revision: plan.goal_revision,
        run_goal_revision: run.goal_revision,
    })
}

// The compile-time test that proves the transaction invariant:
// a state change without an event would leave the DB inconsistent.
// We assert both rows land together.
