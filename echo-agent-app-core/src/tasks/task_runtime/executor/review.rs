#[allow(clippy::too_many_arguments)]
async fn execute_runtime_plan<W: TaskDispatcher + 'static>(
    store: Arc<TaskRuntimeStore>,
    dispatcher: W,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    limits: EkoExecutionLimits,
    parent_cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<RunOutcome, ExecError> {
    let blocking = TaskRuntimeBlockingAdapter::new(store.clone());
    let controller = Arc::new(EkoRuntimeDagController {
        store,
        blocking: blocking.clone(),
        dispatcher: Arc::new(dispatcher),
        reviewer_llm,
        write_sem: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
        shell_sem: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
        llm_sem: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        file_write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        trace_sink,
        cancel: parent_cancel.clone(),
        resolution_metadata: std::sync::Mutex::new(HashMap::new()),
        dispatch_failures: std::sync::Mutex::new(HashMap::new()),
    });
    let runtime_tasks = echo_agent::tasks::RuntimeTaskService::new(
        controller,
        echo_agent::tasks::RuntimeTaskServiceConfig {
            max_concurrent_subagents: limits.max_concurrent_subagents,
            ..Default::default()
        },
    );
    let outcome = runtime_tasks
        .execute(run_id, parent_cancel)
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    let terminal_status = match &outcome {
        echo_agent::tasks::RuntimeDagOutcome::Failed { .. } => Some(TaskRunStatus::Failed),
        echo_agent::tasks::RuntimeDagOutcome::Stalled { .. } => Some(TaskRunStatus::Failed),
        echo_agent::tasks::RuntimeDagOutcome::Paused { .. } => Some(TaskRunStatus::Paused),
        echo_agent::tasks::RuntimeDagOutcome::Cancelled => Some(TaskRunStatus::Cancelled),
        echo_agent::tasks::RuntimeDagOutcome::Completed => None,
    };
    if let Some(status) = terminal_status {
        let transition_run_id = run_id.to_string();
        blocking
            .run("transition runtime task run", move |store| {
                let current = store
                    .get_run(&transition_run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(transition_run_id.clone()))?;
                if current.status == status {
                    Ok(current)
                } else {
                    store.transition_run(&transition_run_id, status)
                }
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
    }
    Ok(match outcome {
        echo_agent::tasks::RuntimeDagOutcome::Completed => RunOutcome::Completed,
        echo_agent::tasks::RuntimeDagOutcome::Failed {
            failed_task_id,
            error,
        } => RunOutcome::Failed {
            failed_task_id: Some(failed_task_id),
            error,
        },
        echo_agent::tasks::RuntimeDagOutcome::Paused { task_id, reason } => RunOutcome::Paused {
            failed_task_id: task_id,
            error: reason,
        },
        echo_agent::tasks::RuntimeDagOutcome::Stalled { reason } => RunOutcome::Failed {
            failed_task_id: None,
            error: reason,
        },
        echo_agent::tasks::RuntimeDagOutcome::Cancelled => RunOutcome::Cancelled,
    })
}

#[allow(clippy::too_many_arguments)]
async fn integrate_reviewed_task<W: TaskDispatcher + 'static>(
    dispatcher: Arc<W>,
    store: Arc<TaskRuntimeStore>,
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    summary: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<(String, Vec<String>), String> {
    let integration = match dispatcher
        .integrate(
            store.clone(),
            blocking.clone(),
            run_id.to_string(),
            task.clone(),
            execution_id.to_string(),
            cancel,
            trace_sink,
        )
        .await
    {
        Ok(integration) => integration,
        Err(error) => return Err(error),
    };
    let Some(integration) = integration else {
        return Ok((summary.to_string(), Vec::new()));
    };

    let integration_summary = integration.summary();
    let changed_files = integration.changed_files.clone();
    Ok((format!("{summary} | {integration_summary}"), changed_files))
}

/// Outcome of the review gate over a freshly-completed task.
#[allow(clippy::large_enum_variant)] // PlanTask is Clone and short-lived in the review path; Box would add indirection with no win
enum ReviewGateOutcome {
    /// Task passed review (or is read-only and self-reviewing). Mark Completed.
    Pass(Option<ReviewResult>),
    /// Review found fixable issues. The claim-bound review candidate is
    /// published with a typed Blocked settlement; only an explicit retry may
    /// restart the task.
    NeedsFix(PlanTask, ReviewResult),
    /// Circuit breaker tripped (retry budget exhausted or repeated fingerprint).
    /// The run should be Suspended.
    Suspend {
        reason: String,
        review: Option<ReviewResult>,
    },
    /// No reviewer LLM configured. M7 requires a stop rather than auto-pass.
    Skipped,
}

/// Run the review gate for a task that just finished executing. Read-only
/// kinds auto-pass; implementation/debugging kinds are reviewed by the LLM
/// (when available) against the domain checklist. Applies the circuit
/// breaker on NeedsFix/Blocked outcomes.
async fn run_review_gate(
    blocking: TaskRuntimeBlockingAdapter,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    task: &PlanTask,
    subagent_output: &str,
) -> ReviewGateOutcome {
    // Skip the LLM gate when the task declares no acceptance criteria
    // AND is not an implementation/debugging kind (those are always gated
    // because prose about mutations cannot be trusted).
    if !super::review::requires_review(task) {
        return ReviewGateOutcome::Pass(None);
    }
    let Some(llm) = reviewer_llm else {
        return ReviewGateOutcome::Skipped;
    };

    // Retry transient review errors (LLM 5xx/timeout, JSON parse failures) up to
    // 2 times before suspending. Transient failures are expected in production
    // and should not block the run on the first hiccup.
    const MAX_REVIEW_RETRIES: u32 = 2;
    let mut retries: u32 = 0;
    let review = loop {
        match super::review::review_task(&llm, run_id, task, subagent_output).await {
            Ok(review) => break review,
            Err(e) => {
                retries += 1;
                if retries <= MAX_REVIEW_RETRIES {
                    tracing::warn!(
                        task_id = %task.id,
                        attempt = retries,
                        error = %e,
                        "review gate transient error, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    continue;
                }
                // Exhausted retries. Do NOT auto-pass — that would let
                // unreviewed mutating work through. Surface the error so the
                // user can retry, lower the standard, or intervene.
                let reason = format!(
                    "review gate failed after {MAX_REVIEW_RETRIES} retries ({e}); run suspended pending user input"
                );
                return ReviewGateOutcome::Suspend {
                    reason,
                    review: None,
                };
            }
        }
    };

    match review.outcome {
        ReviewOutcome::Pass => ReviewGateOutcome::Pass(Some(review)),
        ReviewOutcome::NeedsFix => {
            let prior_run_id = review.run_id.clone();
            let prior_task_id = task.id.clone();
            let mut prior = match blocking
                .run("load runtime task review history", move |store| {
                    store.list_reviews(&prior_run_id, &prior_task_id)
                })
                .await
            {
                Ok(prior) => prior,
                Err(error) => {
                    return ReviewGateOutcome::Suspend {
                        reason: format!("review history unavailable: {error}"),
                        review: Some(review),
                    };
                }
            };
            prior.push(review.clone());
            match super::review::circuit_breaker_action_from_prior(task, &review, &prior, 2) {
                super::review::BreakerAction::CreateFix => ReviewGateOutcome::NeedsFix(
                    super::review::build_fix_task(task, &review),
                    review,
                ),
                super::review::BreakerAction::Suspend { reason } => ReviewGateOutcome::Suspend {
                    reason,
                    review: Some(review),
                },
            }
        }
        ReviewOutcome::Blocked => ReviewGateOutcome::Suspend {
            reason: "review returned blocked".to_string(),
            review: Some(review),
        },
    }
}
