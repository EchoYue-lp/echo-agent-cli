#[cfg(test)]
fn run_completion_blockers(store: &TaskRuntimeStore, run_id: &str) -> Vec<String> {
    store
        .completion_gate_report(run_id)
        .map(|report| {
            report
                .blockers
                .into_iter()
                .map(|item| item.detail)
                .collect()
        })
        .unwrap_or_else(|error| vec![error.to_string()])
}

/// Structured completion assessment. Separates "real execution failure"
/// (retryable) from "completed but acceptance pending" (NOT retryable —
/// must be blocked for review or user retry). contract_version=0 is no
/// longer a failure condition (M7 does not require it).
#[derive(Debug)]
enum CompletionAssessment {
    /// Subagent completed and all execution_checks / required_artifacts
    /// have hard observed evidence. Acceptance criteria are NOT judged
    /// here — that is the ReviewGate's job.
    Executed,
    /// Subagent genuinely failed (non-completed status, empty summary,
    /// remaining_work non-empty, or self-reported failed verification).
    /// This IS retryable within the retry budget.
    ExecutionFailed { reason: String },
    /// Subagent completed but execution evidence or artifacts are missing.
    /// NOT retryable — would just reproduce the same gap. Block instead.
    AcceptancePending {
        missing_checks: Vec<String>,
        missing_artifacts: Vec<String>,
    },
}

/// Assess whether a task's execution result is acceptable on hard-evidence
/// grounds (execution_checks must have observed pass; artifacts must be
/// present with hash + producer id). Acceptance criteria are intentionally
/// NOT judged here — they are reviewer-judged in the ReviewGate, never
/// auto-passed.
///
/// M7 note: contract_version=0 is a valid fallback shape. We do not treat
/// it as a failure. A plain-text summary is still legitimate execution
/// evidence as long as execution_checks (which are shell commands) are
/// empty or actually observed.
fn assess_task_execution(task: &PlanTask, result: &SubagentTaskResult) -> CompletionAssessment {
    // 1. Real execution failure: non-completed status, empty summary,
    //    self-reported remaining work, or self-reported failed verification.
    if result.status != SubagentRunStatus::Completed {
        return CompletionAssessment::ExecutionFailed {
            reason: format!("terminal status is {}", result.status.as_str()),
        };
    }
    if result.summary.trim().is_empty() {
        return CompletionAssessment::ExecutionFailed {
            reason: "summary is empty".to_string(),
        };
    }
    if !result.remaining_work.is_empty() {
        return CompletionAssessment::ExecutionFailed {
            reason: format!("remaining work: {}", result.remaining_work.join("; ")),
        };
    }
    for verification in &result.verification {
        if verification.status != SubagentVerificationStatus::Passed {
            return CompletionAssessment::ExecutionFailed {
                reason: format!(
                    "verification '{}' is {:?}",
                    verification.check, verification.status
                ),
            };
        }
    }

    // 2. execution_checks must have observed + passed evidence.
    let mut missing_checks = Vec::new();
    for required in &task.execution_checks {
        let matched = result.verification.iter().any(|verification| {
            verification.source == SubagentVerificationSource::Observed
                && verification.status == SubagentVerificationStatus::Passed
                && verification_matches(required, &verification.check)
        });
        if !matched {
            missing_checks.push(required.clone());
        }
    }

    // 3. required_artifacts must be present with hash + producer execution id.
    let mut missing_artifacts = Vec::new();
    for required in &task.required_artifacts {
        let matched = result.artifacts.iter().any(|artifact| {
            artifact_matches(required, &artifact.path)
                && artifact.available
                && artifact
                    .sha256
                    .as_deref()
                    .is_some_and(|hash| hash.chars().count() == 64)
                && artifact
                    .producer_execution_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
        });
        if !matched {
            missing_artifacts.push(required.clone());
        }
    }

    if missing_checks.is_empty() && missing_artifacts.is_empty() {
        CompletionAssessment::Executed
    } else {
        CompletionAssessment::AcceptancePending {
            missing_checks,
            missing_artifacts,
        }
    }
}

/// Abstraction over how a single ready task is dispatched in the EKO runtime.
///
/// The framework runtime DAG controller depends on this trait (not on
/// `execute_task` directly), so EKO dispatch and worktree integration can be
/// tested with a deterministic mock instead of a real LLM-backed Agent. The
/// production implementation ([`RealTaskDispatcher`]) wraps `execute_task`.
///
/// The dispatcher is given EKO-specific per-run semaphores and file locks.
/// EKO additionally holds one process-wide permit across all workspace runs.
trait TaskDispatcher: Send + Sync {
    /// Execute `task` for `run_id`. Success carries both the bounded structured
    /// result and the complete model output. The former feeds parent summaries;
    /// the latter is the evidence reviewed against acceptance criteria.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)] // product resource limits + locks are the application dispatch contract
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        task: PlanTask,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>>;

    /// Integrate a reviewed writer result into the authoritative workspace.
    /// Non-writer dispatchers use the default no-op implementation.
    #[allow(clippy::too_many_arguments)]
    fn integrate(
        &self,
        _store: Arc<TaskRuntimeStore>,
        _blocking: TaskRuntimeBlockingAdapter,
        _run_id: String,
        _task: PlanTask,
        _execution_id: String,
        _cancel: CancellationToken,
        _trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<super::worktree::WorktreeIntegrationOutcome>, String>,
                > + Send,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
}

/// Production dispatcher: delegates to [`execute_task`] against the task's
/// local or frozen cross-workspace Agent target.
///
/// Review remains in the EKO runtime controller after a Subagent returns. The
/// dispatcher only needs the Agent and product-specific concurrency primitives.
struct RealTaskDispatcher {
    primary_agent: crate::agent_handle::AgentHandle,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
}

async fn resolve_task_execution_agent(
    store: &TaskRuntimeStore,
    blocking: &TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    local_agent: crate::agent_handle::AgentHandle,
) -> Result<
    (
        crate::agent_handle::AgentHandle,
        Option<crate::agent_pool::AgentPoolExecutionLease>,
    ),
    String,
> {
    let Some(target) = task.execution_target.as_ref() else {
        return Ok((local_agent, None));
    };
    if target.subagent_role != task.agent_role {
        return Err(format!(
            "task '{}' target role '{}' does not match Subagent role '{}'",
            task.id, target.subagent_role, task.agent_role
        ));
    }
    let load_run_id = run_id.to_string();
    let run = blocking
        .run("load task execution target run", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| error.to_string())?;
    let leader = crate::agent_router::AgentAddress::new(
        crate::workspace::WorkspaceId::from_raw(run.workspace_id),
        run.conversation_id,
    );
    let resolver = store.execution_target_resolver().ok_or_else(|| {
        format!(
            "task '{}' targets Agent group '{}' but no cross-workspace resolver is installed",
            task.id, target.group_id
        )
    })?;
    let lease = resolver.acquire(&leader, target).await?;
    let agent = lease.agent();
    Ok((agent, Some(lease)))
}

impl TaskDispatcher for RealTaskDispatcher {
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        task: PlanTask,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>> {
        let local_agent = self.primary_agent.clone();
        let workspace_io = self.workspace_io.clone();
        Box::pin(async move {
            let run_id = context.run_id;
            let cancel = context.cancel;
            let delegation_policy = context.delegation_policy;
            let task_id = task.id.clone();
            let _process_subagent_permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process Subagent permit")),
                permit = PROCESS_EXECUTION_GOVERNOR.subagent.acquire() => permit.map_err(|error| TaskDispatchFailure::failed(task_id.clone(), error.to_string()))?,
            };
            let (execution_agent, target_lease) =
                resolve_task_execution_agent(&store, &blocking, &run_id, &task, local_agent)
                    .await
                    .map_err(|error| TaskDispatchFailure::failed(task_id, error))?;
            // A cross-workspace target needs its own target-runtime receipt.
            // Never reuse the leader workspace authority for that Agent.
            let workspace_io = target_lease.is_none().then_some(workspace_io).flatten();
            // Scope run_id + cancel + trace_sink into task-local so Subagent-internal
            // tools (task_*/task_execute, and their execute_with_context
            // fallback path) and L3 nested Subagents can read them.
            // NOTE: trace_sink/cancel are also passed as explicit params to
            // execute_task (which uses them directly, not via task_local) — but
            // scoping them here keeps the task_local consistent for any code
            // path that reads CURRENT_TRACE_SINK/CURRENT_CANCEL directly.
            let sink_clone = trace_sink.clone();
            let cancel_clone = cancel.clone();
            let result = super::task_tools::with_run_context(
                run_id.clone(),
                cancel_clone,
                sink_clone,
                async {
                    execute_task(
                        store,
                        blocking,
                        execution_agent,
                        write_sem,
                        shell_sem,
                        llm_sem,
                        file_write_locks,
                        trace_sink,
                        run_id,
                        claim,
                        task,
                        cancel,
                        delegation_policy,
                        workspace_io,
                    )
                    .await
                },
            )
            .await;
            drop(target_lease);
            result
        })
    }

    fn integrate(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
        run_id: String,
        task: PlanTask,
        execution_id: String,
        cancel: CancellationToken,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<super::worktree::WorktreeIntegrationOutcome>, String>,
                > + Send,
        >,
    > {
        let local_agent = self.primary_agent.clone();
        Box::pin(async move {
            if !matches!(
                task.kind,
                PlanTaskKind::Implementation | PlanTaskKind::Debugging
            ) {
                return Ok(None);
            }
            if cancel.is_cancelled() {
                return Err("cancelled before worktree integration".to_string());
            }

            let (execution_agent, target_lease) =
                resolve_task_execution_agent(&store, &blocking, &run_id, &task, local_agent)
                    .await?;
            let load_run_id = run_id.clone();
            let run = blocking
                .run("load worktree integration run", move |store| {
                    store
                        .get_run(&load_run_id)?
                        .ok_or(StoreError::RunNotFound(load_run_id))
                })
                .await
                .map_err(|error| error.to_string())?;
            let workspace_id = run.workspace_id;
            let conversation_id = run.conversation_id;

            let working_dir = execution_agent
                .read(|agent| agent.working_dir())
                .await
                .ok_or_else(|| "writer integration requires a Git working directory".to_string())?;
            let repo_root =
                tokio::task::spawn_blocking(move || super::worktree::git_repo_root(&working_dir))
                    .await
                    .map_err(|error| format!("failed to join repo-root lookup: {error}"))?
                    .map_err(|error| error.to_string())?;
            let merge_lock = super::worktree::repo_merge_lock(&repo_root);
            let _merge_guard = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("cancelled while waiting for worktree integration".to_string()),
                guard = merge_lock.lock_owned() => guard,
            };
            if cancel.is_cancelled() {
                return Err("cancelled before worktree integration started".to_string());
            }

            let label = task_worktree_label(&task.agent_role, &run_id, &task.id);
            let ownership = super::planner::file_ownership(&task);
            let branch = super::worktree::fork_branch_name(&label);
            let start_run_id = run_id.clone();
            let start_task_id = task.id.clone();
            let start_message =
                format!("worktree integration started: execution={execution_id}, branch={branch}");
            if let Err(error) = blocking
                .run("note worktree integration start", move |store| {
                    store.note(&start_run_id, Some(&start_task_id), &start_message)
                })
                .await
            {
                tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration start");
            }
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::task(
                    workspace_id.clone(),
                    conversation_id.clone(),
                    run_id.clone(),
                    task.id.clone(),
                    RuntimeEventKind::MergeStarted,
                    serde_json::json!({
                        "execution_id": execution_id,
                        "branch": branch,
                    }),
                )
                .with_agent(task.agent_role.clone()),
            );

            let task_id = task.id.clone();
            let execution_for_merge = execution_id.clone();
            let repo_for_merge = repo_root.clone();
            let label_for_merge = label.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                super::worktree::integrate_fork_worktree(
                    &repo_for_merge,
                    &label_for_merge,
                    &task_id,
                    &execution_for_merge,
                    &ownership,
                )
            })
            .await
            .map_err(|error| format!("failed to join worktree integration: {error}"))?;

            let result = match outcome {
                Ok(outcome) => {
                    let summary = outcome.summary();
                    let note_run_id = run_id.clone();
                    let note_task_id = task.id.clone();
                    let note_summary = summary.clone();
                    let cleanup_warning = outcome.cleanup_warning.clone();
                    if let Err(error) = blocking
                        .run("note worktree integration result", move |store| {
                            store.note(&note_run_id, Some(&note_task_id), &note_summary)?;
                            if let Some(warning) = cleanup_warning {
                                store.note(
                                    &note_run_id,
                                    Some(&note_task_id),
                                    &format!("worktree cleanup warning: {warning}"),
                                )?;
                            }
                            Ok(())
                        })
                        .await
                    {
                        tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration result");
                    }
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::task(
                            workspace_id.clone(),
                            conversation_id.clone(),
                            run_id,
                            task.id.clone(),
                            RuntimeEventKind::MergeCompleted,
                            serde_json::json!({
                                "execution_id": execution_id,
                                "integration_status": outcome.status.as_str(),
                                "branch": outcome.branch,
                                "path": outcome.path,
                                "changed_files": outcome.changed_files,
                                "merge_commit": outcome.merge_commit,
                                "cleanup_warning": outcome.cleanup_warning,
                            }),
                        )
                        .with_agent(task.agent_role),
                    );
                    Ok(Some(outcome))
                }
                Err(error) => {
                    let message = error.to_string();
                    let note_run_id = run_id.clone();
                    let note_task_id = task.id.clone();
                    let failure_note = format!("worktree integration failed: {message}");
                    if let Err(error) = blocking
                        .run("note worktree integration failure", move |store| {
                            store.note(&note_run_id, Some(&note_task_id), &failure_note)
                        })
                        .await
                    {
                        tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration failure");
                    }
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::task(
                            workspace_id,
                            conversation_id,
                            run_id,
                            task.id.clone(),
                            RuntimeEventKind::MergeFailed,
                            serde_json::json!({
                                "execution_id": execution_id,
                                "branch": branch,
                                "error": message,
                            }),
                        )
                        .with_agent(task.agent_role),
                    );
                    Err(message)
                }
            };
            drop(target_lease);
            result
        })
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchSuccess {
    task_id: String,
    result: SubagentTaskResult,
    full_output: String,
    suggested_tasks: Vec<SuggestedTask>,
}

fn task_execution_summary_candidate(
    run_id: &str,
    task: &PlanTask,
    result: SubagentTaskResult,
    suggested_tasks: Vec<SuggestedTask>,
    decisions: Vec<String>,
) -> TaskExecutionSummary {
    TaskExecutionSummary {
        run_id: run_id.to_string(),
        task_id: task.id.clone(),
        subagent_name: task.agent_role.clone(),
        result,
        decisions,
        next_implications: Vec::new(),
        suggested_tasks,
        created_at: chrono::Utc::now(),
    }
}

#[derive(Debug, Clone, Default)]
struct TaskExecutionUsage {
    durable: SubagentRunUsage,
    input_tokens: u64,
    output_tokens: u64,
}

impl TaskExecutionUsage {
    fn from_framework(result: &echo_agent::agent::subagent::SubagentResult) -> Self {
        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
        let iterations = u64::try_from(result.iterations).unwrap_or(u64::MAX);
        let input_tokens = result
            .usage
            .as_ref()
            .map(|usage| usage.prompt_tokens)
            .unwrap_or(0);
        let output_tokens = result
            .usage
            .as_ref()
            .map(|usage| usage.completion_tokens)
            .unwrap_or(0);
        Self {
            durable: SubagentRunUsage {
                duration_ms: Some(duration_ms),
                tokens_used: result
                    .usage
                    .as_ref()
                    .map(|usage| usage.prompt_tokens.saturating_add(usage.completion_tokens)),
                iterations: Some(iterations),
            },
            input_tokens,
            output_tokens,
        }
    }

    fn duration_ms(&self) -> u64 {
        self.durable.duration_ms.unwrap_or(0)
    }

    fn from_turn_receipt(receipt: &TurnReceipt) -> Self {
        let duration_ms = u64::try_from(receipt.elapsed.as_millis()).unwrap_or(u64::MAX);
        Self {
            durable: SubagentRunUsage {
                duration_ms: Some(duration_ms),
                tokens_used: (receipt.llm_calls > 0).then(|| {
                    receipt
                        .prompt_tokens
                        .saturating_add(receipt.completion_tokens)
                }),
                iterations: None,
            },
            input_tokens: receipt.prompt_tokens,
            output_tokens: receipt.completion_tokens,
        }
    }
}

#[allow(clippy::result_large_err)]
async fn finalize_framework_subagent_result(
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    execution_id: &str,
    result: echo_agent::agent::subagent::SubagentResult,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let usage = TaskExecutionUsage::from_framework(&result);
    let usage_run_id = run_id.to_string();
    let usage_execution_id = execution_id.to_string();
    let persisted_usage = usage.clone();
    blocking
        .run("persist framework Subagent usage", move |store| {
            store.account_subagent_usage(
                &usage_run_id,
                &usage_execution_id,
                "framework_dispatch_total",
                persisted_usage.input_tokens,
                persisted_usage.output_tokens,
                persisted_usage.duration_ms(),
            )
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!(
                "failed to persist Subagent usage for {execution_id}: {error}"
            ))
            .with_usage(usage.clone())
        })?;
    if result.outcome.status != echo_agent::agent::subagent::SubagentStatus::Completed {
        let status = result.outcome.status.into();
        let message = if result.outcome.summary.trim().is_empty() {
            result.output
        } else {
            result.outcome.summary
        };
        return Err(ExecutionFailure {
            status,
            message,
            usage: Some(usage),
            agent_failure: None,
        });
    }
    let task_result = SubagentTaskResult::from_framework(&result);
    Ok((task_result, result.output, usage))
}

#[derive(Debug, Clone)]
struct ExecutionFailure {
    status: SubagentRunStatus,
    message: String,
    usage: Option<TaskExecutionUsage>,
    agent_failure: Option<echo_agent::error::AgentFailure>,
}

impl ExecutionFailure {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Failed,
            message: message.into(),
            usage: None,
            agent_failure: None,
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
            usage: None,
            agent_failure: None,
        }
    }

    fn from_agent_failure(
        failure: &echo_agent::error::AgentFailure,
        message: impl Into<String>,
    ) -> Self {
        let status = match failure.terminal_kind {
            echo_agent::error::AgentTerminalKind::Cancelled => SubagentRunStatus::Cancelled,
            echo_agent::error::AgentTerminalKind::TimedOut => SubagentRunStatus::TimedOut,
            echo_agent::error::AgentTerminalKind::Failed
            | echo_agent::error::AgentTerminalKind::PermissionDenied => SubagentRunStatus::Failed,
        };
        Self {
            status,
            message: message.into(),
            usage: None,
            agent_failure: Some(failure.clone()),
        }
    }

    fn from_react(error: echo_agent::error::ReactError, context: &str) -> Self {
        let status = echo_agent::agent::subagent::subagent_status_from_error(&error).into();
        Self {
            status,
            message: format!("{context}: {error}"),
            usage: None,
            agent_failure: Some(echo_agent::error::AgentFailure::from_react_error(&error)),
        }
    }

    fn with_usage(mut self, usage: TaskExecutionUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn attach_agent_failure_evidence(
    result: &mut SubagentTaskResult,
    failure: &echo_agent::error::AgentFailure,
) {
    result.evidence.push(SubagentEvidenceResult {
        kind: "agent_failure".to_string(),
        subject: failure.code.clone(),
        outcome: Some(
            match failure.terminal_kind {
                echo_agent::error::AgentTerminalKind::Failed => "failed",
                echo_agent::error::AgentTerminalKind::Cancelled => "cancelled",
                echo_agent::error::AgentTerminalKind::TimedOut => "timed_out",
                echo_agent::error::AgentTerminalKind::PermissionDenied => "permission_denied",
            }
            .to_string(),
        ),
        details: failure.message.chars().take(1_200).collect(),
        source: SubagentVerificationSource::Observed,
        attributes: serde_json::to_value(failure).unwrap_or(serde_json::Value::Null),
    });
}

#[derive(Clone)]
struct EkoAgentTurnContext {
    workspace_id: String,
    conversation_id: String,
    run_id: String,
    task_id: Option<String>,
    execution_id: Option<String>,
    agent_role: Option<String>,
}

impl EkoAgentTurnContext {
    fn run(run: &TaskRun) -> Self {
        Self {
            workspace_id: run.workspace_id.clone(),
            conversation_id: run.conversation_id.clone(),
            run_id: run.run_id.clone(),
            task_id: None,
            execution_id: None,
            agent_role: None,
        }
    }

    fn primary_task(run: &TaskRun, task: &PlanTask, execution_id: &str) -> Self {
        Self {
            workspace_id: run.workspace_id.clone(),
            conversation_id: run.conversation_id.clone(),
            run_id: run.run_id.clone(),
            task_id: Some(task.id.clone()),
            execution_id: Some(execution_id.to_string()),
            agent_role: Some(task.agent_role.clone()),
        }
    }

    fn event(&self, kind: RuntimeEventKind, payload: serde_json::Value) -> ExecEvent {
        let event = match (&self.task_id, &self.execution_id) {
            (Some(task_id), Some(execution_id)) => ExecEvent::subagent(
                self.workspace_id.clone(),
                self.conversation_id.clone(),
                self.run_id.clone(),
                task_id.clone(),
                execution_id.clone(),
                kind,
                payload,
            ),
            _ => ExecEvent::run(
                self.workspace_id.clone(),
                self.conversation_id.clone(),
                self.run_id.clone(),
                kind,
                payload,
            ),
        };
        if let Some(agent_role) = self.agent_role.as_ref() {
            event.with_agent(agent_role.clone())
        } else {
            event
        }
    }
}

struct PrimaryTaskTurnPersistence {
    blocking: TaskRuntimeBlockingAdapter,
    replay_safe_tools: HashSet<String>,
}

struct RunTurnPersistence {
    blocking: TaskRuntimeBlockingAdapter,
    turn_id: String,
}

#[derive(Default)]
struct EkoAgentTurnState {
    output: String,
    in_thinking: bool,
    pending_verification: HashMap<String, String>,
    pending_file_access: HashMap<String, (bool, String)>,
    observed_evidence: Vec<echo_agent::agent::subagent::SubagentEvidence>,
    observed_artifacts: Vec<echo_agent::agent::subagent::SubagentArtifact>,
    mutating_tool_observed: bool,
}

struct EkoAgentTurnObservation {
    output: String,
    observed_evidence: Vec<echo_agent::agent::subagent::SubagentEvidence>,
    observed_artifacts: Vec<echo_agent::agent::subagent::SubagentArtifact>,
    mutating_tool_observed: bool,
}

/// The sole EKO adapter below [`AgentTurnDriver`] for TaskRuntime-owned turns.
///
/// Framework code owns stream startup, envelope sequencing, exact terminal
/// detection, typed failures, cancellation, and provider-reported receipt
/// accounting. This sink owns only EKO product projection and persistence:
/// `ExecEvent`, exact event-id usage, tool boundaries, evidence, and artifacts.
struct EkoAgentTurnSink {
    context: EkoAgentTurnContext,
    trace_sink: Option<ExecSink>,
    primary_task: Option<PrimaryTaskTurnPersistence>,
    run_turn: Option<RunTurnPersistence>,
    mutating_tools: HashSet<String>,
    state: std::sync::Mutex<EkoAgentTurnState>,
}

impl EkoAgentTurnSink {
    fn for_run(
        run: &TaskRun,
        turn_id: &str,
        blocking: TaskRuntimeBlockingAdapter,
        mutating_tools: HashSet<String>,
        trace_sink: Option<ExecSink>,
    ) -> Self {
        Self {
            context: EkoAgentTurnContext::run(run),
            trace_sink,
            primary_task: None,
            run_turn: Some(RunTurnPersistence {
                blocking,
                turn_id: turn_id.to_string(),
            }),
            mutating_tools,
            state: std::sync::Mutex::new(EkoAgentTurnState::default()),
        }
    }

    fn for_primary_task(
        run: &TaskRun,
        task: &PlanTask,
        execution_id: &str,
        blocking: TaskRuntimeBlockingAdapter,
        replay_safe_tools: HashSet<String>,
        trace_sink: Option<ExecSink>,
    ) -> Self {
        Self {
            context: EkoAgentTurnContext::primary_task(run, task, execution_id),
            trace_sink,
            primary_task: Some(PrimaryTaskTurnPersistence {
                blocking,
                replay_safe_tools,
            }),
            run_turn: None,
            mutating_tools: HashSet::new(),
            state: std::sync::Mutex::new(EkoAgentTurnState::default()),
        }
    }

    fn emit(&self, kind: RuntimeEventKind, payload: serde_json::Value) {
        emit_exec(self.trace_sink.as_ref(), self.context.event(kind, payload));
    }

    fn finish(&self, final_answer: Option<&str>) -> EkoAgentTurnObservation {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(final_answer) = final_answer.filter(|answer| !answer.is_empty()) {
            state.output = final_answer.to_string();
        }
        EkoAgentTurnObservation {
            output: std::mem::take(&mut state.output),
            observed_evidence: std::mem::take(&mut state.observed_evidence),
            observed_artifacts: std::mem::take(&mut state.observed_artifacts),
            mutating_tool_observed: state.mutating_tool_observed,
        }
    }

    fn persistence_error(
        operation: &str,
        error: impl std::fmt::Display,
    ) -> echo_agent::error::ReactError {
        echo_agent::error::ReactError::Other(format!("{operation}: {error}"))
    }
}

#[async_trait::async_trait]
impl EventSink for EkoAgentTurnSink {
    async fn on_event(
        &self,
        envelope: echo_agent::agent::EventEnvelope,
    ) -> echo_agent::error::Result<SinkControl> {
        let source_event_id = envelope.event_id.to_string();
        match envelope.payload {
            AgentEvent::Token(content) => {
                let in_thinking = {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if !state.in_thinking {
                        state.output.push_str(&content);
                    }
                    state.in_thinking
                };
                self.emit(
                    if in_thinking {
                        RuntimeEventKind::ThinkingDelta
                    } else {
                        RuntimeEventKind::TokenDelta
                    },
                    serde_json::json!({ "content": content }),
                );
            }
            AgentEvent::ThinkStart => {
                self.state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .in_thinking = true;
                self.emit(RuntimeEventKind::ThinkingStarted, serde_json::json!({}));
            }
            AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            } => {
                self.state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .in_thinking = false;
                self.emit(
                    RuntimeEventKind::ThinkingEnded,
                    serde_json::json!({
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                    }),
                );
            }
            AgentEvent::LlmUsage {
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_prompt_tokens,
                cache_creation_prompt_tokens,
                usage_reported,
            } => {
                if usage_reported && let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let input_tokens = u64::try_from(prompt_tokens).unwrap_or(u64::MAX);
                    let output_tokens = u64::try_from(completion_tokens).unwrap_or(u64::MAX);
                    let usage_event_id = source_event_id.clone();
                    primary_task
                        .blocking
                        .run("persist primary Subagent usage", move |store| {
                            store.account_subagent_usage(
                                &run_id,
                                &execution_id,
                                &usage_event_id,
                                input_tokens,
                                output_tokens,
                                0,
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "failed to persist primary Subagent usage",
                                error,
                            )
                        })?;
                }
                if usage_reported && let Some(run_turn) = self.run_turn.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let turn_id = run_turn.turn_id.clone();
                    let input_tokens = u64::try_from(prompt_tokens).unwrap_or(u64::MAX);
                    let output_tokens = u64::try_from(completion_tokens).unwrap_or(u64::MAX);
                    let usage_event_id = source_event_id.clone();
                    run_turn
                        .blocking
                        .run("persist primary RunTurn usage", move |store| {
                            store
                                .account_run_turn_usage(
                                    &run_id,
                                    &turn_id,
                                    &usage_event_id,
                                    input_tokens,
                                    output_tokens,
                                )
                                .map(|_| ())
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "failed to persist primary RunTurn usage",
                                error,
                            )
                        })?;
                }
                self.emit(
                    RuntimeEventKind::Usage,
                    serde_json::json!({
                        "model": model,
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": total_tokens,
                        "cached_prompt_tokens": cached_prompt_tokens,
                        "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                        "usage_reported": usage_reported,
                        "usage_event_id": source_event_id,
                    }),
                );
            }
            AgentEvent::ToolCall {
                call_id,
                invocation,
            } => {
                let name = invocation.name.clone();
                let args = invocation.args.clone();
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.mutating_tool_observed |= self.mutating_tools.contains(&name);
                    if let Some(check) = verification_check_from_agent_tool(&name, &args) {
                        state.pending_verification.insert(call_id.clone(), check);
                    }
                    if let Some(access) = file_access_from_agent_tool(&name, &args) {
                        state.pending_file_access.insert(call_id.clone(), access);
                    }
                }
                if let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let task_id = self.context.task_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its task identity".to_string(),
                        )
                    })?;
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let tool_call_id = call_id.clone();
                    let tool_name = name.clone();
                    let replay_safe = primary_task.replay_safe_tools.contains(&name);
                    primary_task
                        .blocking
                        .run("persist tool start boundary", move |store| {
                            store.record_tool_started(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &tool_call_id,
                                &tool_name,
                                replay_safe,
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error("failed to persist tool start boundary", error)
                        })?;
                }
                self.emit(
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "call_id": call_id,
                        "invocation": invocation,
                    }),
                );
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                result,
            } => {
                let result_text = if result.success {
                    result.output.clone()
                } else {
                    result
                        .error
                        .clone()
                        .unwrap_or_else(|| result.output.clone())
                };
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(check) = state.pending_verification.remove(&call_id) {
                        state.observed_evidence.push(
                            echo_agent::agent::subagent::SubagentEvidence {
                                kind: "verification".to_string(),
                                subject: check,
                                outcome: Some(if result.success {
                                    "passed".to_string()
                                } else {
                                    "failed".to_string()
                                }),
                                details: result_text.chars().take(500).collect(),
                                source:
                                    echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                                attributes: serde_json::Value::Null,
                            },
                        );
                    }
                    if result.success
                        && let Some((write, path)) = state.pending_file_access.remove(&call_id)
                    {
                        state.observed_evidence.push(
                            echo_agent::agent::subagent::SubagentEvidence {
                                kind: if write { "file_write" } else { "file_read" }.to_string(),
                                subject: path,
                                outcome: Some("succeeded".to_string()),
                                details: String::new(),
                                source:
                                    echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                                attributes: serde_json::Value::Null,
                            },
                        );
                    } else {
                        state.pending_file_access.remove(&call_id);
                    }
                    if let Some(artifact) = result.artifact.as_ref() {
                        state.observed_artifacts.push(
                            echo_agent::agent::subagent::SubagentArtifact {
                                path: artifact.path.to_string_lossy().to_string(),
                                kind: "tool_log".to_string(),
                                bytes: Some(artifact.artifact_bytes),
                                sha256: Some(artifact.sha256.clone()),
                                producer_execution_id: self.context.execution_id.clone(),
                                available: true,
                            },
                        );
                    }
                }
                if let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let task_id = self.context.task_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its task identity".to_string(),
                        )
                    })?;
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let tool_call_id = call_id.clone();
                    let tool_name = name.clone();
                    let tool_result_text = result_text.clone();
                    let tool_success = result.success;
                    let tool_failure = result.failure.clone();
                    primary_task
                        .blocking
                        .run("persist tool terminal boundary", move |store| {
                            store.record_tool_finished(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &tool_call_id,
                                &tool_name,
                                tool_success,
                                &tool_result_text,
                                tool_failure.as_ref(),
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "tool settled but its terminal boundary was not persisted",
                                error,
                            )
                        })?;
                }
                self.emit(
                    RuntimeEventKind::ToolCompleted,
                    serde_json::json!({
                        "call_id": call_id,
                        "name": name,
                        "result": result,
                    }),
                );
            }
            AgentEvent::ToolStream {
                call_id,
                name,
                event,
            } => {
                let payload = match event {
                    echo_agent::tools::ToolStreamEvent::Progress { message, percent } => {
                        serde_json::json!({
                            "call_id": call_id,
                            "name": name,
                            "message": message,
                            "percent": percent,
                        })
                    }
                    echo_agent::tools::ToolStreamEvent::Output { channel, chunk } => {
                        serde_json::json!({
                            "call_id": call_id,
                            "name": name,
                            "channel": match channel {
                                echo_agent::tools::ToolOutputChannel::Stdout => "stdout",
                                echo_agent::tools::ToolOutputChannel::Stderr => "stderr",
                                echo_agent::tools::ToolOutputChannel::Log => "log",
                            },
                            "chunk": chunk,
                        })
                    }
                    echo_agent::tools::ToolStreamEvent::Complete(_) => {
                        return Ok(SinkControl::Continue);
                    }
                };
                self.emit(RuntimeEventKind::ToolOutput, payload);
            }
            AgentEvent::FinalAnswer(answer) => {
                if !answer.is_empty() {
                    self.state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .output = answer;
                }
            }
            AgentEvent::Cancelled | AgentEvent::Error { .. } => {}
            _ => {}
        }
        Ok(SinkControl::Continue)
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchFailure {
    task_id: String,
    status: SubagentRunStatus,
    message: String,
    agent_failure: Option<echo_agent::error::AgentFailure>,
}

impl TaskDispatchFailure {
    fn failed(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Failed,
            message: message.into(),
            agent_failure: None,
        }
    }

    fn cancelled(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
            agent_failure: None,
        }
    }

    fn from_execution(task_id: impl Into<String>, failure: ExecutionFailure) -> Self {
        Self {
            task_id: task_id.into(),
            status: failure.status,
            message: failure.message,
            agent_failure: failure.agent_failure,
        }
    }

    fn into_react(self) -> echo_agent::error::ReactError {
        use echo_agent::error::AgentError;
        match self.status {
            SubagentRunStatus::Cancelled => {
                echo_agent::error::ReactError::Agent(Box::new(AgentError::Cancelled(self.message)))
            }
            SubagentRunStatus::TimedOut => {
                echo_agent::error::ReactError::Agent(Box::new(AgentError::Timeout(self.message)))
            }
            SubagentRunStatus::Running
            | SubagentRunStatus::Completed
            | SubagentRunStatus::Failed => echo_agent::error::ReactError::Other(self.message),
        }
    }
}

type TaskDispatchResult = Result<TaskDispatchSuccess, TaskDispatchFailure>;

/// Pick the largest deterministic subset of the ready frontier that has no
/// writer ownership conflicts. Read-only tasks never consume ownership.
fn select_ownership_safe_wave(ready: Vec<PlanTask>) -> Vec<PlanTask> {
    let mut selected = Vec::new();
    let mut selected_writers: Vec<super::planner::FileOwnership> = Vec::new();
    for task in ready {
        let ownership = super::planner::file_ownership(&task);
        if matches!(ownership, super::planner::FileOwnership::ReadOnly) {
            selected.push(task);
            continue;
        }
        if selected_writers
            .iter()
            .all(|selected| !ownership.conflicts_with(selected))
        {
            selected_writers.push(ownership);
            selected.push(task);
        }
    }
    selected
}

struct EkoRuntimeDagController<W: TaskDispatcher> {
    store: Arc<TaskRuntimeStore>,
    blocking: TaskRuntimeBlockingAdapter,
    dispatcher: Arc<W>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    resolution_metadata: std::sync::Mutex<HashMap<String, RuntimeTaskProductSettlement>>,
    dispatch_failures: std::sync::Mutex<HashMap<String, TaskDispatchFailure>>,
}

#[derive(Clone)]
pub struct TaskRuntimeBlockingAdapter {
    store: Arc<TaskRuntimeStore>,
    supervisor: Arc<TaskRuntimeOperationSupervisor>,
}

const PROCESS_TASK_RUNTIME_FILE_IO_LIMIT: usize = 8;
static PROCESS_TASK_RUNTIME_FILE_IO: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(PROCESS_TASK_RUNTIME_FILE_IO_LIMIT)));
tokio::task_local! {
    static CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR: usize;
}

#[derive(Default)]
struct TaskRuntimeOperationState {
    accepting: bool,
    active: usize,
    orphan_failures: Vec<String>,
}

/// Store-owned authority for every accepted async or blocking TaskRuntime
/// operation. Callers only await receipts; dropping a caller never owns or
/// aborts the operation itself.
pub(crate) struct TaskRuntimeOperationSupervisor {
    state: std::sync::Mutex<TaskRuntimeOperationState>,
    idle: tokio::sync::Notify,
}

struct TaskRuntimeOperationReceipt {
    supervisor: Arc<TaskRuntimeOperationSupervisor>,
}

pub(crate) struct TaskRuntimeSettlementReservation {
    receipt: TaskRuntimeOperationReceipt,
}

impl Drop for TaskRuntimeOperationReceipt {
    fn drop(&mut self) {
        if let Ok(mut state) = self.supervisor.state.lock() {
            state.active = state.active.saturating_sub(1);
            if state.active == 0 {
                self.supervisor.idle.notify_waiters();
            }
        }
    }
}

impl TaskRuntimeOperationSupervisor {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(TaskRuntimeOperationState {
                accepting: true,
                ..TaskRuntimeOperationState::default()
            }),
            idle: tokio::sync::Notify::new(),
        })
    }

    fn is_nested_operation(self: &Arc<Self>) -> bool {
        let identity = Arc::as_ptr(self) as usize;
        CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR
            .try_with(|id| *id == identity)
            .unwrap_or(false)
    }

    fn register(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<TaskRuntimeOperationReceipt, StoreError> {
        let nested = self.is_nested_operation();
        let mut state = self.state.lock().map_err(|_| StoreError::LockPoisoned)?;
        if !state.accepting && !nested {
            return Err(StoreError::InvalidPlan(format!(
                "TaskRuntime operation admission is closed during {operation}"
            )));
        }
        state.active = state.active.checked_add(1).ok_or_else(|| {
            StoreError::InvalidPlan("TaskRuntime operation capacity exhausted".to_string())
        })?;
        drop(state);
        Ok(TaskRuntimeOperationReceipt {
            supervisor: Arc::clone(self),
        })
    }

    pub(crate) fn begin_shutdown(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "TaskRuntime operation supervisor lock is poisoned".to_string())?;
        state.accepting = false;
        Ok(())
    }

    pub(crate) fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active)
            .unwrap_or(usize::MAX)
    }

    pub(crate) async fn join(&self) -> Result<(), String> {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let failures = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "TaskRuntime operation supervisor lock is poisoned".to_string())?;
                if state.active == 0 {
                    Some(std::mem::take(&mut state.orphan_failures))
                } else {
                    None
                }
            };
            if let Some(failures) = failures {
                return if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                };
            }
            notified.await;
        }
    }

    fn record_orphan_failure(&self, operation: &'static str, error: &StoreError) {
        if let Ok(mut state) = self.state.lock() {
            state.orphan_failures.push(format!("{operation}: {error}"));
        }
    }
}

impl TaskRuntimeBlockingAdapter {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        let supervisor = store.operation_supervisor();
        Self { store, supervisor }
    }

    pub async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> echo_agent::error::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<TaskRuntimeStore>) -> Result<T, StoreError> + Send + 'static,
    {
        self.run_store(operation, function).await.map_err(|error| {
            echo_agent::error::ReactError::Other(format!(
                "TaskRuntime blocking operation {operation} failed: {error}"
            ))
        })
    }

    pub async fn run_store<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<TaskRuntimeStore>) -> Result<T, StoreError> + Send + 'static,
    {
        let store = self.store.clone();
        self.run_owned(operation, move || function(store)).await
    }

    pub async fn run_owned<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    {
        let permit = PROCESS_TASK_RUNTIME_FILE_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| {
                StoreError::InvalidPlan(format!(
                    "TaskRuntime blocking adapter closed during {operation}: {error}"
                ))
            })?;
        let receipt = self.supervisor.register(operation)?;
        let supervisor = Arc::clone(&self.supervisor);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let execution = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            function()
        });
        tokio::spawn(async move {
            let _receipt = receipt;
            let result = match execution.await {
                Ok(result) => result,
                Err(error) => Err(StoreError::InvalidPlan(format!(
                    "TaskRuntime blocking operation {operation} failed to join: {error}"
                ))),
            };
            if let Err(orphaned) = sender.send(result)
                && let Err(error) = orphaned
            {
                supervisor.record_orphan_failure(operation, &error);
            }
        });
        receiver.await.map_err(|_| {
            StoreError::InvalidPlan(format!(
                "TaskRuntime blocking operation {operation} ended without a receipt"
            ))
        })?
    }

    /// Run a multi-stage async command under store ownership. Nested blocking
    /// settlements retain admission after phase-one shutdown so an accepted
    /// command can always publish its terminal fact.
    pub async fn run_async_owned<T, F>(
        &self,
        operation: &'static str,
        future: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let receipt = self.supervisor.register(operation)?;
        let receiver = self.spawn_async_with_receipt(operation, receipt, future);
        receiver.await.map_err(|_| {
            StoreError::InvalidPlan(format!(
                "TaskRuntime async operation {operation} ended without a receipt"
            ))
        })?
    }

    pub(crate) fn reserve_settlement(
        &self,
        operation: &'static str,
    ) -> Result<TaskRuntimeSettlementReservation, StoreError> {
        self.supervisor
            .register(operation)
            .map(|receipt| TaskRuntimeSettlementReservation { receipt })
    }

    pub(crate) fn record_lifecycle_debt(&self, operation: &'static str, error: &StoreError) {
        self.supervisor.record_orphan_failure(operation, error);
    }

    pub(crate) fn spawn_reserved_settlement<T, F>(
        &self,
        operation: &'static str,
        reservation: TaskRuntimeSettlementReservation,
        future: F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, StoreError>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        self.spawn_async_with_receipt(operation, reservation.receipt, future)
    }

    fn spawn_async_with_receipt<T, F>(
        &self,
        operation: &'static str,
        receipt: TaskRuntimeOperationReceipt,
        future: F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, StoreError>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let supervisor = Arc::clone(&self.supervisor);
        let supervisor_id = Arc::as_ptr(&self.supervisor) as usize;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let execution =
            tokio::spawn(CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR.scope(supervisor_id, future));
        tokio::spawn(async move {
            let _receipt = receipt;
            let result = match execution.await {
                Ok(result) => result,
                Err(error) => Err(StoreError::InvalidPlan(format!(
                    "TaskRuntime async operation {operation} failed to join: {error}"
                ))),
            };
            if let Err(orphaned) = sender.send(result)
                && let Err(error) = orphaned
            {
                supervisor.record_orphan_failure(operation, &error);
            }
        });
        receiver
    }
}

impl<W: TaskDispatcher> EkoRuntimeDagController<W> {
    fn plan_task(runtime_task: &echo_agent::tasks::Task) -> echo_agent::error::Result<PlanTask> {
        PlanTask::try_from(runtime_task.clone()).map_err(echo_agent::error::ReactError::Other)
    }

    async fn review_stop_disposition(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeStopDisposition> {
        let run_id = run_id.to_string();
        let run = self
            .blocking
            .run("load run review disposition", move |store| {
                store
                    .get_run(&run_id)?
                    .ok_or(StoreError::RunNotFound(run_id))
            })
            .await?;
        Ok(if run.attended_mode == AttendedMode::Unattended {
            echo_agent::tasks::RuntimeStopDisposition::Fail
        } else {
            echo_agent::tasks::RuntimeStopDisposition::Pause
        })
    }

    async fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: impl Into<String>,
    ) -> echo_agent::error::Result<()> {
        let run_id = run_id.to_string();
        let task_id = task_id.map(str::to_string);
        let message = message.into();
        self.blocking
            .run("append runtime task note", move |store| {
                store.note(&run_id, task_id.as_deref(), &message)
            })
            .await
    }

    fn stage_resolution_metadata(
        &self,
        claim_id: &str,
        metadata: RuntimeTaskProductSettlement,
    ) -> echo_agent::error::Result<()> {
        let mut metadata_by_claim = self
            .resolution_metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match metadata_by_claim.entry(claim_id.to_string()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(metadata);
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                Err(echo_agent::error::ReactError::Other(format!(
                    "resolution metadata for claim '{claim_id}' was staged more than once"
                )))
            }
        }
    }
}

#[async_trait::async_trait]
impl<W: TaskDispatcher + 'static> echo_agent::tasks::RuntimeDagController
    for EkoRuntimeDagController<W>
{
    type DispatchOutput = TaskDispatchSuccess;

    async fn load_snapshot(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimePlanSnapshot> {
        let run_id = run_id.to_string();
        self.blocking
            .run("load exact revisioned task graph", move |store| {
                store.load_runtime_plan_snapshot(&run_id)
            })
            .await
    }

    async fn claim_task(
        &self,
        run_id: &str,
        task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskClaimOutcome> {
        let run_id = run_id.to_string();
        let task = task.clone();
        self.blocking
            .run("claim runtime task", move |store| {
                store.claim_runtime_task(&run_id, &task, expected_revision)
            })
            .await
    }

    async fn claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> echo_agent::error::Result<bool> {
        let run_id = run_id.to_string();
        let task_id = task_id.to_string();
        let claim = claim.clone();
        self.blocking
            .run("check runtime task claim", move |store| {
                store.runtime_task_claim_is_current(&run_id, &task_id, &claim)
            })
            .await
    }

    fn select_ready_wave(
        &self,
        tasks: &[echo_agent::tasks::Task],
        ready_task_ids: Vec<String>,
    ) -> Vec<String> {
        let ready = ready_task_ids
            .iter()
            .filter_map(|task_id| {
                tasks
                    .iter()
                    .find(|task| task.spec.id == *task_id)
                    .cloned()
                    .and_then(|task| match PlanTask::try_from(task) {
                        Ok(task) => Some(task),
                        Err(error) => {
                            tracing::error!(task_id, %error, "invalid EKO task extension in ready frontier");
                            None
                        }
                    })
            })
            .collect::<Vec<_>>();
        select_ownership_safe_wave(ready)
            .into_iter()
            .map(|task| task.id)
            .collect()
    }

    async fn dispatch_task(
        &self,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        runtime_task: echo_agent::tasks::Task,
    ) -> echo_agent::error::Result<Self::DispatchOutput> {
        let task = Self::plan_task(&runtime_task)?;
        let active_task_id = task.id.clone();
        let execution_id = subagent_execution_id(&context.run_id, &task.id, &claim);
        let recovery_run_id = context.run_id.clone();
        let recovery_task_id = task.id.clone();
        let recovery_execution_id = execution_id.clone();
        let recovery_revision = claim.revision;
        let recovery_attempt = claim.attempt;
        let recovery = self
            .blocking
            .run("load recoverable Subagent result", move |store| {
                store.recoverable_subagent_result_for_attempt(
                    &recovery_run_id,
                    &recovery_task_id,
                    &recovery_execution_id,
                    recovery_revision,
                    recovery_attempt,
                )
            })
            .await;
        match recovery {
            Ok(Some(recovered)) => {
                tracing::info!(
                    run_id = %context.run_id,
                    task_id = %task.id,
                    execution_id,
                    "task_runtime: reusing durable Subagent result after restart"
                );
                let note_run_id = context.run_id.clone();
                let note_task_id = task.id.clone();
                if let Err(error) = self
                    .blocking
                    .run("note recovered Subagent result", move |store| {
                        store.note(
                            &note_run_id,
                            Some(&note_task_id),
                            "reused completed Subagent result; continuing at review boundary",
                        )
                    })
                    .await
                {
                    tracing::warn!(run_id = %context.run_id, task_id = %task.id, %error, "failed to note recovered Subagent result");
                }
                return Ok(TaskDispatchSuccess {
                    task_id: task.id,
                    result: recovered.result,
                    full_output: recovered.full_output,
                    suggested_tasks: Vec::new(),
                });
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                run_id = %context.run_id,
                task_id = %task.id,
                %error,
                "failed to inspect durable Subagent result; dispatching normally"
            ),
        }

        let claim_id = claim.claim_id.clone();
        self.dispatcher
            .dispatch(
                self.store.clone(),
                self.blocking.clone(),
                context,
                claim,
                task,
                self.write_sem.clone(),
                self.shell_sem.clone(),
                self.llm_sem.clone(),
                self.file_write_locks.clone(),
                self.trace_sink.clone(),
            )
            .await
            .map_err(|failure| {
                if failure.task_id != active_task_id {
                    return echo_agent::error::ReactError::Other(format!(
                        "dispatcher returned failure for task '{}' while '{}' was active",
                        failure.task_id, active_task_id
                    ));
                }
                if failure.agent_failure.is_some() {
                    self.dispatch_failures
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(claim_id, failure.clone());
                }
                failure.into_react()
            })
    }

    async fn resolve_dispatch(
        &self,
        run_id: &str,
        claim: echo_agent::tasks::TaskClaim,
        runtime_task: echo_agent::tasks::Task,
        dispatch: echo_agent::error::Result<Self::DispatchOutput>,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskResolutionRequest> {
        let task = Self::plan_task(&runtime_task)?;
        let dispatched = match dispatch {
            Ok(dispatched) => dispatched,
            Err(error) => {
                let typed_failure = self
                    .dispatch_failures
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&claim.claim_id)
                    .and_then(|failure| failure.agent_failure);
                let message = typed_failure
                    .as_ref()
                    .map(|failure| failure.message.clone())
                    .unwrap_or_else(|| error.to_string());
                let status = typed_failure
                    .as_ref()
                    .map(|failure| match failure.terminal_kind {
                        echo_agent::error::AgentTerminalKind::Cancelled => {
                            echo_agent::agent::subagent::SubagentStatus::Cancelled
                        }
                        echo_agent::error::AgentTerminalKind::TimedOut => {
                            echo_agent::agent::subagent::SubagentStatus::TimedOut
                        }
                        echo_agent::error::AgentTerminalKind::Failed
                        | echo_agent::error::AgentTerminalKind::PermissionDenied => {
                            echo_agent::agent::subagent::SubagentStatus::Failed
                        }
                    })
                    .unwrap_or_else(|| {
                        echo_agent::agent::subagent::subagent_status_from_error(&error)
                    });
                let request = if let Some(failure) = typed_failure.as_ref().filter(|failure| {
                    failure.retryable
                        && failure.category == echo_agent::error::AgentFailureCategory::Llm
                }) {
                    echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                        failure_fingerprint: Some(
                            super::turn_lifecycle::agent_failure_fingerprint(failure),
                        ),
                        error: failure.message.clone(),
                        exhaustion: if failure.terminal_kind
                            == echo_agent::error::AgentTerminalKind::TimedOut
                        {
                            echo_agent::tasks::RuntimeRetryExhaustion::TimedOut
                        } else {
                            echo_agent::tasks::RuntimeRetryExhaustion::Failed
                        },
                    }
                } else {
                    match status {
                        echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::Cancelled
                        }
                        echo_agent::agent::subagent::SubagentStatus::TimedOut => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::TimedOut {
                                error: format!("Subagent timed out: {message}"),
                            }
                        }
                        echo_agent::agent::subagent::SubagentStatus::Completed
                        | echo_agent::agent::subagent::SubagentStatus::Failed => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                                error: message.clone(),
                            }
                        }
                    }
                };
                let mut result = SubagentTaskResult::terminal(
                    status.into(),
                    message.clone(),
                    vec![message.clone()],
                );
                if let Some(failure) = typed_failure.as_ref() {
                    attach_agent_failure_evidence(&mut result, failure);
                }
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(message.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            Vec::new(),
                            vec![message],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: typed_failure,
                    },
                )?;
                return Ok(request);
            }
        };

        let TaskDispatchSuccess {
            task_id,
            mut result,
            full_output,
            suggested_tasks,
        } = dispatched;
        if task_id != task.id {
            return Err(echo_agent::error::ReactError::Other(format!(
                "dispatcher returned task '{task_id}' for active task '{}'",
                task.id
            )));
        }

        match assess_task_execution(&task, &result) {
            CompletionAssessment::ExecutionFailed { reason } => {
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(reason.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![format!("execution failed: {reason}")],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: None,
                    },
                )?;
                Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                    failure_fingerprint: None,
                    error: format!("execution failed: {reason}"),
                    exhaustion: echo_agent::tasks::RuntimeRetryExhaustion::Failed,
                })
            }
            CompletionAssessment::AcceptancePending {
                missing_checks,
                missing_artifacts,
            } => {
                let reason = format!(
                    "acceptance pending: missing execution checks [{}], missing artifacts [{}]",
                    missing_checks.join(", "),
                    missing_artifacts.join(", "),
                );
                let disposition = self.review_stop_disposition(run_id).await?;
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(reason.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![reason.clone()],
                        )),
                        review: None,
                        diagnostic_note: Some(reason.clone()),
                        typed_terminal: None,
                    },
                )?;
                Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                    error: reason,
                    disposition,
                })
            }
            CompletionAssessment::Executed => {
                if !echo_agent::tasks::RuntimeDagController::claim_is_current(
                    self, run_id, &task.id, &claim,
                )
                .await?
                {
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                        error: "dispatch completed after its claim was superseded".to_string(),
                    });
                }
                let summary = result.summary.clone();
                let review_output = if full_output.trim().is_empty() {
                    summary.as_str()
                } else {
                    full_output.as_str()
                };
                let review = run_review_gate(
                    self.blocking.clone(),
                    self.reviewer_llm.clone(),
                    run_id,
                    &task,
                    review_output,
                )
                .await;
                let (block_reason, review_candidate) = match review {
                    ReviewGateOutcome::Pass(review) => (None, review),
                    ReviewGateOutcome::NeedsFix(_fix_task, review) => (
                        Some("review needs fix; awaiting explicit retry".to_string()),
                        Some(review),
                    ),
                    ReviewGateOutcome::Suspend { reason, review } => {
                        (Some(format!("review suspended: {reason}")), review)
                    }
                    ReviewGateOutcome::Skipped => (
                        Some("reviewer unavailable; blocked pending LLM".to_string()),
                        None,
                    ),
                };
                if let Some(reason) = block_reason {
                    let disposition = self.review_stop_disposition(run_id).await?;
                    self.stage_resolution_metadata(
                        &claim.claim_id,
                        RuntimeTaskProductSettlement {
                            summary: Some(reason.clone()),
                            execution_summary: Some(task_execution_summary_candidate(
                                run_id,
                                &task,
                                result,
                                suggested_tasks,
                                vec![reason.clone()],
                            )),
                            review: review_candidate,
                            diagnostic_note: Some(reason.clone()),
                            typed_terminal: None,
                        },
                    )?;
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                        error: reason,
                        disposition,
                    });
                }

                if !echo_agent::tasks::RuntimeDagController::claim_is_current(
                    self, run_id, &task.id, &claim,
                )
                .await?
                {
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                        error: "review completed after its claim was superseded".to_string(),
                    });
                }
                let execution_id = subagent_execution_id(run_id, &task.id, &claim);
                match integrate_reviewed_task(
                    self.dispatcher.clone(),
                    self.store.clone(),
                    self.blocking.clone(),
                    run_id,
                    &task,
                    &execution_id,
                    &summary,
                    self.cancel.clone(),
                    self.trace_sink.clone(),
                )
                .await
                {
                    Ok((completion_summary, changed_files)) => {
                        if !changed_files.is_empty() {
                            result.touched_files.written = changed_files;
                        }
                        let execution_summary = task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![completion_summary.clone()],
                        );
                        self.stage_resolution_metadata(
                            &claim.claim_id,
                            RuntimeTaskProductSettlement {
                                summary: Some(completion_summary),
                                execution_summary: Some(execution_summary),
                                review: review_candidate,
                                diagnostic_note: None,
                                typed_terminal: None,
                            },
                        )?;
                        Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Completed)
                    }
                    Err(error) => {
                        let error = format!("worktree integration failed: {error}");
                        result.status = SubagentRunStatus::Failed;
                        if !result.remaining_work.contains(&error) {
                            result.remaining_work.push(error.clone());
                        }
                        self.stage_resolution_metadata(
                            &claim.claim_id,
                            RuntimeTaskProductSettlement {
                                summary: Some(error.clone()),
                                execution_summary: Some(task_execution_summary_candidate(
                                    run_id,
                                    &task,
                                    result,
                                    suggested_tasks,
                                    vec![error.clone()],
                                )),
                                review: review_candidate,
                                diagnostic_note: Some(error.clone()),
                                typed_terminal: None,
                            },
                        )?;
                        Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed { error })
                    }
                }
            }
        }
    }

    async fn settle_resolution(
        &self,
        run_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        runtime_task: &echo_agent::tasks::Task,
        request: echo_agent::tasks::RuntimeTaskResolutionRequest,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskResolution> {
        let product = self
            .resolution_metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&claim.claim_id)
            .unwrap_or_default();
        let committed_payload = product
            .execution_summary
            .as_ref()
            .map(|summary| {
                serde_json::json!({
                    "terminal_status": summary.result.status.as_str(),
                    "summary": &summary.result.summary,
                    "artifacts": &summary.result.artifacts,
                    "verification": &summary.result.verification,
                    "remaining_work": &summary.result.remaining_work,
                    "touched_files": &summary.result.touched_files,
                    "agent_failure": &product.typed_terminal,
                })
            })
            .unwrap_or_else(|| serde_json::json!({}));
        let run_id = run_id.to_string();
        let task_id = runtime_task.spec.id.clone();
        let agent_role = Self::plan_task(runtime_task)?.agent_role;
        let claim = claim.clone();
        let (outcome, run) = self
            .blocking
            .run("settle runtime task resolution", move |store| {
                let outcome = store
                    .settle_runtime_task_resolution(&run_id, &task_id, &claim, request, product)?;
                let run = store.get_run(&run_id)?;
                Ok((outcome, run))
            })
            .await?;
        let terminal_event = match &outcome {
            echo_agent::tasks::RuntimeTaskResolution::Completed => {
                Some(RuntimeEventKind::TaskCompleted)
            }
            echo_agent::tasks::RuntimeTaskResolution::Skipped => {
                Some(RuntimeEventKind::TaskSkipped)
            }
            echo_agent::tasks::RuntimeTaskResolution::TimedOut { .. } => {
                Some(RuntimeEventKind::TaskTimedOut)
            }
            echo_agent::tasks::RuntimeTaskResolution::Failed { .. } => {
                Some(RuntimeEventKind::TaskFailed)
            }
            echo_agent::tasks::RuntimeTaskResolution::Blocked { .. } => {
                Some(RuntimeEventKind::TaskBlocked)
            }
            echo_agent::tasks::RuntimeTaskResolution::Cancelled => {
                Some(RuntimeEventKind::TaskCancelled)
            }
            echo_agent::tasks::RuntimeTaskResolution::Pending
            | echo_agent::tasks::RuntimeTaskResolution::Superseded => None,
        };
        if let Some(run) = run
            && let Some(terminal_event) = terminal_event
        {
            emit_exec(
                self.trace_sink.as_ref(),
                ExecEvent::task(
                    run.workspace_id,
                    run.conversation_id,
                    run.run_id,
                    runtime_task.spec.id.clone(),
                    terminal_event,
                    committed_payload,
                )
                .with_agent(agent_role),
            );
        }
        Ok(outcome)
    }

    async fn abandon_claim(
        &self,
        run_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        runtime_task: &echo_agent::tasks::Task,
        abandonment: echo_agent::tasks::RuntimeClaimAbandonment,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskSettlementOutcome> {
        let (status, summary) = match abandonment {
            echo_agent::tasks::RuntimeClaimAbandonment::Interrupted { disposition } => {
                match disposition {
                    echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled => (
                        echo_agent::tasks::TaskStatus::Cancelled,
                        "dispatch cancelled before resolution".to_string(),
                    ),
                    echo_agent::tasks::RuntimeInterruptionDisposition::Paused { reason } => (
                        echo_agent::tasks::TaskStatus::Paused(reason.clone()),
                        reason,
                    ),
                }
            }
            echo_agent::tasks::RuntimeClaimAbandonment::Failed { error } => {
                (echo_agent::tasks::TaskStatus::Failed(error.clone()), error)
            }
        };
        let terminal_event = match &status {
            echo_agent::tasks::TaskStatus::Cancelled => RuntimeEventKind::TaskCancelled,
            echo_agent::tasks::TaskStatus::Paused(_) => RuntimeEventKind::TaskBlocked,
            echo_agent::tasks::TaskStatus::Failed(_) => RuntimeEventKind::TaskFailed,
            _ => RuntimeEventKind::TaskFailed,
        };
        let payload = serde_json::json!({ "summary": &summary });
        let agent_role = Self::plan_task(runtime_task)?.agent_role;
        let run_id = run_id.to_string();
        let task_id = runtime_task.spec.id.clone();
        let claim = claim.clone();
        let (outcome, run) = self
            .blocking
            .run("settle abandoned runtime task claim", move |store| {
                let outcome = store.settle_runtime_task_claim(
                    &run_id,
                    &task_id,
                    &claim,
                    status,
                    Some(summary),
                )?;
                let run = store.get_run(&run_id)?;
                Ok((outcome, run))
            })
            .await?;
        if outcome == echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
            && let Some(run) = run
        {
            emit_exec(
                self.trace_sink.as_ref(),
                ExecEvent::task(
                    run.workspace_id,
                    run.conversation_id,
                    run.run_id,
                    runtime_task.spec.id.clone(),
                    terminal_event,
                    payload,
                )
                .with_agent(agent_role),
            );
        }
        Ok(outcome)
    }

    async fn failed_task_disposition(
        &self,
        run_id: &str,
        _task: &echo_agent::tasks::Task,
        all_unfinished_failed_or_blocked: bool,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeStopDisposition> {
        if all_unfinished_failed_or_blocked {
            Ok(echo_agent::tasks::RuntimeStopDisposition::Fail)
        } else {
            self.review_stop_disposition(run_id).await
        }
    }

    async fn interruption_disposition(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeInterruptionDisposition> {
        let run_id = run_id.to_string();
        let run = self
            .blocking
            .run("load runtime interruption intent", move |store| {
                store
                    .get_run(&run_id)?
                    .ok_or(StoreError::RunNotFound(run_id))
            })
            .await?;
        Ok(if run.status == TaskRunStatus::Paused {
            echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
                reason: "paused by user".to_string(),
            }
        } else {
            echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled
        })
    }

    async fn settle_interruption(
        &self,
        run_id: &str,
        expected_revision: u64,
        disposition: echo_agent::tasks::RuntimeInterruptionDisposition,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeInterruptionSettlementOutcome> {
        let run_id = run_id.to_string();
        self.blocking
            .run("settle runtime task interruption", move |store| {
                store.settle_runtime_task_interruption(&run_id, expected_revision, disposition)
            })
            .await
    }

    async fn note_stalled(&self, run_id: &str, reason: &str) -> echo_agent::error::Result<()> {
        self.note(run_id, None, reason.to_string()).await
    }
}
