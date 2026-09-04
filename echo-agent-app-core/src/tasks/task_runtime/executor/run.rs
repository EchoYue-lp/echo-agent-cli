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
    // Reflect the outcome on the run state. Each branch also writes a trace
    // Run record when a RunStore is available.
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
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunCompleted,
                    serde_json::json!({ "status": "completed" }),
                ),
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
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunFailed,
                    serde_json::json!({
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            let final_run_id = run_id.to_string();
            let final_task_id = failed_task_id.clone();
            let final_error = format!("run failed: {error}");
            blocking
                .run("finalize failed runtime run", move |store| {
                    store
                        .finalize_run_with_note_task(
                            &final_run_id,
                            TaskRunStatus::Failed,
                            final_task_id.as_deref(),
                            Some(&final_error),
                        )
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "failed",
            );
        }
        Ok(RunOutcome::Cancelled) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled" }),
                ),
            );
            let final_run_id = run_id.to_string();
            blocking
                .run("finalize cancelled runtime run", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Cancelled, None)
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "cancelled",
            );
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
        }
        Ok(RunOutcome::Paused {
            failed_task_id,
            error,
        }) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "status": "paused",
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            let note_run_id = run_id.to_string();
            let note_task_id = failed_task_id.clone();
            let note = format!("run paused: {error}");
            blocking
                .run("note paused runtime run", move |store| {
                    store.note(&note_run_id, note_task_id.as_deref(), &note)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "paused",
            );
        }
        Err(e) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunFailed,
                    serde_json::json!({ "error": e.to_string() }),
                ),
            );
            let final_run_id = run_id.to_string();
            let final_error = format!("executor error: {e}");
            blocking
                .run("finalize runtime executor error", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Failed, Some(&final_error))
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
        }
    }
    outcome
}
