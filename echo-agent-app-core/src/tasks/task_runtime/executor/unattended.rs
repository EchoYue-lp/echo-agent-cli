/// Execute a single task through a selected Subagent or the primary Agent.
///
/// The framework executor holds the per-run Subagent permit; the dispatcher
/// also holds EKO's process permit. This function enforces the same two-level
/// write/shell/LLM limits plus file ownership.
#[allow(clippy::too_many_arguments)] // store + semaphores + locks + sinks all thread through
async fn execute_task(
    store: Arc<TaskRuntimeStore>,
    blocking: TaskRuntimeBlockingAdapter,
    primary_agent: crate::agent_handle::AgentHandle,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    run_id: String,
    claim: echo_agent::tasks::TaskClaim,
    task: PlanTask,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> TaskDispatchResult {
    let task_id = task.id.clone();
    let is_write = !task.kind.is_read_only();
    let load_run_id = run_id.clone();
    let run_context = blocking
        .run("load dispatch run identity", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("failed to load TaskRun identity: {error}"),
            )
        })?;
    let workspace_id = run_context.workspace_id.clone();
    let conversation_id = run_context.conversation_id.clone();
    let root_message_id = run_context.root_message_id.clone();

    // ── U1c phase-1 CP B: per-task unattended preflight ──
    // Re-check the task (kind + tools + shell) before acquiring permits.
    // Chat runs (Attended) skip this; only Unattended runs are checked.
    // Terminal fail on violation — never Paused, never awaits a human.
    {
        let attended_mode = run_context.attended_mode;
        if attended_mode == AttendedMode::Unattended
            && let Err(rejection) =
                preflight_unattended_task(&task, super::task_tools::current_unattended_write_mode())
        {
            let msg = format!(
                "CP B preflight rejected task '{}': {}",
                task_id, rejection.reason
            );
            return Err(TaskDispatchFailure::failed(task_id.clone(), msg));
        }
    }

    // Create a child cancellation token for THIS task and register it with the
    // store. remove_task / update_task can cancel it to stop this Subagent
    // promptly without cancelling sibling tasks. child_token() means run-level
    // cancel still propagates here (child fires when parent fires).
    let task_cancel = cancel.child_token();
    store.register_task_cancel_token(&run_id, &task_id, task_cancel.clone());
    // RAII guard: always unregister on exit (success/fail/cancel), so the
    // token map doesn't leak finished tasks. Owns its key strings to avoid
    // borrowing task_id/run_id (which may be moved later in this function).
    struct TokenGuard {
        store: std::sync::Arc<TaskRuntimeStore>,
        run_id: String,
        task_id: String,
    }
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            self.store
                .unregister_task_cancel_token(&self.run_id, &self.task_id);
        }
    }
    let _token_guard = TokenGuard {
        store: store.clone(),
        run_id: run_id.clone(),
        task_id: task_id.clone(),
    };

    // A PlanTask is a stable plan node; each dispatch attempt is a distinct
    // SubagentRun. Never collapse retries back to the bare task id.
    let attempt = claim.attempt;
    let claim_revision = claim.revision;
    let execution_id = subagent_execution_id(&run_id, &task_id, &claim);
    let contract = subagent_runtime_contract(&primary_agent, &task.agent_role, &task.kind).await;
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        kind = %task.kind.as_str(),
        agent_role = %task.agent_role,
        read_only = task.kind.is_read_only(),
        prompt_chars = task.description.chars().count(),
        "task_runtime: task dispatch start"
    );

    emit_task_started(
        trace_sink.as_ref(),
        &workspace_id,
        &conversation_id,
        &run_id,
        &execution_id,
        &task,
        &contract,
    );

    // Acquire EKO product-resource permits with cancel awareness:
    // - Read-only tasks need no additional write/shell permit; the framework
    //   and EKO process Subagent permits are already held by the dispatcher.
    // - Write tasks (implementation/debugging) take the write permit.
    // - Verification tasks (shell/build/test) take the write permit + the shell
    //   permit (default 1, plan §678-680 shell_concurrency = 1).
    let is_shell = matches!(task.kind, PlanTaskKind::Verification);
    let (_process_write_permit, _write_permit, _process_shell_permit, _shell_permit) = if is_shell {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let process_wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process write permit")),
            p = PROCESS_EXECUTION_GOVERNOR.write.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for write permit")),
            p = write_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired write permit"
        );
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = shell_sem.available_permits(),
            "task_runtime: waiting for shell permit"
        );
        let process_sp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process shell permit")),
            p = PROCESS_EXECUTION_GOVERNOR.shell.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        let sp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for shell permit")),
            p = shell_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired shell permit"
        );
        (Some(process_wp), Some(wp), Some(process_sp), Some(sp))
    } else if is_write {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let process_wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process write permit")),
            p = PROCESS_EXECUTION_GOVERNOR.write.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for write permit")),
            p = write_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired write permit"
        );
        (Some(process_wp), Some(wp), None, None)
    } else {
        (None, None, None, None)
    };

    // Physical safety net below the ownership-safe DAG wave: exact file owners
    // take the same normalized mutex keys. Unknown owners were already kept out
    // of mixed writer waves and remain isolated in their own worktree.
    //
    // Two-layer concurrency:
    // - write_sem: global writer count cap (max_concurrent_writes=4)
    // - per-file TokioMutex: file-level mutual exclusion (1 permit per file)
    let ownership = super::planner::file_ownership(&task);
    let _file_lock_guard = if is_write {
        // CRITICAL: sort files before acquiring locks to prevent classic
        // lock-ordering deadlock. Without this, two tasks declaring the same
        // files in different orders (e.g. [A,B] vs [B,A]) would deadlock when
        // both reach Step 2 concurrently (A waits for B while B waits for A).
        // Sorting guarantees all tasks acquire per-file locks in the same
        // canonical order, breaking any potential wait-for cycle.
        let sorted_files: Vec<String> = ownership
            .known_files()
            .map(|files| files.iter().cloned().collect())
            .unwrap_or_default();

        if sorted_files.is_empty() {
            None
        } else {
            // Step 1: get-or-create per-file mutexes (outer lock held briefly).
            let per_file_mutexes: Vec<Arc<TokioMutex<()>>> = {
                let mut locks = file_write_locks.lock().unwrap_or_else(|e| e.into_inner());
                sorted_files
                    .iter()
                    .map(|f| {
                        locks
                            .entry(f.clone())
                            .or_insert_with(|| Arc::new(TokioMutex::new(())))
                            .clone()
                    })
                    .collect()
            }; // outer lock released here — brief, never held across awaits.

            // Step 2: acquire all per-file locks asynchronously. Overlapping files
            // block here until the previous writer releases its guard.
            let mut guards: Vec<OwnedMutexGuard<()>> = Vec::with_capacity(per_file_mutexes.len());
            for mtx in per_file_mutexes {
                let guard = tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => {
                        return Err(TaskDispatchFailure::cancelled(
                            task_id.clone(),
                            "cancelled while waiting for file write lock",
                        ));
                    }
                    guard = mtx.lock_owned() => guard,
                };
                guards.push(guard);
            }
            Some(FileLockGuard { _guards: guards })
        }
    } else {
        None
    };

    // G4: LLM rate-limit permit — caps concurrent LLM calls to prevent
    // provider rate-limit hits and cost spikes (plan §704).
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        available = llm_sem.available_permits(),
        "task_runtime: waiting for llm permit"
    );
    let _process_llm_permit = tokio::select! {
        biased;
        _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process LLM permit")),
        p = PROCESS_EXECUTION_GOVERNOR.llm.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
    };
    let _llm_permit = tokio::select! {
        biased;
        _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for LLM permit")),
        p = llm_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
    };
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        "task_runtime: acquired llm permit"
    );

    // Summary Chain: gather the summaries of this task's completed
    // dependencies, so the Subagent gets compact upstream context instead of
    // (or in addition to) re-reading everything from scratch (plan §1039).
    let prompt_run_id = run_id.clone();
    let prompt_task = task.clone();
    let (dep_summaries, parent_goal) = blocking
        .run("load task prompt context", move |store| {
            let dependencies =
                collect_dependency_summaries(store.as_ref(), &prompt_run_id, &prompt_task)?;
            let goal = store.get_run(&prompt_run_id)?.map(|run| run.goal);
            Ok((dependencies, goal))
        })
        .await
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("failed to load Subagent prompt context: {error}"),
            )
        })?;

    let workspace_root = primary_workspace_root_for_prompt(
        &contract.isolation_requested,
        primary_agent.read(|agent| agent.working_dir()).await,
    );
    let prompt_payload = crate::subagent_prompt::EkoPromptPayload::planned_task(
        &task,
        &dep_summaries,
        delegation_policy.can_delegate(),
        parent_goal.as_deref(),
        workspace_root.as_deref(),
    )
    .to_value()
    .map_err(|error| {
        TaskDispatchFailure::failed(
            task_id.clone(),
            format!("failed to serialize Subagent prompt payload: {error}"),
        )
    })?;
    let task_input = if task.description.trim().is_empty() {
        task.title.clone()
    } else {
        task.description.clone()
    };

    // Dispatch the task. Three paths, by kind:
    // - Read-only kinds (read_only_review, investigation, test_plan, review,
    //   summary) → delegate to the registered readonly subagent role via Fork.
    //   The child cancel token propagates parent-run cancellation.
    // - Writer kinds (implementation, debugging) delegate to the selected
    //   writer-capable Subagent. Coding uses worktree isolation; data roles use
    //   isolated data workspaces. Dispatch failure is terminal.
    // - Verification (shell/build/test) → MAIN agent executes directly. These
    //   run against the workspace (testing just-written changes), so routing
    //   them to a separate worktree checkout would detach them from the work.
    let is_read_only_task = task.kind.is_read_only();
    let is_writer_task = matches!(
        task.kind,
        PlanTaskKind::Implementation | PlanTaskKind::Debugging
    );
    let dispatch_hooks_from_runtime = !is_read_only_task && !is_writer_task;
    // Resolve the run's root_message_id so the framework can carry it on
    // SubagentEvent::DispatchStarted → execution://event, letting the frontend
    // pin the subagent stream to the right chat message block.
    let controlled_attempt = if is_read_only_task || is_writer_task {
        let framework_executor = primary_agent
            .read(|agent| agent.subagent_executor().clone())
            .await;
        let (control_identity, framework_identity) = super::subagent_control::attempt_identity(
            &run_id,
            &task_id,
            &execution_id,
            claim_revision,
            attempt,
        )
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("invalid Subagent attempt identity: {error}"),
            )
        })?;
        let assigned_run_id = run_id.clone();
        let assigned_task_id = task_id.clone();
        let assigned_execution_id = execution_id.clone();
        let assigned_role = task.agent_role.clone();
        let assigned_title = task.title.clone();
        let assigned_read_only = task.kind.is_read_only();
        let assigned_control_identity = control_identity.clone();
        let assigned_executor = framework_executor.clone();
        let guard = blocking
            .run("persist controlled Subagent assignment", move |store| {
                let guard = store.record_controlled_subagent_assigned(
                    &assigned_run_id,
                    &assigned_task_id,
                    &assigned_execution_id,
                    &assigned_role,
                    &assigned_title,
                    claim_revision,
                    attempt,
                    assigned_read_only,
                    dispatch_hooks_from_runtime,
                    assigned_executor.clone(),
                )?;
                store.deliver_pending_subagent_guidance(
                    &assigned_control_identity,
                    &assigned_executor,
                )?;
                Ok(guard)
            })
            .await
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        Some((framework_identity, guard))
    } else {
        let assigned_run_id = run_id.clone();
        let assigned_task_id = task_id.clone();
        let assigned_execution_id = execution_id.clone();
        let assigned_role = task.agent_role.clone();
        let assigned_title = task.title.clone();
        let assigned_read_only = task.kind.is_read_only();
        blocking
            .run("persist Subagent assignment", move |store| {
                store.record_subagent_assigned(
                    &assigned_run_id,
                    &assigned_task_id,
                    &assigned_execution_id,
                    &assigned_role,
                    &assigned_title,
                    claim_revision,
                    attempt,
                    assigned_read_only,
                    dispatch_hooks_from_runtime,
                )
            })
            .await
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        None
    };
    emit_subagent_started(
        trace_sink.as_ref(),
        &workspace_id,
        &run_id,
        &execution_id,
        &task,
        &contract,
        claim_revision,
        attempt,
        &conversation_id,
        Some(&root_message_id),
    );
    let framework_attempt_identity = controlled_attempt
        .as_ref()
        .map(|(identity, _guard)| identity.clone());
    let result = if is_read_only_task {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            task_chars = task_input.chars().count(),
            "task_runtime: delegating read-only task to subagent"
        );
        let dispatch_result = run_readonly_subagent(
            &primary_agent,
            &run_id,
            &execution_id,
            Some(&root_message_id),
            &task.agent_role,
            &task_input,
            prompt_payload.clone(),
            task.allowed_tools.clone(),
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
            framework_attempt_identity.clone().ok_or_else(|| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    "read-only Subagent is missing its attempt identity",
                )
            })?,
            workspace_io.clone(),
        )
        .await;
        match dispatch_result {
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    terminal_status = ?sub_result.outcome.status,
                    "task_runtime: read-only subagent settled"
                );
                finalize_framework_subagent_result(
                    blocking.clone(),
                    &run_id,
                    &execution_id,
                    sub_result,
                )
                .await
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    error = %e,
                    "task_runtime: read-only subagent failed"
                );
                Err(e)
            }
        }
    } else if is_writer_task {
        // Route to the selected writer-capable Subagent.
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            task_chars = task_input.chars().count(),
            "task_runtime: delegating writer task to subagent"
        );
        let dispatch_result = run_writer_subagent(
            &primary_agent,
            blocking.clone(),
            &run_id,
            &execution_id,
            &task_isolation_id(&run_id, &task_id),
            &task.agent_role,
            &task_input,
            prompt_payload.clone(),
            task.allowed_tools.clone(),
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
            framework_attempt_identity.ok_or_else(|| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    "writer Subagent is missing its attempt identity",
                )
            })?,
            workspace_io.clone(),
        )
        .await;
        match dispatch_result {
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    summary_chars = sub_result.outcome.summary.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    terminal_status = ?sub_result.outcome.status,
                    "task_runtime: writer subagent settled"
                );
                finalize_framework_subagent_result(
                    blocking.clone(),
                    &run_id,
                    &execution_id,
                    sub_result,
                )
                .await
            }
            Err(error) => Err(if task_cancel.is_cancelled() {
                ExecutionFailure::cancelled("task cancelled")
            } else {
                error
            }),
        }
    } else {
        let compiler = crate::subagent_prompt::EkoSubagentPromptCompiler;
        let compiled = compiler.compile_primary_invocation(&SubagentPromptInput {
            agent_name: "primary",
            task: &task_input,
            mode: echo_agent::agent::subagent::ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            parent_context: None,
            inherit_history: None,
            payload: Some(&prompt_payload),
            constraints: &[],
        });
        emit_primary_subagent_isolation_observed(
            trace_sink.as_ref(),
            &workspace_id,
            &conversation_id,
            &run_id,
            &execution_id,
            &task,
            &contract,
        );
        run_main_agent_task(
            &primary_agent,
            blocking.clone(),
            &run_id,
            &task,
            &execution_id,
            &compiled.task_input,
            task_cancel.clone(),
            trace_sink.clone(),
            workspace_io,
        )
        .await
    };

    match result {
        Ok((task_result, full_output, usage)) => {
            // The parent consumes the bounded structured summary; extract
            // suggested_tasks from the full output because that optional block
            // appears before the terminal ## Result contract.
            let suggested_tasks = extract_suggested_tasks_from_subagent_output(&full_output);
            let parent_facing = task_result.summary.trim().to_string();
            tracing::info!(
                run_id = %run_id,
                task_id = %task_id,
                agent_role = %task.agent_role,
                summary_chars = parent_facing.chars().count(),
                output_chars = full_output.chars().count(),
                "task_runtime: task completed"
            );
            let persisted_run_id = run_id.clone();
            let persisted_task_id = task_id.clone();
            let persisted_execution_id = execution_id.clone();
            let persisted_agent_role = task.agent_role.clone();
            let persisted_task_title = task.title.clone();
            let persisted_result = task_result.clone();
            let persisted_output = full_output.clone();
            let persisted_usage = usage.durable.clone();
            let suggestion_note = (!suggested_tasks.is_empty()).then(|| {
                let titles = suggested_tasks
                    .iter()
                    .map(|suggestion| suggestion.title.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "subagent suggested {} follow-up task(s): [{titles}]. Not auto-inserted into plan; promote via task_update if desired.",
                    suggested_tasks.len()
                )
            });
            if let Err(error) = blocking
                .run("persist successful Subagent boundary", move |store| {
                    super::ledger::archive_trace(
                        &persisted_run_id,
                        &persisted_task_id,
                        &persisted_output,
                        None,
                    );
                    super::ledger::write_progress(store.as_ref(), &persisted_run_id, None)?;
                    store.record_subagent_released(SubagentReleaseRecord {
                        run_id: &persisted_run_id,
                        task_id: &persisted_task_id,
                        execution_id: &persisted_execution_id,
                        agent_name: &persisted_agent_role,
                        task_subject: &persisted_task_title,
                        plan_revision: claim_revision,
                        attempt,
                        status: persisted_result.status.as_str(),
                        result: Some(&persisted_result),
                        full_output: Some(&persisted_output),
                        usage: Some(&persisted_usage),
                        dispatch_hook: dispatch_hooks_from_runtime,
                    })?;
                    if let Some(note) = suggestion_note {
                        store.note(&persisted_run_id, Some(&persisted_task_id), &note)?;
                    }
                    Ok(())
                })
                .await
            {
                return Err(TaskDispatchFailure::failed(
                    task_id,
                    format!("Subagent completed but terminal boundary was not persisted: {error}"),
                ));
            }
            // Suggested tasks are persisted in TaskExecutionSummary.suggested_tasks
            // (see put_summary above). They are NOT auto-inserted into the plan —
            // doing so caused unbounded plan expansion + dependent tasks to wait
            // forever on looping parents. The primary agent / user can promote a
            // suggestion via task_update when desired. Record a Note so the
            // suggestions are visible in the event stream regardless.
            let terminal_payload = serde_json::json!({
                "execution_id": &execution_id,
                "plan_revision": claim_revision,
                "attempt": attempt,
                "conversation_id": conversation_id,
                "message_id": root_message_id,
                "output": &full_output,
                "terminal_status": task_result.status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
                "usage": &usage.durable,
            });
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::subagent(
                    workspace_id.clone(),
                    conversation_id.clone(),
                    run_id.clone(),
                    task_id.clone(),
                    execution_id.clone(),
                    subagent_terminal_event(task_result.status),
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            Ok(TaskDispatchSuccess {
                task_id,
                result: task_result,
                full_output,
                suggested_tasks,
            })
        }
        Err(failure) => {
            let status = failure.status;
            let message = failure.message;
            let usage = failure.usage;
            let agent_failure = failure.agent_failure;
            let mut task_result =
                SubagentTaskResult::terminal(status, message.clone(), vec![message.clone()]);
            if let Some(agent_failure) = agent_failure.as_ref() {
                attach_agent_failure_evidence(&mut task_result, agent_failure);
            }
            let persisted_run_id = run_id.clone();
            let persisted_task_id = task_id.clone();
            let persisted_execution_id = execution_id.clone();
            let persisted_agent_role = task.agent_role.clone();
            let persisted_task_title = task.title.clone();
            let persisted_result = task_result.clone();
            let persisted_message = message.clone();
            let persisted_usage = usage.as_ref().map(|value| value.durable.clone());
            if let Err(error) = blocking
                .run("persist failed Subagent boundary", move |store| {
                    store.record_subagent_released(SubagentReleaseRecord {
                        run_id: &persisted_run_id,
                        task_id: &persisted_task_id,
                        execution_id: &persisted_execution_id,
                        agent_name: &persisted_agent_role,
                        task_subject: &persisted_task_title,
                        plan_revision: claim_revision,
                        attempt,
                        status: status.as_str(),
                        result: Some(&persisted_result),
                        full_output: Some(&persisted_message),
                        usage: persisted_usage.as_ref(),
                        dispatch_hook: dispatch_hooks_from_runtime,
                    })
                })
                .await
            {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    %error,
                    "failed to persist Subagent terminal boundary"
                );
            }
            if status == SubagentRunStatus::Cancelled {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    "task_runtime: task cancelled"
                );
            } else {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    error = %message,
                    "task_runtime: task failed"
                );
            }
            let terminal_payload = serde_json::json!({
                "execution_id": &execution_id,
                "plan_revision": claim_revision,
                "attempt": attempt,
                "conversation_id": conversation_id,
                "message_id": root_message_id,
                "error": &message,
                "terminal_status": status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
                "usage": usage.as_ref().map(|value| &value.durable),
                "agent_failure": &agent_failure,
            });
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::subagent(
                    workspace_id.clone(),
                    conversation_id.clone(),
                    run_id.clone(),
                    task_id.clone(),
                    execution_id.clone(),
                    subagent_terminal_event(status),
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            Err(TaskDispatchFailure::from_execution(
                task_id,
                ExecutionFailure {
                    status,
                    message,
                    usage,
                    agent_failure,
                },
            ))
        }
    }
}

const MAX_SUGGESTED_TASKS_PER_SUBAGENT: usize = 5;

#[derive(Debug, serde::Deserialize)]
struct SuggestedTaskEnvelope {
    #[serde(default)]
    suggested_tasks: Vec<RawSuggestedTask>,
}

#[derive(Debug, serde::Deserialize)]
struct RawSuggestedTask {
    title: Option<String>,
    description: Option<String>,
    kind: Option<PlanTaskKind>,
    agent_role: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    why_needed: Option<String>,
    risk: Option<String>,
}

fn extract_suggested_tasks_from_subagent_output(text: &str) -> Vec<SuggestedTask> {
    let mut out = Vec::new();
    for candidate in suggested_task_json_candidates(text) {
        let Ok(envelope) = serde_json::from_str::<SuggestedTaskEnvelope>(&candidate) else {
            continue;
        };
        for raw in envelope.suggested_tasks {
            if out.len() >= MAX_SUGGESTED_TASKS_PER_SUBAGENT {
                return out;
            }
            let Some(task) = normalize_suggested_task(raw) else {
                continue;
            };
            out.push(task);
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

fn suggested_task_json_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for block in text.split("```json").skip(1) {
        if let Some(json) = block.split("```").next() {
            candidates.push(json.trim().to_string());
        }
    }
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        candidates.push(trimmed.to_string());
    }
    candidates
}

fn normalize_suggested_task(raw: RawSuggestedTask) -> Option<SuggestedTask> {
    let title = raw.title.unwrap_or_default().trim().to_string();
    let description = raw.description.unwrap_or_default().trim().to_string();
    if title.is_empty() || description.is_empty() {
        return None;
    }
    Some(SuggestedTask {
        title: title.chars().take(120).collect(),
        description,
        kind: raw.kind.unwrap_or(PlanTaskKind::Investigation),
        agent_role: raw
            .agent_role
            .filter(|role| !role.trim().is_empty())
            .unwrap_or_else(|| "explorer".to_string()),
        dependencies: raw
            .dependencies
            .into_iter()
            .map(|dep| dep.trim().to_string())
            .filter(|dep| !dep.is_empty())
            .take(8)
            .collect(),
        why_needed: raw.why_needed.unwrap_or_default().trim().to_string(),
        risk: raw
            .risk
            .filter(|risk| !risk.trim().is_empty())
            .unwrap_or_else(|| "medium".to_string()),
    })
}

/// Prefers the structured TaskExecutionSummary (persisted by put_summary at
/// task boundary) over the truncated todo.summary text, so downstream Subagents
/// get full context: summary, touched files, decisions, and remaining work.
fn collect_dependency_summaries(
    store: &TaskRuntimeStore,
    run_id: &str,
    task: &PlanTask,
) -> Result<Vec<(String, String)>, StoreError> {
    if task.depends_on.is_empty() {
        return Ok(Vec::new());
    }
    let plan = store
        .get_plan(run_id)?
        .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
    let todos = store.list_todos(run_id)?;
    let summaries = task
        .depends_on
        .iter()
        .map(|dep_id| {
            plan.tasks
                .iter()
                .find(|dependency| &dependency.id == dep_id)
                .map_or(Ok(None), |dependency| {
                    if dependency.status != echo_agent::tasks::TaskStatus::Completed {
                        return Ok(None);
                    }
                    let todo = todos.iter().find(|todo| todo.task_id == dependency.id);
                    // Prefer the structured summary when available; fall back to
                    // the truncated todo text for tasks that predate put_summary.
                    let structured = store.get_summary(run_id, &dependency.id)?.map(|s| {
                        let mut parts: Vec<String> = Vec::new();
                        if !s.result.summary.trim().is_empty() {
                            parts.push(format!("完成: {}", s.result.summary));
                        }
                        if !s.result.touched_files.written.is_empty() {
                            parts.push(format!(
                                "修改文件: {}",
                                s.result.touched_files.written.join(", ")
                            ));
                        }
                        if !s.decisions.is_empty() {
                            parts.push(format!("决策: {}", s.decisions.join("; ")));
                        }
                        (dependency.title.clone(), parts.join(" | "))
                    });
                    Ok(structured.or_else(|| {
                        todo.and_then(|item| item.summary.as_deref())
                            .map(|s| (dependency.title.clone(), s.to_string()))
                    }))
                })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(summaries.into_iter().flatten().collect())
}

struct SubagentRuntimeContract {
    prompt_source: String,
    isolation_requested: String,
    context_in: String,
    returns: String,
}

fn primary_workspace_root_for_prompt(
    isolation_requested: &str,
    workspace_root: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    workspace_root.filter(|_| !matches!(isolation_requested, "worktree" | "workspace"))
}

fn runtime_contract_started_payload(
    contract: &SubagentRuntimeContract,
    task: &PlanTask,
    execution_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "execution_id": execution_id,
        "kind": task.kind.as_str(),
        "agent_role": task.agent_role,
        "title": task.title,
        "task": task.description,
        "prompt_source": contract.prompt_source,
        "isolation_requested": contract.isolation_requested,
        "context_in": contract.context_in,
        "returns": contract.returns,
    })
}

fn runtime_isolation_observed_payload(
    contract: &SubagentRuntimeContract,
    isolation_observed: &str,
) -> serde_json::Value {
    serde_json::json!({
        "isolation_requested": contract.isolation_requested,
        "isolation_observed": isolation_observed,
    })
}

fn emit_task_started(
    sink: Option<&ExecSink>,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::task(
            workspace_id,
            conversation_id,
            run_id,
            task.id.clone(),
            RuntimeEventKind::TaskStarted,
            runtime_contract_started_payload(contract, task, execution_id),
        )
        .with_agent(task.agent_role.clone()),
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_subagent_started(
    sink: Option<&ExecSink>,
    workspace_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
    plan_revision: u64,
    attempt: u32,
    conversation_id: &str,
    message_id: Option<&str>,
) {
    let mut payload = runtime_contract_started_payload(contract, task, execution_id);
    if let serde_json::Value::Object(fields) = &mut payload {
        fields.insert("plan_revision".to_string(), plan_revision.into());
        fields.insert("attempt".to_string(), attempt.into());
        fields.insert("conversation_id".to_string(), conversation_id.into());
        if let Some(message_id) = message_id {
            fields.insert("message_id".to_string(), message_id.into());
        }
    }
    emit_exec(
        sink,
        ExecEvent::subagent(
            workspace_id,
            conversation_id,
            run_id,
            task.id.clone(),
            execution_id,
            RuntimeEventKind::Started,
            payload,
        )
        .with_agent(task.agent_role.clone()),
    );
}

fn emit_primary_subagent_isolation_observed(
    sink: Option<&ExecSink>,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::subagent(
            workspace_id,
            conversation_id,
            run_id,
            task.id.clone(),
            execution_id,
            RuntimeEventKind::IsolationObserved,
            runtime_isolation_observed_payload(contract, "primary"),
        )
        .with_agent(task.agent_role.clone()),
    );
}

async fn subagent_runtime_contract(
    primary_agent: &crate::agent_handle::AgentHandle,
    agent_role: &str,
    kind: &PlanTaskKind,
) -> SubagentRuntimeContract {
    let registry = primary_agent
        .read(|agent| agent.subagent_registry().clone())
        .await;
    let definitions = registry.list_available().await;
    let definition = definitions.iter().find(|def| def.name == agent_role);

    let prompt_source = definition
        .and_then(|def| {
            def.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("prompt_source:").map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string());

    let isolation_requested = definition
        .and_then(|def| {
            def.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("isolation:").map(str::to_string))
        })
        .unwrap_or_else(|| {
            if matches!(kind, PlanTaskKind::Implementation | PlanTaskKind::Debugging) {
                "worktree".to_string()
            } else if kind.is_read_only() {
                "context".to_string()
            } else {
                "primary".to_string()
            }
        });

    SubagentRuntimeContract {
        prompt_source,
        isolation_requested,
        context_in: "task_context + dependency summaries + workspace root".to_string(),
        returns: "TaskExecutionSummary + execution://event trace".to_string(),
    }
}

/// Run a READ-ONLY task by delegating to a registered subagent role via the
/// primary agent's prompt-payload delegation API. Fork mode runs the Subagent
/// on an isolated agent instance under the executor's own semaphore (not the
/// primary agent's execution_mutex), so multiple read-only Subagents run in
/// parallel. The child cancel token propagates parent-run cancellation.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
#[allow(clippy::result_large_err)]
async fn run_readonly_subagent(
    primary_agent: &crate::agent_handle::AgentHandle,
    run_id: &str,
    execution_id: &str,
    message_id: Option<&str>,
    role: &str,
    task_input: &str,
    prompt_payload: serde_json::Value,
    allowed_tools: Vec<String>,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
    attempt_identity: echo_agent::agent::subagent::SubagentAttemptIdentity,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<echo_agent::agent::subagent::SubagentResult, ExecutionFailure> {
    primary_agent
        .read_async(|agent| {
            let task_input = task_input.to_string();
            let prompt_payload = prompt_payload.clone();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let message_id = message_id.map(|s| s.to_string());
            let core_trace_sink = exec_trace_sink_to_core(trace_sink);
            let attempt_identity = attempt_identity.clone();
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let runtime_context = Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: None,
                    run_id: Some(run_id.clone()),
                    turn_id: message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: None,
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                    resource_guards,
                });
                agent
                    .delegate_to_agent_attempt_with_prompt_payload(
                        &role,
                        &task_input,
                        &run_id,
                        cancel,
                        0,
                        runtime_context,
                        Some(allowed_tools),
                        Some(prompt_payload),
                        attempt_identity,
                    )
                    .await
                    .map_err(|error| {
                        ExecutionFailure::from_react(error, "subagent dispatch failed")
                    })
            })
        })
        .await
}

fn exec_trace_sink_to_core(trace_sink: Option<ExecSink>) -> Option<echo_agent::tools::TraceSinkFn> {
    // Wrap an app-layer `ExecSink` into the framework's `TraceSinkFn`
    // (Value-based) so it can be carried across `tokio::spawn` boundaries via
    // `ExternalRunContext.trace_sink`. The app's `scoped_with_ctx_run_id`
    // (task_tools.rs) reads `ctx.trace_sink` back and re-scopes it into
    // `CURRENT_TRACE_SINK` so tools running inside a spawned task (e.g.
    // `task_execute`) can emit execution-flow events.
    //
    // Subagent dispatch itself does NOT use this path — it goes through
    // `SubagentEventBus`. This conversion is only for the main-agent tool path
    // (task_execute / task_create) that runs inside the framework's spawned
    // tool executor and needs to reach the trace_sink.
    trace_sink.map(|sink| {
        Arc::new(move |value: serde_json::Value| {
            if let Ok(ev) = serde_json::from_value::<ExecEvent>(value) {
                sink(ev);
            }
        }) as echo_agent::tools::TraceSinkFn
    })
}

/// Run a CODE-WRITER task (implementation / debugging) by delegating to the
/// registered writer subagent role via Fork dispatch (Sprint 9).
///
/// Mirrors [`run_readonly_subagent`] but with attachment-aware delegation: when
/// the run carries user attachments (images/files), the multimodal variant
/// the message-aware prompt-payload delegation API is used so the writer
/// Subagent sees them (parity with the old in-place `run_main_agent_task` path).
///
/// The registered writer Subagent carries the full write tool set and its
/// definition selects worktree or data-workspace isolation. Coding writes land
/// in an isolated checkout rather than the main workspace.
/// If EKO cannot establish the requested worktree, dispatch hard-fails.
/// rather than silently sharing the main tree.
/// Disjoint exact owners may run concurrently; the DAG scheduler separates
/// overlapping and unknown ownership before dispatch.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
#[allow(clippy::result_large_err)]
async fn run_writer_subagent(
    primary_agent: &crate::agent_handle::AgentHandle,
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    execution_id: &str,
    isolation_id: &str,
    role: &str,
    task_input: &str,
    prompt_payload: serde_json::Value,
    allowed_tools: Vec<String>,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
    attempt_identity: echo_agent::agent::subagent::SubagentAttemptIdentity,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<echo_agent::agent::subagent::SubagentResult, ExecutionFailure> {
    // Rebuild a multimodal Message when the run carries user attachments, so
    // the writer Subagent sees the same images/files as the primary agent would
    // (parity with the main-agent task path in this executor authority).
    let load_run_id = run_id.to_string();
    let run_record = blocking
        .run("load writer Subagent attachments", move |store| {
            store.get_run(&load_run_id)
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!(
                "failed to load writer Subagent attachments: {error}"
            ))
        })?;
    let root_message_id = run_record.as_ref().map(|r| r.root_message_id.clone());
    let conversation_id = run_record.as_ref().map(|r| r.conversation_id.clone());
    let run_message: Option<echo_agent::llm::types::Message> = run_record.as_ref().and_then(|r| {
        if r.attachments.is_empty() {
            None
        } else {
            crate::attachments::build_message_from_refs(task_input, &r.attachments).ok()
        }
    });

    primary_agent
        .read_async(|agent| {
            let task_input = task_input.to_string();
            let prompt_payload = prompt_payload.clone();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let isolation_id = isolation_id.to_string();
            let run_message = run_message.clone();
            let core_trace_sink = exec_trace_sink_to_core(trace_sink);
            let attempt_identity = attempt_identity.clone();
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let runtime_context = Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: conversation_id.clone(),
                    run_id: Some(run_id.clone()),
                    turn_id: root_message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: Some(isolation_id),
                    message_id: root_message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                    resource_guards,
                });
                if let Some(msg) = run_message {
                    agent
                        .delegate_to_agent_attempt_with_message_and_prompt_payload(
                            &role,
                            &task_input,
                            msg,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                            Some(allowed_tools.clone()),
                            Some(prompt_payload.clone()),
                            attempt_identity,
                        )
                        .await
                } else {
                    agent
                        .delegate_to_agent_attempt_with_prompt_payload(
                            &role,
                            &task_input,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                            Some(allowed_tools),
                            Some(prompt_payload),
                            attempt_identity,
                        )
                        .await
                }
                .map_err(|error| {
                    ExecutionFailure::from_react(error, "writer subagent dispatch failed")
                })
            })
        })
        .await
    // The SubagentResult returned by delegation carries the writer's accumulated
    // output, which already includes the appended worktree diff from dispatch_fork's
    // finalize step (Sprint 8). trace_sink is accepted for signature parity with
    // run_main_agent_task but unused here — subagent token/thinking events are
    // emitted by the framework's executor event bus, not this caller.
}

/// Run a MUTATING task (verification) directly on the PRIMARY agent via its
/// versioned streaming contract. These tasks are never delegated to a read-only subagent
/// (readonly Subagents can't write). The write_sem acquired by the caller serializes them,
/// and the primary agent's execution_mutex serializes them further — correct,
/// because mutating work must not race.
///
/// Cancellation: `Agent::execute` is not cancel-aware, so we race it against
/// the cancel token. If the run is cancelled mid-task, we return an error and
/// the task is marked Failed (the run then goes Cancelled/Failed).
fn tool_call_is_replay_safe(agent: &echo_agent::agent::ReactAgent, tool_name: &str) -> bool {
    let Some(tool) = agent.tool_manager().get_tool(tool_name) else {
        return false;
    };
    let permissions = tool.permissions();
    !permissions.iter().any(|permission| {
        matches!(
            permission,
            echo_agent::prelude::ToolPermission::Write
                | echo_agent::prelude::ToolPermission::Execute
                | echo_agent::prelude::ToolPermission::Network
                | echo_agent::prelude::ToolPermission::Sensitive
        )
    })
}

fn tool_call_may_mutate_workspace(agent: &echo_agent::agent::ReactAgent, tool_name: &str) -> bool {
    if UNATTENDED_DIRECT_MUTATION_TOOLS.contains(&tool_name) {
        return true;
    }
    agent
        .tool_manager()
        .get_tool(tool_name)
        .is_some_and(|tool| {
            tool.permissions().iter().any(|permission| {
                matches!(
                    permission,
                    echo_agent::prelude::ToolPermission::Write
                        | echo_agent::prelude::ToolPermission::Execute
                )
            })
        })
}

fn verification_check_from_agent_tool(name: &str, args: &serde_json::Value) -> Option<String> {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    if !matches!(
        normalized.as_str(),
        "shell" | "bash" | "terminal" | "run_code" | "execute_command"
    ) {
        return None;
    }
    ["command", "cmd", "code", "script"]
        .iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn file_access_from_agent_tool(name: &str, args: &serde_json::Value) -> Option<(bool, String)> {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    let write = normalized.contains("write")
        || normalized.contains("edit")
        || normalized.contains("delete")
        || normalized.contains("patch");
    let read = normalized.contains("read")
        || normalized.contains("search")
        || normalized.contains("glob")
        || normalized.contains("grep");
    if !write && !read {
        return None;
    }
    ["path", "file_path", "target", "directory"]
        .iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|path| (write, path.to_string()))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
async fn run_main_agent_task(
    primary_agent: &crate::agent_handle::AgentHandle,
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    prompt: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let run_id = run_id.to_string();
    let execution_id = execution_id.to_string();

    // Preserve the user's attachments for primary verification tasks.
    let load_run_id = run_id.clone();
    let run_record = blocking
        .run("load primary task attachments", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!("failed to load TaskRun identity: {error}"))
        })?;
    let root_message_id = Some(run_record.root_message_id.clone());
    let run_message = if run_record.attachments.is_empty() {
        None
    } else {
        crate::attachments::build_message_from_refs(prompt, &run_record.attachments).ok()
    };

    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let run_message = run_message.clone();
            let execution_id = execution_id.clone();
            let blocking = blocking.clone();
            let run_record = run_record.clone();
            let task = task.clone();
            let working_dir = workspace_io
                .as_ref()
                .map(|scope| scope.data_root().to_path_buf());
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let visible_tools =
                    crate::tool_exposure::initial_visible_tools_for_task_run(&agent.tool_names());
                crate::tool_exposure::record_schema_budget(
                    &agent.tool_definitions(),
                    &visible_tools,
                );
                let runtime_state_id = agent.conversation_id().map(str::to_string);
                let transcript_generation_id = runtime_state_id
                    .as_ref()
                    .filter(|runtime_state_id| {
                        Some(*runtime_state_id) != Some(&run_record.conversation_id)
                    })
                    .cloned();
                let invocation = echo_agent::agent::AgentInvocationContext {
                    history: None,
                    runtime_state_id,
                    transcript_generation_id,
                    input_lifecycle: None,
                    runtime: Some(echo_agent::tools::ExternalRunContext {
                        conversation_id: Some(run_record.conversation_id.clone()),
                        run_id: Some(run_id.clone()),
                        turn_id: root_message_id.clone(),
                        execution_id: Some(execution_id.clone()),
                        isolation_id: None,
                        message_id: root_message_id,
                        cancel: Some(Arc::new(cancel.clone())),
                        trace_sink: exec_trace_sink_to_core(trace_sink.clone()),
                        delegation_policy: None,
                        resource_guards: Vec::new(),
                    }),
                    working_dir,
                    cancel: None,
                    disabled_tools: Some(crate::tool_exposure::disabled_tools()),
                    visible_tools: Some(visible_tools),
                    run_budget: None,
                    resource_guards,
                };
                let event_identity = echo_agent::agent::EventIdentity::from_invocation(&invocation)
                    .map_err(|error| {
                        ExecutionFailure::from_react(error, "invalid task event identity")
                    })?;
                let replay_safe_tools = agent
                    .tool_names()
                    .into_iter()
                    .filter(|name| tool_call_is_replay_safe(agent, name))
                    .collect();
                let sink = EkoAgentTurnSink::for_primary_task(
                    &run_record,
                    &task,
                    &execution_id,
                    blocking.clone(),
                    replay_safe_tools,
                    trace_sink,
                );
                let request = match run_message {
                    Some(message) => TurnRequest::from_message(event_identity, message),
                    None => TurnRequest::new(event_identity, prompt),
                }
                .mode(TurnMode::Execute)
                .cancel(cancel)
                .invocation(invocation);
                let receipt = AgentTurnDriver.drive(agent, request, &sink).await;
                let usage = TaskExecutionUsage::from_turn_receipt(&receipt);
                let observation = sink.finish(receipt.final_answer.as_deref());

                match receipt.outcome {
                    TurnOutcome::Completed => {}
                    TurnOutcome::Cancelled => {
                        return Err(ExecutionFailure::cancelled("task cancelled").with_usage(usage));
                    }
                    TurnOutcome::Failed(failure) => {
                        let message = failure.message.clone();
                        return Err(ExecutionFailure::from_agent_failure(&failure, message)
                            .with_usage(usage));
                    }
                }

                let working_dir = agent.working_dir();
                let mut outcome = echo_agent::agent::subagent::parse_subagent_outcome(
                    &observation.output,
                    echo_agent::agent::subagent::SubagentStatus::Completed,
                    Some(&execution_id),
                    working_dir.as_deref(),
                );
                echo_agent::agent::subagent::merge_observed_evidence(
                    &mut outcome,
                    observation.observed_evidence,
                    observation.observed_artifacts,
                );
                let duration_run_id = run_id.clone();
                let duration_execution_id = execution_id.clone();
                let duration_ms = usage.duration_ms();
                blocking
                    .run("persist primary Subagent duration", move |store| {
                        store.account_subagent_usage(
                            &duration_run_id,
                            &duration_execution_id,
                            "primary_subagent_duration",
                            0,
                            0,
                            duration_ms,
                        )
                    })
                    .await
                    .map_err(|error| {
                        ExecutionFailure::failed(format!(
                            "failed to persist primary Subagent duration: {error}"
                        ))
                        .with_usage(usage.clone())
                    })?;
                Ok((
                    SubagentTaskResult::from_framework_outcome(&outcome),
                    observation.output,
                    usage,
                ))
            })
        })
        .await
}
/// RAII guard that releases file write locks when dropped (G5).
struct FileLockGuard {
    /// Per-file async mutex guards. Dropping releases all per-file locks.
    _guards: Vec<OwnedMutexGuard<()>>,
}

/// Write a terminal Run record to the trace store when available.
/// Best-effort: trace failures are logged but never fail the run.
fn save_trace(
    run_store: Option<&Arc<dyn echo_agent::trace::RunStore>>,
    run_id: &str,
    goal: &str,
    conversation_id: &str,
    status: &str,
) {
    let Some(rs) = run_store else { return };
    let Some(trace_status) = trace_run_status(status) else {
        // The framework trace schema has no Paused state. Omitting this optional
        // diagnostic record is truthful; projecting Paused as Completed is not.
        return;
    };
    let run = echo_agent::trace::Run {
        run_id: run_id.to_string(),
        parent_run_id: None,
        agent_name: "task-runtime".to_string(),
        model: String::new(),
        provider: None,
        turn_id: None,
        execution_id: None,
        session_id: conversation_id.to_string(),
        status: trace_status,
        input: goal.to_string(),
        events: vec![],
        final_output: None,
        error: if status == "failed" {
            Some("run failed".to_string())
        } else {
            None
        },
        token_usage: Default::default(),
        timings: Default::default(),
        started_at: chrono::Utc::now(),
        finished_at: Some(chrono::Utc::now()),
    };
    let rs = rs.clone();
    let log_id = run_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = rs.save(run).await {
            tracing::warn!(run_id = %log_id, error = %e, "trace Run save failed (non-fatal)");
        } else {
            tracing::debug!(run_id = %log_id, "trace Run saved");
        }
    });
}

fn trace_run_status(status: &str) -> Option<echo_agent::trace::RunStatus> {
    match status {
        "completed" => Some(echo_agent::trace::RunStatus::Completed),
        "failed" => Some(echo_agent::trace::RunStatus::Failed),
        "cancelled" => Some(echo_agent::trace::RunStatus::Cancelled),
        "paused" => None,
        _ => None,
    }
}

// ── Unattended run adapter (cron / background AgentChat) ────────────────
