/// Launch an unattended run through the unified TaskRuntime executor,
/// bypassing the chat routing path. Generic over the source kind (cron /
/// background AgentChat) and route.
///
/// Creates a run, then drives the agent's ReAct loop in the run's context so
/// the agent itself calls `task_create` (to materialise the plan) and
/// `task_execute` (which internally calls `execute_run`). Simple prompts that
/// the agent answers directly (without `task_execute`) are materialized as a
/// one-task Plan and must pass the same requirement/evidence completion gate.
///
/// **Why not call `execute_run` directly?** `execute_run` requires a plan to
/// already exist (`store.get_plan → NoPlan` if absent). The plan is created
/// by the agent during its ReAct loop via the `task_create` tool. Skipping
/// the agent loop would leave the plan empty and the run would fail
/// immediately. This mirrors how `launch_unified_run` (chat path) works.
///
/// The run is created with `attended_mode = Unattended` so the configured
/// write preflight applies inside `task_execute` / `execute_task`.
#[allow(clippy::too_many_arguments)] // run identity + Agent + cancellation + write policy form the driver boundary
#[cfg(test)]
async fn launch_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    source_kind: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
) -> Result<String, ExecError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    create_unattended_run(&store, &run_id, source_kind, source_id, fire_id, prompt)?;

    drive_unattended_run(
        store.clone(),
        primary_agent,
        &run_id,
        source_id,
        fire_id,
        prompt,
        parent_cancel,
        write_mode,
        None,
    )
    .await
}

pub(crate) fn create_unattended_run(
    store: &TaskRuntimeStore,
    run_id: &str,
    source_kind: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
) -> Result<(), ExecError> {
    let conversation_id = format!("{source_kind}:{source_id}:{fire_id}");

    // 1. Create the run in Pending, attended_mode = Unattended.
    store.create_run_for_active_workspace(
        run_id,
        &conversation_id,
        "", // root_message_id — no chat message for unattended run
        DomainProfile::General,
        prompt,
        "parallel_readonly_delegation",
        AttendedMode::Unattended,
    )?;
    store.configure_run_continuation(run_id, true, true, None, None)?;

    // 2. Transition Pending → Running.
    store.transition_run(run_id, TaskRunStatus::Running)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // retained compatibility wrapper around drive_agent_run
pub(crate) async fn drive_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<String, ExecError> {
    drive_agent_run(
        store,
        primary_agent,
        run_id,
        source_id,
        fire_id,
        prompt,
        parent_cancel,
        write_mode,
        RunPlanPolicy::AllowDirect,
        None,
        workspace_io,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn drive_owned_agent_turn(
    blocking: TaskRuntimeOperation,
    primary_agent: &crate::agent_handle::AgentHandle,
    run: &TaskRun,
    turn_id: &str,
    prompt: &str,
    cancel: CancellationToken,
    disabled_tools: HashSet<String>,
    trace_sink: Option<ExecSink>,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<(TurnReceipt, EkoAgentTurnObservation), ExecError> {
    let run_id = run.run_id.clone();
    let conversation_id = run.conversation_id.clone();
    let message_id = Some(run.root_message_id.clone()).filter(|value| !value.trim().is_empty());
    let turn_id = turn_id.to_string();
    let prompt = prompt.to_string();
    let core_trace_sink = exec_trace_sink_to_core(trace_sink.clone());
    let trace_sink_for_scope = trace_sink.clone();
    let working_dir = workspace_io
        .as_ref()
        .map(|scope| scope.data_root().to_path_buf());
    let resource_guards = workspace_io
        .as_ref()
        .map(crate::state::WorkspaceIoInvocation::resource_guards)
        .unwrap_or_default();
    super::task_tools::with_run_context(
        run_id.clone(),
        cancel.clone(),
        trace_sink_for_scope,
        async {
            let agent_inner = primary_agent.inner().clone();
            let agent = agent_inner.read().await;
            let visible_tools = crate::tool_exposure::initial_visible_tools_for_profile(
                run.domain_profile,
                &agent.tool_names(),
            );
            crate::tool_exposure::record_schema_budget(&agent.tool_definitions(), &visible_tools);
            let mutating_tools: HashSet<String> = agent
                .tool_names()
                .into_iter()
                .filter(|name| tool_call_may_mutate_workspace(&agent, name))
                .collect();
            let mut disabled_tools = disabled_tools;
            disabled_tools.extend(mutating_tools.iter().cloned());
            let runtime_state_id = agent.conversation_id().map(str::to_string);
            let transcript_generation_id = runtime_state_id
                .as_ref()
                .filter(|runtime_state_id| Some(*runtime_state_id) != Some(&conversation_id))
                .cloned();
            let invocation = echo_agent::agent::AgentInvocationContext {
                history: None,
                runtime_state_id,
                transcript_generation_id,
                input_lifecycle: None,
                runtime: Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: Some(conversation_id),
                    run_id: Some(run_id.clone()),
                    turn_id: Some(turn_id.clone()),
                    execution_id: None,
                    isolation_id: None,
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: None,
                    resource_guards: Vec::new(),
                    subagent_lineage: None,
                    uplink: None,
                }),
                working_dir,
                cancel: None,
                disabled_tools: Some(disabled_tools),
                visible_tools: Some(visible_tools),
                run_budget: None,
                resource_guards,
            };
            let event_identity = echo_agent::agent::EventIdentity::from_invocation(&invocation)
                .map_err(|error| {
                    ExecError::Other(format!("invalid run agent event identity: {error}"))
                })?;
            let sink = EkoAgentTurnSink::for_run(
                run,
                &turn_id,
                blocking,
                mutating_tools,
                trace_sink.clone(),
            );
            let request = TurnRequest::new(event_identity, prompt)
                .mode(TurnMode::Execute)
                .cancel(cancel)
                .invocation(invocation);
            let receipt = AgentTurnDriver.drive(&*agent, request, &sink).await;
            let observation = sink.finish(receipt.final_answer.as_deref());
            Ok((receipt, observation))
        },
    )
    .await
}

/// Drive an already-created Run through an independent primary Agent's ReAct
/// loop. The Agent may materialize a plan through `task_create` +
/// `task_execute`; direct completion is controlled by [`RunPlanPolicy`].
///
/// Unattended direct read-only work stays in the original checkout. Workspace
/// mutation is routed through formal writer PlanTasks, whose existing Subagent
/// integration path creates a worktree only when the writer is dispatched.
#[allow(clippy::too_many_arguments)]
pub async fn drive_agent_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
    plan_policy: RunPlanPolicy,
    trace_sink: Option<ExecSink>,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<String, ExecError> {
    let child_cancel = parent_cancel.child_token();
    let blocking = TaskRuntimeOperation::new(store.clone());
    let admission_run_id = run_id.to_string();
    let admission_cancel = child_cancel.clone();
    let (_cancel_registration, run_for_scope) = blocking
        .run("register agent-driven run", move |store| {
            let registration = store
                .register_run_cancellation(&admission_run_id, admission_cancel)
                .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
            let run = store
                .get_run(&admission_run_id)?
                .ok_or(StoreError::RunNotFound(admission_run_id))?;
            Ok((registration, run))
        })
        .await
        .map_err(|error| ExecError::Other(format!("register run cancellation: {error}")))?;
    let attended_mode = run_for_scope.attended_mode;
    let prompt = unattended_run_prompt(prompt, attended_mode, write_mode);
    let mut disabled_tools =
        direct_mutation_disabled_tools(attended_mode, write_mode).unwrap_or_default();
    disabled_tools.extend(crate::tool_exposure::disabled_tools());
    let continuation_configured = blocking
        .run("validate agent-driven continuation", {
            let run_id = run_id.to_string();
            move |store| {
                store.get_run_state(&run_id).map(|snapshot| {
                    snapshot
                        .and_then(|state| state.continuation)
                        .is_some_and(|continuation| continuation.enabled)
                })
            }
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    if !continuation_configured {
        return Err(ExecError::Other(format!(
            "run {run_id} must configure continuation in its creation transaction"
        )));
    }

    let mut origin = RunTurnOrigin::User;
    loop {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let claim_run_id = run_id.to_string();
        let claim_turn_id = turn_id.clone();
        let claim = blocking
            .run("claim owned agent RunTurn", move |store| {
                store.claim_run_turn(
                    &claim_run_id,
                    &claim_turn_id,
                    origin,
                    TurnVisibility::Internal,
                )
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        match claim {
            super::store::RunTurnClaimOutcome::Started(_) => {}
            super::store::RunTurnClaimOutcome::NotSubmitted(reason) => {
                return Err(ExecError::Other(format!(
                    "owned RunTurn was not submitted for {run_id}: {reason:?}"
                )));
            }
        }
        let (mut turn_receipt, turn_observation) = drive_owned_agent_turn(
            blocking.clone(),
            &primary_agent,
            &run_for_scope,
            &turn_id,
            &prompt,
            child_cancel.clone(),
            disabled_tools.clone(),
            trace_sink.clone(),
            workspace_io.clone(),
        )
        .await?;
        let mut terminal = turn_receipt.outcome;
        let plan_exists = blocking
            .run("inspect agent-driven run plan", {
                let run_id = run_id.to_string();
                move |store| store.get_plan(&run_id).map(|plan| plan.is_some())
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        if matches!(&terminal, TurnOutcome::Completed)
            && !child_cancel.is_cancelled()
            && plan_policy == RunPlanPolicy::AllowDirect
            && !plan_exists
        {
            if turn_observation.mutating_tool_observed {
                terminal = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "direct_mutation_requires_plan",
                    "a mutating tool was attempted outside a materialized TaskPlan",
                ));
            } else if !turn_observation.output.trim().is_empty()
                && let Err(error) =
                    materialize_direct_completion(&store, run_id, turn_observation).await
            {
                terminal = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "direct_completion",
                    format!("failed to persist direct completion evidence: {error}"),
                ));
            }
        }
        if let TurnOutcome::Failed(failure) = &terminal {
            tracing::warn!(
                source_id,
                run_id,
                failure_category = ?failure.category,
                terminal_kind = ?failure.terminal_kind,
                failure_code = %failure.code,
                retryable = failure.retryable,
                http_status = ?failure.http_status,
                "Run agent emitted typed terminal failure"
            );
        }
        turn_receipt.outcome = terminal;
        let persisted = super::turn_lifecycle::persist_run_turn_terminal(
            &blocking,
            run_id,
            &turn_receipt,
        )
        .await
        .map_err(ExecError::Other)?;
        let decision = super::turn_lifecycle::decide_after_persisted_run_turn(
            &blocking,
            &store,
            run_id,
            &turn_receipt,
            persisted,
            trace_sink.as_ref(),
        )
        .await
        .map_err(ExecError::Other)?;
        if decision == super::turn_lifecycle::RunTurnDecision::Stop {
            break;
        }
        match super::continuation::await_owned_continue(&store, run_id, &child_cancel).await {
            super::continuation::OwnedContinueOutcome::Ready => {
                origin = RunTurnOrigin::Continuation;
            }
            super::continuation::OwnedContinueOutcome::Stop => break,
            super::continuation::OwnedContinueOutcome::Cancelled => {
                let cancelled_run_id = run_id.to_string();
                blocking
                    .run("cancel owned agent continuation", move |store| {
                        store.transition_run(&cancelled_run_id, TaskRunStatus::Cancelled)?;
                        store.stop_owned_command_cells(&cancelled_run_id)
                    })
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break;
            }
            super::continuation::OwnedContinueOutcome::Shutdown => {
                let paused_run_id = run_id.to_string();
                blocking
                    .run(
                        "pause owned agent continuation for shutdown",
                        move |store| {
                            store
                                .request_pause_with_reason(
                                    &paused_run_id,
                                    RunPauseReason::BootRecovery,
                                    Some("application shutdown interrupted an owned continuation"),
                                )
                                .map(|_| ())
                        },
                    )
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break;
            }
        }
    }

    // `task_execute`, direct completion, or the shared RunTurn lifecycle owns
    // settlement. The driver only verifies the durable result.
    let final_run_id = run_id.to_string();
    let final_status = blocking
        .run("load agent-driven run outcome", move |store| {
            store
                .get_run(&final_run_id)?
                .map(|run| run.status)
                .ok_or(StoreError::RunNotFound(final_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;

    match final_status {
        TaskRunStatus::Completed => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run completed"
            );
            // B5.1 design: cron/unattended runs use an Ephemeral/DirectReview
            // memory policy — their results surface to the user via the kept
            // worktree diff artifact (above), NOT via recall. So we deliberately
            // do NOT write_memory_candidate here (cron has no recall closure;
            // adding one would be a separate, scoped change). This is distinct
            // from the autonomous chat path (create_complex_task), which DOES
            // block-write its completion memory for recall.
        }
        TaskRunStatus::Failed => {
            tracing::warn!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run failed"
            );
        }
        TaskRunStatus::Cancelled => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run cancelled"
            );
        }
        TaskRunStatus::Paused => {
            tracing::info!(
                source_id = %source_id,
                run_id = %run_id,
                "Agent-driven run paused and remains resumable"
            );
        }
        status => {
            return Err(ExecError::Other(format!(
                "run {run_id} did not settle after its owned continuation; read back {}",
                status.as_str()
            )));
        }
    }

    let settled_run_id = run_id.to_string();
    let settled = blocking
        .run("verify agent-driven run settlement", move |store| {
            store
                .get_run(&settled_run_id)?
                .ok_or(StoreError::RunNotFound(settled_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    if !matches!(
        settled.status,
        TaskRunStatus::Completed
            | TaskRunStatus::Failed
            | TaskRunStatus::Cancelled
            | TaskRunStatus::Paused
    ) {
        return Err(ExecError::Other(format!(
            "run {run_id} did not reach a durable terminal or paused state; read back {}",
            settled.status.as_str()
        )));
    }

    Ok(run_id.to_string())
}

async fn materialize_direct_completion(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    observation: EkoAgentTurnObservation,
) -> Result<(), ExecError> {
    let final_answer = observation.output;
    let load_run_id = run_id.to_string();
    let run = TaskRuntimeOperation::new(store.clone())
        .run("load direct completion run", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    let title = {
        let value = run.goal.chars().take(120).collect::<String>();
        if value.trim().is_empty() {
            "Complete the requested task".to_string()
        } else {
            value
        }
    };
    let task_id = "direct-answer";
    let plan = TaskPlan {
        plan_id: format!("plan:{run_id}"),
        run_id: run_id.to_string(),
        revision: 0,
        domain_profile: run.domain_profile,
        goal_revision: run.goal_revision,
        goal_sha256: run.goal_sha256,
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask {
            id: task_id.to_string(),
            title,
            description: run.goal,
            kind: PlanTaskKind::Summary,
            agent_role: "primary-agent".to_string(),
            domain_profile: run.domain_profile,
            ..PlanTask::default()
        }],
    };
    let mut framework_outcome = echo_agent::subagent::parse_subagent_outcome(
        &final_answer,
        echo_agent::subagent::SubagentStatus::Completed,
        Some(&format!("{run_id}:direct-answer")),
        None,
    );
    echo_agent::subagent::merge_observed_evidence(
        &mut framework_outcome,
        observation.observed_evidence,
        observation.observed_artifacts,
    );
    let summary = TaskExecutionSummary {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        subagent_name: "primary-agent".to_string(),
        outcome: framework_outcome,
        decisions: Vec::new(),
        next_implications: Vec::new(),
        suggested_tasks: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    super::revisioned_runtime::commit_direct_completion(
        store.clone(),
        plan,
        summary,
        final_answer,
    )
    .await
    .map_err(|error| ExecError::Other(format!("commit direct TaskPlan: {error}")))?;
    Ok(())
}

pub(crate) async fn drive_existing_cron_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: String,
    cron_task_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
) -> Result<String, ExecError> {
    drive_unattended_run(
        store.clone(),
        primary_agent,
        &run_id,
        cron_task_id,
        fire_id,
        prompt,
        parent_cancel,
        UnattendedWriteMode::default(),
        None,
    )
    .await?;
    let status_run_id = run_id.clone();
    let status = TaskRuntimeOperation::new(store.clone())
        .run("load cron run outcome", move |store| {
            store
                .get_run(&status_run_id)?
                .map(|run| run.status)
                .ok_or(StoreError::RunNotFound(status_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    match status {
        TaskRunStatus::Completed => Ok(run_id),
        TaskRunStatus::Failed => Err(ExecError::Other(format!(
            "cron run {run_id} finished with failed status"
        ))),
        TaskRunStatus::Cancelled => {
            Err(ExecError::Other(format!("cron run {run_id} was cancelled")))
        }
        TaskRunStatus::Paused => Err(ExecError::Other(format!(
            "cron run {run_id} paused and requires attention"
        ))),
        other => Err(ExecError::Other(format!(
            "cron run {run_id} ended in non-terminal status {}",
            other.as_str()
        ))),
    }
}

// ── Unattended preflight (dual-checkpoint, spec §4.2 v2) ───────────────

/// Preflight error for unattended runs — terminal, never Paused.
#[derive(Debug, Clone)]
pub struct PreflightRejection {
    pub reason: String,
}

/// Tool-name allowlist for unattended `ReadOnlyPlanNoShell` runs.
///
/// §A: A2 (allow network) — local read-only tools + readonly network tools.
/// Write / execute / shell tools are NOT on this list.
/// Tool names verified against actual `Tool::name()` registrations.
const UNATTENDED_READONLY_TOOLS: &[&str] = &[
    // Local read-only
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "code_search",
    "task_list",
    "task_execute", // plan materialisation trigger
    // Read-only network (§A = A2)
    "web_search",
    "web_fetch",
];

/// Checkpoint A: scan the full plan after materialisation, before execution.
///
/// Returns `Ok(())` if every task in the plan passes the three-layer check:
/// 1. task kind in `is_unattended_readonly_allowed()` whitelist
/// 2. every `allowed_tools` entry is in `UNATTENDED_READONLY_TOOLS`
/// 3. no shell/test commands (verification must be empty)
///
/// The three layers are enforced only when `mode` is [`UnattendedWriteMode::Disabled`]
/// (D7 stage 2). Under `Worktree` / `InPlace` the safety comes from isolation
/// rather than prohibition, so all layers are skipped.
///
/// On violation → `Err(PreflightRejection)` — terminal fail, never Paused.
pub fn preflight_unattended_plan(
    tasks: &[PlanTask],
    mode: UnattendedWriteMode,
) -> Result<(), PreflightRejection> {
    // Under Worktree / InPlace, write safety is provided by the execution
    // environment (isolated worktree or user consent), not by banning.
    if mode.writes_allowed() {
        return Ok(());
    }
    // Disabled: stage-1 read-only enforcement.
    for t in tasks {
        // Layer 1: task kind whitelist
        if !t.kind.is_unattended_readonly_allowed() {
            return Err(PreflightRejection {
                reason: format!(
                    "task kind '{}' is not allowed in unattended ReadOnlyPlanNoShell mode \
                     (allowed: ReadOnlyReview, Investigation, Summary)",
                    t.kind.as_str()
                ),
            });
        }
        // Layer 2: tool allowlist (only if the task declares tools)
        for tool_name in &t.allowed_tools {
            if !UNATTENDED_READONLY_TOOLS.contains(&tool_name.as_str()) {
                return Err(PreflightRejection {
                    reason: format!(
                        "tool '{}' is not in the unattended readonly allowlist (task '{}')",
                        tool_name, t.id
                    ),
                });
            }
        }
        // Layer 3: no shell/test commands
        if !t.execution_checks.is_empty() {
            return Err(PreflightRejection {
                reason: format!(
                    "task '{}' declares execution_checks/shell commands — \
                     shell is DisabledByDefault in unattended mode",
                    t.id
                ),
            });
        }
    }
    Ok(())
}

/// Checkpoint B: per-task preflight — same three layers, for a single task.
/// Called before each task acquires its permit in `execute_task`.
pub fn preflight_unattended_task(
    task: &PlanTask,
    mode: UnattendedWriteMode,
) -> Result<(), PreflightRejection> {
    preflight_unattended_plan(std::slice::from_ref(task), mode)
}
