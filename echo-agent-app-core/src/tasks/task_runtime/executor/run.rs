/// Execute a planned run to completion.
///
/// The caller (a Tauri command) holds the `AppState`, the store, and the
/// optional `AgentPool`. Execution is driven on the provided runtime; the
/// caller typically `tokio::spawn`s this and lets it run independently of the
/// chat stream (so a long run does not block the GUI, per plan §4).
#[allow(clippy::too_many_arguments)] // many typed handles + concurrency primitives; grouping would fragment the read path
pub async fn execute_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: Option<crate::agent_handle::AgentHandle>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    memory_generation: Option<crate::evolution::ReviewGenerationLease>,
    run_store: Option<Arc<dyn echo_agent::trace::RunStore>>,
    trace_sink: Option<ExecSink>,
    run_id: &str,
    parent_cancel: CancellationToken,
    memory_policy: super::memory_bridge::MemoryPolicy,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<RunOutcome, ExecError> {
    let blocking = TaskRuntimeOperation::new(store.clone());
    let initial_run_id = run_id.to_string();
    let (run, initial_plan) = blocking
        .run("load runtime execution admission", move |store| {
            let run = store
                .get_run(&initial_run_id)?
                .ok_or_else(|| StoreError::RunNotFound(initial_run_id.clone()))?;
            let plan = store
                .get_plan(&initial_run_id)?
                .ok_or(StoreError::PlanNotFound(initial_run_id))?;
            Ok((run, plan))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    // The caller must have transitioned Pending → Running before spawning
    // the executor. Here we only accept Running.
    if run.status != TaskRunStatus::Running {
        return Err(ExecError::NotRunning(run_id.to_string(), run.status));
    }
    tracing::info!(
        run_id = %run_id,
        task_count = initial_plan.tasks.len(),
        status = %run.status.as_str(),
        route = %run.route,
        "task_runtime: execute_run start"
    );
    emit_exec(
        trace_sink.as_ref(),
        ExecEvent::run(
            run.workspace_id.clone(),
            run.conversation_id.clone(),
            run_id.to_string(),
            RuntimeEventKind::RunStarted,
            serde_json::json!({
                "goal": &run.goal,
                "conversation_id": &run.conversation_id,
                "mode": "task_runtime",
            }),
        ),
    );

    let primary_agent = primary_agent.ok_or(ExecError::NoAgent)?;
    let limits = EkoExecutionLimits::default();

    let mut drain_cycle = 0usize;
    let outcome = loop {
        let plan_run_id = run_id.to_string();
        let plan = blocking
            .run("load runtime drain plan", move |store| {
                store
                    .get_plan(&plan_run_id)?
                    .ok_or(StoreError::PlanNotFound(plan_run_id))
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        let unresolved_count = plan
            .tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .count();
        if unresolved_count == 0 {
            let report_run_id = run_id.to_string();
            let report = blocking
                .run("load runtime completion gate", move |store| {
                    store.completion_gate_report(&report_run_id)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            if !report.ready {
                let error = report
                    .blockers
                    .iter()
                    .map(|item| format!("{:?}: {}", item.code, item.detail))
                    .collect::<Vec<_>>()
                    .join("; ");
                let pause_run_id = run_id.to_string();
                let pause_error = error.clone();
                blocking
                    .run("pause rejected runtime completion", move |store| {
                        store
                            .request_pause_with_reason(
                                &pause_run_id,
                                RunPauseReason::NeedsInput,
                                Some(&pause_error),
                            )
                            .map(|_| ())
                    })
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break Ok(RunOutcome::Paused {
                    failed_task_id: None,
                    error,
                });
            }
            let complete_run_id = run_id.to_string();
            if blocking
                .run("commit quiescent runtime completion", move |store| {
                    store.complete_run_if_quiescent(&complete_run_id)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?
            {
                break Ok(RunOutcome::Completed);
            }
            drain_cycle = drain_cycle.saturating_add(1);
            continue;
        }
        tracing::info!(
            run_id = %run_id,
            drain_cycle,
            task_count = plan.tasks.len(),
            unresolved_count,
            "task_runtime: drain plan snapshot"
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            RealTaskDispatcher {
                primary_agent: primary_agent.clone(),
                workspace_io: workspace_io.clone(),
            },
            reviewer_llm.clone(),
            run_id,
            limits,
            parent_cancel.clone(),
            trace_sink.clone(),
        )
        .await;

        if matches!(outcome, Ok(RunOutcome::Completed)) {
            // Always return to the locked completion gate. This closes the
            // race where a plan patch commits after the last wave snapshot but
            // before the run is marked Completed.
            drain_cycle = drain_cycle.saturating_add(1);
            tracing::info!(
                run_id = %run_id,
                drain_cycle,
                "task_runtime: appended tasks detected after completed snapshot; continuing drain"
            );
            continue;
        }

        break outcome;
    };
    // Reflect the outcome on the run state. The durable status is the single
    // authority: each branch settles first, then emits exactly the status that
    // landed. This is what keeps the live event stream and the on-disk
    // summary/terminal in agreement when cancellation races the executor.
    match &outcome {
        Ok(RunOutcome::Completed) => {
            let status_run_id = run_id.to_string();
            let goal_completed = blocking
                .run("inspect runtime Goal completion", move |store| {
                    store
                        .get_run(&status_run_id)?
                        .map(|run| run.status == TaskRunStatus::Completed)
                        .ok_or(StoreError::RunNotFound(status_run_id))
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            if !goal_completed {
                // The active RunTurn owns the atomic RunTurnFinished + Goal
                // completion batch and publishes the terminal projection.
                return outcome;
            }
            emit_committed_run_event(
                trace_sink.as_ref(),
                &run,
                TaskRunStatus::Completed,
                None,
                None,
            );
            // With an active primary RunTurn, Goal completion is committed by
            // turn_lifecycle in the same batch as RunTurnFinished. Without an
            // active turn, complete_run_if_quiescent committed it above.
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "completed",
            );
            super::memory_bridge::write_memory_candidate_dispatch(
                memory_policy,
                memory_generation.as_ref(),
                &store,
                super::memory_bridge::MemoryEvent::RunCompleted {
                    run_id: run_id.to_string(),
                    goal: run.goal.clone(),
                },
            )
            .await;
        }
        Ok(RunOutcome::Failed {
            failed_task_id,
            error,
        }) => {
            let final_run_id = run_id.to_string();
            let final_task_id = failed_task_id.clone();
            let final_error = format!("run failed: {error}");
            let settled = blocking
                .run("finalize failed runtime run", move |store| {
                    store
                        .finalize_run_with_note_task(
                            &final_run_id,
                            TaskRunStatus::Failed,
                            final_task_id.as_deref(),
                            Some(&final_error),
                        )
                        .map(|run| run.status)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            emit_committed_run_event(
                trace_sink.as_ref(),
                &run,
                settled,
                failed_task_id.clone(),
                Some(error.as_str()),
            );
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                trace_label(settled),
            );
            // A late failure must not overwrite a terminal run that already
            // won; the committed status decides the closure.
            if settled != TaskRunStatus::Failed
                && let Some(committed) =
                    committed_outcome(settled, failed_task_id.clone(), error.clone())
            {
                return Ok(committed);
            }
        }
        Ok(RunOutcome::Cancelled) => {
            let final_run_id = run_id.to_string();
            let settled = blocking
                .run("finalize cancelled runtime run", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Cancelled, None)
                        .map(|run| run.status)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            emit_committed_run_event(trace_sink.as_ref(), &run, settled, None, None);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                trace_label(settled),
            );
            if settled == TaskRunStatus::Cancelled {
                super::memory_bridge::write_memory_candidate_dispatch(
                    memory_policy,
                    memory_generation.as_ref(),
                    &store,
                    super::memory_bridge::MemoryEvent::RunCancelledByUser {
                        run_id: run_id.to_string(),
                        goal: run.goal.clone(),
                    },
                )
                .await;
            } else {
                if let Some(committed) = committed_outcome(settled, None, String::new()) {
                    return Ok(committed);
                }
            }
        }
        Ok(RunOutcome::Paused {
            failed_task_id,
            error,
        }) => {
            let note_run_id = run_id.to_string();
            let note_task_id = failed_task_id.clone();
            let note = format!("run paused: {error}");
            let settled = blocking
                .run("note paused runtime run", move |store| {
                    store.note(&note_run_id, note_task_id.as_deref(), &note)?;
                    store
                        .get_run(&note_run_id)?
                        .map(|run| run.status)
                        .ok_or(StoreError::RunNotFound(note_run_id))
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            emit_committed_run_event(
                trace_sink.as_ref(),
                &run,
                settled,
                failed_task_id.clone(),
                Some(error.as_str()),
            );
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                trace_label(settled),
            );
            if settled != TaskRunStatus::Paused
                && let Some(committed) =
                    committed_outcome(settled, failed_task_id.clone(), error.clone())
            {
                return Ok(committed);
            }
        }
        Err(ExecError::Framework(echo_agent::error::ReactError::ExecutionNotSettled { .. })) =>
        {
            // Observation is not a run resolution or a retry instruction.
        }
        Err(e) => {
            let final_run_id = run_id.to_string();
            let final_error = format!("executor error: {e}");
            let settled = blocking
                .run("finalize runtime executor error", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Failed, Some(&final_error))
                        .map(|run| run.status)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            let error_text = e.to_string();
            emit_committed_run_event(
                trace_sink.as_ref(),
                &run,
                settled,
                None,
                Some(error_text.as_str()),
            );
            if let Some(committed) = committed_outcome(settled, None, String::new()) {
                return Ok(committed);
            }
        }
    }
    outcome
}

/// Outcome variant matching the durable terminal status. Non-terminal statuses
/// keep the original executor outcome.
fn committed_outcome(
    status: TaskRunStatus,
    failed_task_id: Option<String>,
    error: String,
) -> Option<RunOutcome> {
    match status {
        TaskRunStatus::Completed => Some(RunOutcome::Completed),
        TaskRunStatus::Cancelled => Some(RunOutcome::Cancelled),
        TaskRunStatus::Failed => Some(RunOutcome::Failed {
            failed_task_id,
            error,
        }),
        TaskRunStatus::Paused => Some(RunOutcome::Paused {
            failed_task_id,
            error,
        }),
        TaskRunStatus::Pending | TaskRunStatus::Running => None,
    }
}

/// Stable trace label for one durable run status.
fn trace_label(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Completed => "completed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Pending => "pending",
        TaskRunStatus::Running => "running",
    }
}

/// Emit the live event that matches the committed run status.
///
/// The event kind and payload are derived from the durable status rather than
/// from the executor's requested outcome, so a cancelled run never streams a
/// completion payload (and vice versa).
fn emit_committed_run_event(
    sink: Option<&ExecSink>,
    run: &TaskRun,
    settled: TaskRunStatus,
    failed_task_id: Option<String>,
    error: Option<&str>,
) {
    let payload = serde_json::json!({
        "status": settled.as_str(),
        "failed_task_id": failed_task_id,
        "error": error,
    });
    let kind = match settled {
        TaskRunStatus::Completed => RuntimeEventKind::RunCompleted,
        TaskRunStatus::Failed => RuntimeEventKind::RunFailed,
        TaskRunStatus::Cancelled => RuntimeEventKind::RunCancelled,
        TaskRunStatus::Paused | TaskRunStatus::Pending | TaskRunStatus::Running => {
            RuntimeEventKind::RunStatusChanged
        }
    };
    emit_exec(
        sink,
        ExecEvent::run(
            run.workspace_id.clone(),
            run.conversation_id.clone(),
            run.run_id.clone(),
            kind,
            payload,
        ),
    );
}
