#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::subagent::SubagentPromptCompiler;
    use futures::future::BoxFuture;
    use futures::stream::{self, BoxStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedTurnAgent {
        script: fn() -> Vec<AgentEvent>,
    }

    struct PermissionCountingTool {
        name: String,
        permission: echo_agent::prelude::ToolPermission,
        calls: Arc<AtomicUsize>,
    }

    impl PermissionCountingTool {
        fn new(
            name: &str,
            permission: echo_agent::prelude::ToolPermission,
            calls: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                permission,
                calls,
            }
        }
    }

    impl echo_agent::tools::Tool for PermissionCountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Dynamically registered permission test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })
        }

        fn execute<'a>(
            &'a self,
            _parameters: echo_agent::tools::ToolParameters,
        ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(echo_agent::tools::ToolResult::success(
                    "dynamic tool executed",
                ))
            })
        }

        fn permissions(&self) -> Vec<echo_agent::prelude::ToolPermission> {
            vec![self.permission]
        }
    }

    impl ScriptedTurnAgent {
        fn new(script: fn() -> Vec<AgentEvent>) -> Self {
            Self { script }
        }
    }

    impl Agent for ScriptedTurnAgent {
        fn name(&self) -> &str {
            "scripted-task-turn"
        }

        fn model_name(&self) -> &str {
            "scripted-model"
        }

        fn system_prompt(&self) -> &str {
            ""
        }

        fn execute<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<String>> {
            Box::pin(async { Ok(String::new()) })
        }

        fn execute_stream<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<
            'a,
            echo_agent::error::Result<BoxStream<'a, echo_agent::error::Result<AgentEvent>>>,
        > {
            let events = (self.script)();
            Box::pin(async move {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as BoxStream<'a, _>)
            })
        }
    }

    fn turn_identity(
        run_id: &str,
        turn_id: &str,
    ) -> Result<echo_agent::agent::EventIdentity, String> {
        echo_agent::agent::EventIdentity::for_chat(
            Some("task-turn-conversation".to_string()),
            turn_id,
            turn_id,
            Some(run_id.to_string()),
        )
        .map_err(|error| error.to_string())
    }

    async fn drive_scripted_run_turn(
        run: &TaskRun,
        turn_id: &str,
        script: fn() -> Vec<AgentEvent>,
    ) -> Result<TurnReceipt, String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let sink = EkoAgentTurnSink::for_run(
            run,
            turn_id,
            TaskRuntimeBlockingAdapter::new(store),
            HashSet::new(),
            None,
        );
        let request =
            TurnRequest::new(turn_identity(&run.run_id, turn_id)?, "test").mode(TurnMode::Execute);
        Ok(AgentTurnDriver
            .drive(&ScriptedTurnAgent::new(script), request, &sink)
            .await)
    }

    fn compiled_task_prompt(
        task: &PlanTask,
        dependency_summaries: &[(String, String)],
        delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
        user_goal: Option<&str>,
    ) -> Result<String, String> {
        let payload = crate::subagent_prompt::EkoPromptPayload::planned_task(
            task,
            dependency_summaries,
            delegation_policy.can_delegate(),
            user_goal,
            None,
        )
        .to_value()?;
        let compiler = crate::subagent_prompt::EkoSubagentPromptCompiler;
        Ok(compiler
            .compile_invocation(&SubagentPromptInput {
                agent_name: &task.agent_role,
                task: &task.description,
                mode: echo_agent::agent::subagent::ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::Fresh,
                parent_context: None,
                inherit_history: None,
                payload: Some(&payload),
                constraints: &[],
            })
            .task_input)
    }

    async fn drive_dynamic_permission_case(
        permission: echo_agent::prelude::ToolPermission,
        tool_name: &str,
        source_id: &str,
    ) -> Result<(TaskRunStatus, usize, bool), String> {
        use echo_agent::testing::MockLlmClient;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("dynamic-permission")
                .then_tool_call("dynamic-call", tool_name, r#"{"path":"README.md"}"#)
                .with_response("x".repeat(1_301)),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("dynamic-permission")
                .llm_client(mock)
                .tool(Box::new(PermissionCountingTool::new(
                    tool_name,
                    permission,
                    calls.clone(),
                )))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            source_id,
            "fire-1",
            "exercise a dynamically registered tool",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let status = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .map(|run| run.status)
            .ok_or_else(|| "dynamic permission run missing".to_string())?;
        let has_plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .is_some();
        Ok((status, calls.load(Ordering::SeqCst), has_plan))
    }

    #[tokio::test]
    async fn task_turn_driver_rejects_stream_without_terminal() -> Result<(), String> {
        fn missing_terminal() -> Vec<AgentEvent> {
            vec![AgentEvent::Token("partial output".to_string())]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("missing-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing-terminal run was not created".to_string())?;
        let receipt =
            drive_scripted_run_turn(&run, "missing-terminal-turn", missing_terminal).await?;

        assert!(matches!(receipt.outcome, TurnOutcome::Failed(_)));
        assert!(receipt.final_answer.is_none());
        assert_eq!(
            TaskExecutionUsage::from_turn_receipt(&receipt)
                .durable
                .tokens_used,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_turn_usage_counts_only_provider_reported_events() -> Result<(), String> {
        fn usage_script() -> Vec<AgentEvent> {
            vec![
                AgentEvent::LlmUsage {
                    model: "unknown-usage".to_string(),
                    prompt_tokens: 100,
                    completion_tokens: 200,
                    total_tokens: 300,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: false,
                },
                AgentEvent::LlmUsage {
                    model: "reported-usage".to_string(),
                    prompt_tokens: 3,
                    completion_tokens: 4,
                    total_tokens: 7,
                    cached_prompt_tokens: 1,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: true,
                },
                AgentEvent::FinalAnswer("done".to_string()),
            ]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "reported-usage".to_string(),
            title: "Count reported usage".to_string(),
            agent_role: "primary".to_string(),
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        store
            .configure_run_continuation(&run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        let execution_id = format!("{run_id}:reported-usage:1:1");
        store
            .record_subagent_assigned(
                &run_id,
                &task.id,
                &execution_id,
                &task.agent_role,
                &task.title,
                1,
                1,
                false,
                false,
            )
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reported-usage run was not created".to_string())?;
        let sink = EkoAgentTurnSink::for_primary_task(
            &run,
            &task,
            &execution_id,
            TaskRuntimeBlockingAdapter::new(store.clone()),
            HashSet::new(),
            None,
        );
        let request = TurnRequest::new(turn_identity(&run_id, "reported-usage-turn")?, "test")
            .mode(TurnMode::Execute);
        let receipt = AgentTurnDriver
            .drive(&ScriptedTurnAgent::new(usage_script), request, &sink)
            .await;

        assert!(matches!(receipt.outcome, TurnOutcome::Completed));
        assert_eq!(receipt.prompt_tokens, 3);
        assert_eq!(receipt.completion_tokens, 4);
        assert_eq!(receipt.llm_calls, 1);
        let subagent_run = store
            .list_subagent_runs(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|candidate| candidate.subagent_run_id == execution_id)
            .ok_or_else(|| "reported-usage SubagentRun was not persisted".to_string())?;
        assert_eq!(subagent_run.usage.tokens_used, Some(7));
        Ok(())
    }

    #[tokio::test]
    async fn task_turn_driver_preserves_typed_provider_timeout_and_cancel() -> Result<(), String> {
        fn provider_failure() -> Vec<AgentEvent> {
            let failure = echo_agent::error::AgentFailure {
                category: echo_agent::error::AgentFailureCategory::Llm,
                terminal_kind: echo_agent::error::AgentTerminalKind::TimedOut,
                retryable: true,
                code: "llm_timeout".to_string(),
                http_status: Some(504),
                message: "provider timed out".to_string(),
            };
            vec![AgentEvent::Error {
                source: "llm".to_string(),
                message: failure.message.clone(),
                failure,
            }]
        }

        fn cancelled() -> Vec<AgentEvent> {
            vec![AgentEvent::Cancelled]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("typed-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "typed-terminal run was not created".to_string())?;
        let failed = drive_scripted_run_turn(&run, "provider-timeout", provider_failure).await?;
        match failed.outcome {
            TurnOutcome::Failed(failure) => {
                assert_eq!(
                    failure.category,
                    echo_agent::error::AgentFailureCategory::Llm
                );
                assert_eq!(
                    failure.terminal_kind,
                    echo_agent::error::AgentTerminalKind::TimedOut
                );
                assert!(failure.retryable);
                assert_eq!(failure.code, "llm_timeout");
                assert_eq!(failure.http_status, Some(504));
            }
            other => return Err(format!("expected typed provider failure, got {other:?}")),
        }
        let cancelled = drive_scripted_run_turn(&run, "cancelled-turn", cancelled).await?;
        assert!(matches!(cancelled.outcome, TurnOutcome::Cancelled));
        Ok(())
    }

    #[test]
    fn typed_provider_timeout_requeues_then_settles_canonical_timed_out_status()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "provider-timeout".to_string(),
            title: "Call provider".to_string(),
            max_retries: 1,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let failure = echo_agent::error::AgentFailure {
            category: echo_agent::error::AgentFailureCategory::Llm,
            terminal_kind: echo_agent::error::AgentTerminalKind::TimedOut,
            retryable: true,
            code: "llm_timeout".to_string(),
            http_status: Some(504),
            message: "provider timed out".to_string(),
        };

        for expected_outcome in ["pending", "timed_out"] {
            let snapshot = store
                .load_runtime_plan_snapshot(&run_id)
                .map_err(|error| error.to_string())?;
            let runtime_task = snapshot
                .tasks
                .iter()
                .find(|candidate| candidate.spec.id == task.id)
                .cloned()
                .ok_or_else(|| "provider timeout task missing".to_string())?;
            let claim = match store
                .claim_runtime_task(&run_id, &runtime_task, snapshot.revision)
                .map_err(|error| error.to_string())?
            {
                echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
                echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                    return Err("provider timeout claim unexpectedly reloaded".to_string());
                }
            };
            let mut result = SubagentTaskResult::terminal(
                SubagentRunStatus::TimedOut,
                failure.message.clone(),
                vec![failure.message.clone()],
            );
            attach_agent_failure_evidence(&mut result, &failure);
            let resolution = store
                .settle_runtime_task_resolution(
                    &run_id,
                    &task.id,
                    &claim,
                    echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                        failure_fingerprint: Some(
                            crate::tasks::task_runtime::turn_lifecycle::agent_failure_fingerprint(
                                &failure,
                            ),
                        ),
                        error: failure.message.clone(),
                        exhaustion: echo_agent::tasks::RuntimeRetryExhaustion::TimedOut,
                    },
                    RuntimeTaskProductSettlement {
                        summary: Some(failure.message.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            &run_id,
                            &task,
                            result,
                            Vec::new(),
                            vec![failure.message.clone()],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: Some(failure.clone()),
                    },
                )
                .map_err(|error| error.to_string())?;
            match expected_outcome {
                "pending" => assert_eq!(
                    resolution,
                    echo_agent::tasks::RuntimeTaskResolution::Pending
                ),
                "timed_out" => assert!(matches!(
                    resolution,
                    echo_agent::tasks::RuntimeTaskResolution::TimedOut { .. }
                )),
                _ => return Err("invalid expected timeout outcome".to_string()),
            }
        }

        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == task.id)
            .ok_or_else(|| "provider timeout Todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::TimedOut);
        let summary = store
            .get_summary(&run_id, &task.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider timeout summary missing".to_string())?;
        assert!(summary.result.evidence.iter().any(|evidence| {
            evidence.kind == "agent_failure"
                && evidence
                    .attributes
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    == Some("llm_timeout")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_write_execute_tools_are_disabled_before_handler_dispatch() -> Result<(), String>
    {
        let write = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Write,
            "read_file",
            "dynamic-write",
        )
        .await?;
        assert_eq!(write, (TaskRunStatus::Failed, 0, false));

        let execute = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Execute,
            "grep",
            "dynamic-execute",
        )
        .await?;
        assert_eq!(execute, (TaskRunStatus::Failed, 0, false));

        let read = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Read,
            "read_file",
            "dynamic-read",
        )
        .await?;
        assert_eq!(read, (TaskRunStatus::Completed, 1, true));
        Ok(())
    }

    #[test]
    fn unattended_worktree_mode_routes_mutations_through_formal_plans() {
        let disabled =
            direct_mutation_disabled_tools(AttendedMode::Unattended, UnattendedWriteMode::Worktree)
                .unwrap_or_default();

        assert!(disabled.contains("shell"));
        assert!(disabled.contains("apply_patch"));
        assert!(disabled.contains("git_commit"));
        assert!(!disabled.contains("read_file"));
        assert!(!disabled.contains("task_create"));
        assert!(!disabled.contains("task_execute"));

        let prompt = unattended_run_prompt(
            "update the implementation",
            AttendedMode::Unattended,
            UnattendedWriteMode::Worktree,
        );
        assert!(prompt.contains("formal plan"));
        assert!(prompt.contains("only when their Subagent is actually dispatched"));
        assert!(prompt.ends_with("update the implementation"));
    }

    #[test]
    fn independent_runs_hide_mutations_unless_in_place_is_explicit() {
        assert!(
            direct_mutation_disabled_tools(AttendedMode::Attended, UnattendedWriteMode::Worktree,)
                .is_some_and(|disabled| disabled.contains("apply_patch"))
        );
        assert!(
            direct_mutation_disabled_tools(AttendedMode::Unattended, UnattendedWriteMode::InPlace,)
                .is_none()
        );
        assert_eq!(
            unattended_run_prompt(
                "inspect the repository",
                AttendedMode::Unattended,
                UnattendedWriteMode::InPlace,
            ),
            "inspect the repository"
        );
    }

    #[test]
    fn paused_run_is_not_projected_as_completed_trace() {
        assert_eq!(trace_run_status("paused"), None);
        assert_eq!(
            trace_run_status("completed"),
            Some(echo_agent::trace::RunStatus::Completed)
        );
    }

    // ── Preflight tests (Phase B) ─────────────────────────────────────────

    /// Helper: build a `PlanTask` stub with just the fields the preflight
    /// gate actually inspects (kind / allowed_tools / verification).
    fn preflight_task(
        id: &str,
        kind: PlanTaskKind,
        tools: &[&str],
        verification: &[&str],
    ) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            kind,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            required_artifacts: Vec::new(),
            execution_checks: verification.iter().map(|s| s.to_string()).collect(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 0,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
            claim: None,
            sort_order: 0,
        }
    }

    fn ownership_task(id: &str, kind: PlanTaskKind, files: &[&str]) -> PlanTask {
        let mut task = preflight_task(id, kind, &[], &[]);
        task.files = files.iter().map(|file| file.to_string()).collect();
        task
    }

    #[test]
    fn ownership_wave_runs_disjoint_writers_together() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("writer-a", PlanTaskKind::Implementation, &["src/a.rs"]),
            ownership_task("writer-b", PlanTaskKind::Debugging, &["src/b.rs"]),
            ownership_task("reader", PlanTaskKind::Investigation, &["src/a.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["writer-a", "writer-b", "reader"]);
    }

    #[test]
    fn ownership_wave_defers_overlapping_writer() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("writer-a", PlanTaskKind::Implementation, &["src/shared.rs"]),
            ownership_task("writer-b", PlanTaskKind::Debugging, &["src/shared.rs"]),
            ownership_task("writer-c", PlanTaskKind::Implementation, &["src/c.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["writer-a", "writer-c"]);
    }

    #[test]
    fn ownership_wave_unknown_writer_serializes_from_writers_but_not_readers() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("unknown", PlanTaskKind::Implementation, &[]),
            ownership_task("writer", PlanTaskKind::Implementation, &["src/a.rs"]),
            ownership_task("reader", PlanTaskKind::Review, &["src/a.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["unknown", "reader"]);
    }

    #[test]
    fn runtime_contract_distinguishes_requested_and_observed_isolation() -> Result<(), String> {
        let contract = SubagentRuntimeContract {
            prompt_source: "builtin:implementer".to_string(),
            isolation_requested: "worktree".to_string(),
            context_in: "task context".to_string(),
            returns: "summary".to_string(),
        };
        let task = PlanTask {
            id: "task-1".to_string(),
            title: "Implement change".to_string(),
            description: "Update the runtime".to_string(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".to_string(),
            ..PlanTask::default()
        };
        let started = runtime_contract_started_payload(&contract, &task, "run-1:task-1:7:2");
        if started.get("isolation").is_some() {
            return Err(
                "legacy isolation field must not claim configured isolation happened".into(),
            );
        }
        if started
            .get("isolation_requested")
            .and_then(|value| value.as_str())
            != Some("worktree")
        {
            return Err("started event must report requested worktree isolation".into());
        }
        if started.get("isolation_observed").is_some() {
            return Err("started event must not invent observed isolation".into());
        }
        if started.get("execution_id").and_then(|value| value.as_str()) != Some("run-1:task-1:7:2")
        {
            return Err("started event must preserve the revision-scoped execution id".into());
        }

        let fallback = runtime_isolation_observed_payload(&contract, "primary-fallback");
        if fallback
            .get("isolation_observed")
            .and_then(|value| value.as_str())
            != Some("primary-fallback")
        {
            return Err("writer fallback must report primary-fallback observation".into());
        }
        Ok(())
    }

    #[test]
    fn isolated_subagent_prompt_uses_only_dispatch_time_workspace() {
        let root = std::path::PathBuf::from("/workspace/main");

        assert_eq!(
            primary_workspace_root_for_prompt("context", Some(root.clone())),
            Some(root.clone())
        );
        assert_eq!(
            primary_workspace_root_for_prompt("primary", Some(root.clone())),
            Some(root.clone())
        );
        assert_eq!(
            primary_workspace_root_for_prompt("worktree", Some(root.clone())),
            None
        );
        assert_eq!(
            primary_workspace_root_for_prompt("workspace", Some(root)),
            None
        );
    }

    #[tokio::test]
    async fn writer_runtime_contract_requests_worktree_without_claiming_fallback()
    -> Result<(), String> {
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = crate::agent_handle::AgentHandle::new(agent);

        let contract =
            subagent_runtime_contract(&handle, "missing-writer", &PlanTaskKind::Implementation)
                .await;
        if contract.isolation_requested != "worktree" {
            return Err(format!(
                "writer must request worktree isolation, got {}",
                contract.isolation_requested
            ));
        }
        Ok(())
    }

    #[test]
    fn primary_isolation_event_reaches_sink_before_terminal() -> Result<(), String> {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<ExecEvent>::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink: ExecSink = Arc::new(move |event| {
            sink_recorded
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
        });
        let task = PlanTask {
            id: "task-1".to_string(),
            title: "Inspect runtime".to_string(),
            description: "Inspect context lifecycle".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            ..PlanTask::default()
        };
        let contract = SubagentRuntimeContract {
            prompt_source: "builtin:explorer".to_string(),
            isolation_requested: "primary".to_string(),
            context_in: "task context".to_string(),
            returns: "summary".to_string(),
        };

        emit_task_started(
            Some(&sink),
            "workspace-1",
            "conversation-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
        );
        emit_subagent_started(
            Some(&sink),
            "workspace-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
            1,
            1,
            "conversation-1",
            Some("message-1"),
        );
        emit_primary_subagent_isolation_observed(
            Some(&sink),
            "workspace-1",
            "conversation-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
        );
        emit_exec(
            Some(&sink),
            ExecEvent::subagent(
                "workspace-1",
                "conversation-1",
                "run-1",
                "task-1",
                "task-1:1",
                RuntimeEventKind::Completed,
                serde_json::json!({"output": "done"}),
            ),
        );

        let events = recorded.lock().unwrap_or_else(|error| error.into_inner());
        let event_names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
        if event_names != ["task_started", "started", "isolation_observed", "completed"] {
            return Err(format!("unexpected event ordering: {event_names:?}"));
        }
        let started = events
            .get(1)
            .ok_or_else(|| "missing started event".to_string())?;
        let observed = events
            .get(2)
            .ok_or_else(|| "missing isolation observation".to_string())?;
        if events.first().map(|event| event.scope) != Some(ExecEventScope::Task)
            || events.get(1).map(|event| event.scope) != Some(ExecEventScope::Subagent)
            || events
                .get(1)
                .and_then(|event| event.subagent_run_id.as_deref())
                != Some("task-1:1")
        {
            return Err("task and Subagent event scopes were not separated".to_string());
        }
        if started.payload.get("isolation").is_some() || observed.payload.get("isolation").is_some()
        {
            return Err("backend must not emit the legacy isolation field".to_string());
        }
        if started
            .payload
            .get("isolation_requested")
            .and_then(|value| value.as_str())
            != Some("primary")
            || observed
                .payload
                .get("isolation_observed")
                .and_then(|value| value.as_str())
                != Some("primary")
        {
            return Err("requested/observed isolation fields were not delivered".to_string());
        }
        Ok(())
    }

    #[test]
    fn preflight_disabled_rejects_write_kinds() -> Result<(), String> {
        // B1: stage-1 regression — under Disabled, write kinds are rejected.
        let task = preflight_task("t1", PlanTaskKind::Implementation, &[], &[]);
        let reason = match preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled) {
            Err(error) => error.reason,
            Ok(_) => return Err("write kind was accepted under Disabled".to_string()),
        };
        assert!(
            reason.contains("implementation"),
            "reason should mention 'implementation', got {reason:?}"
        );
        Ok(())
    }

    #[test]
    fn subagent_output_can_suggest_followup_tasks() -> Result<(), String> {
        let output = r#"
Read the runtime path and found one missing branch.

```json
{
  "suggested_tasks": [
    {
      "title": "Verify resume branch",
      "description": "Trace resume_task_run through the runtime store.",
      "kind": "investigation",
      "agent_role": "explorer",
      "dependencies": ["t1"],
      "why_needed": "The current task found an unverified resume path.",
      "risk": "low"
    }
  ]
}
```
"#;
        let tasks = extract_suggested_tasks_from_subagent_output(output);
        assert_eq!(tasks.len(), 1);
        let task = tasks
            .first()
            .ok_or_else(|| "expected one suggested task".to_string())?;
        assert_eq!(task.title, "Verify resume branch");
        assert_eq!(task.kind, PlanTaskKind::Investigation);
        assert_eq!(task.agent_role, "explorer");
        assert_eq!(task.dependencies, vec!["t1".to_string()]);
        Ok(())
    }

    #[test]
    fn preflight_disabled_rejects_write_tools() -> Result<(), String> {
        // B1: under Disabled, tools outside the readonly allowlist are rejected.
        let task = preflight_task("t1", PlanTaskKind::Investigation, &["apply_patch"], &[]);
        let reason = match preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled) {
            Err(error) => error.reason,
            Ok(_) => return Err("write tool was accepted under Disabled".to_string()),
        };
        assert!(
            reason.contains("apply_patch"),
            "reason should mention 'apply_patch', got {reason:?}"
        );
        Ok(())
    }

    #[test]
    fn preflight_disabled_rejects_verification_shell() -> Result<(), String> {
        // B1: under Disabled, any verification (shell) entry is rejected.
        let task = preflight_task("t1", PlanTaskKind::Investigation, &[], &["cargo test"]);
        let reason = match preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled) {
            Err(error) => error.reason,
            Ok(_) => {
                return Err(
                    "execution_checks shell command was accepted under Disabled".to_string()
                );
            }
        };
        assert!(
            reason.contains("execution_checks") || reason.contains("shell"),
            "reason should mention execution_checks/shell, got {reason:?}"
        );
        Ok(())
    }

    #[test]
    fn preflight_disabled_passes_readonly_readonly() {
        // B1: read-only task with read-only tools and no verification passes.
        let task = preflight_task(
            "t1",
            PlanTaskKind::ReadOnlyReview,
            &["read_file", "grep"],
            &[],
        );
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_ok(),
            "readonly plan should pass under Disabled, got {result:?}"
        );
    }

    #[test]
    fn preflight_worktree_permits_write_kinds_and_tools() {
        // B2: under Worktree, write safety comes from isolation — the
        // preflight gate is fully skipped.
        let write_task = preflight_task(
            "w1",
            PlanTaskKind::Implementation,
            &["apply_patch", "shell"],
            &["cargo check"],
        );
        let result = preflight_unattended_plan(&[write_task], UnattendedWriteMode::Worktree);
        assert!(
            result.is_ok(),
            "write task should pass under Worktree (safety from isolation), got {result:?}"
        );
    }

    #[test]
    fn preflight_inplace_permits_write_kinds_and_tools() {
        // B3: under InPlace, user has explicitly consented — preflight is
        // fully skipped.
        let write_task = preflight_task(
            "w1",
            PlanTaskKind::Implementation,
            &["apply_patch", "shell"],
            &["cargo check"],
        );
        let result = preflight_unattended_plan(&[write_task], UnattendedWriteMode::InPlace);
        assert!(
            result.is_ok(),
            "write task should pass under InPlace (user consent), got {result:?}"
        );
    }

    // ── Phase 3.4 regression ─────────────────────────────────────────────

    #[tokio::test]
    async fn launch_unattended_run_returns_run_id() -> Result<(), String> {
        // Phase 3.4-1: launch_unattended_run must return the run_id so callers
        // (submit) can hand it to the Tauri layer. A simple prompt (mock returns
        // "ok", agent never calls task_execute) is materialized as a one-task
        // Plan and completes through the shared evidence gate (Q5).
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Arc;
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let direct_answer = "x".repeat(1_301);
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call("direct-read", "read_file", r#"{"path":"README.md"}"#)
                .with_response_usage(
                    direct_answer.clone(),
                    echo_agent::llm::types::Usage {
                        prompt_tokens: Some(3),
                        completion_tokens: Some(4),
                        total_tokens: Some(7),
                        ..Default::default()
                    },
                ),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .tool(Box::new(
                    MockTool::new("read_file")
                        .with_parameters(serde_json::json!({
                            "type": "object",
                            "properties": { "path": { "type": "string" } },
                            "required": ["path"]
                        }))
                        .with_response("project documentation"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let cancel = echo_agent::agent::CancellationToken::new();
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "src-1",
            "fire-1",
            "hello",
            cancel,
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        // The returned id must key a real run whose direct answer was promoted
        // into the same revisioned Plan + Evidence contract as a delegated run.
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run should exist".to_string())?;
        assert_eq!(
            run.status,
            TaskRunStatus::Completed,
            "direct run events: {:?}",
            store
                .list_events(&run_id, 0)
                .map_err(|error| error.to_string())?
        );
        let continuation = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "direct completion continuation missing".to_string())?;
        assert_eq!(continuation.tokens_used, 7);
        assert_eq!(
            continuation
                .last_turn
                .as_ref()
                .map(|turn| (turn.input_tokens, turn.output_tokens)),
            Some((3, 4))
        );
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct completion plan should exist".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(
            plan.tasks.first().map(|task| task.id.as_str()),
            Some("direct-answer")
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "direct-answer")
            .ok_or_else(|| "direct completion Todo missing".to_string())?;
        assert_eq!(todo.summary.as_deref(), Some(direct_answer.as_str()));
        let summary = store
            .get_summary(&run_id, "direct-answer")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct completion summary missing".to_string())?;
        assert!(
            summary.result.evidence.iter().any(|evidence| {
                evidence.kind == "file_read" && evidence.subject == "README.md"
            })
        );
        let report = store
            .completion_gate_report(&run_id)
            .map_err(|error| error.to_string())?;
        assert!(report.ready, "direct completion evidence: {report:?}");
        let journal =
            std::fs::read_to_string(shadow_root.path().join(&run_id).join("events.jsonl"))
                .map_err(|error| error.to_string())?;
        let frames = journal
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let frame = frames
            .iter()
            .find(|frame| {
                frame
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|records| {
                        records.iter().any(|record| {
                            record
                                .get("event")
                                .and_then(|event| event.get("event_type"))
                                .and_then(serde_json::Value::as_str)
                                == Some("plan_revision_committed")
                        })
                    })
            })
            .ok_or_else(|| "direct completion transaction frame missing".to_string())?;
        let event_types = frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "direct completion frame has no records".to_string())?
            .iter()
            .filter_map(|record| {
                record
                    .get("event")
                    .and_then(|event| event.get("event_type"))
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            [
                "plan_revision_committed",
                "task_started",
                "note",
                "task_completed",
            ]
        );
        let completion_frame = frames
            .iter()
            .find(|frame| {
                frame
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|records| {
                        let kinds = records
                            .iter()
                            .filter_map(|record| {
                                record
                                    .get("event")
                                    .and_then(|event| event.get("event_type"))
                                    .and_then(serde_json::Value::as_str)
                            })
                            .collect::<Vec<_>>();
                        kinds == ["run_turn_finished", "run_status_changed"]
                    })
            })
            .ok_or_else(|| {
                "RunTurn terminal and Goal completion were not committed atomically".to_string()
            })?;
        assert!(completion_frame.get("records").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn owned_run_turn_uses_durable_provider_retry_before_direct_completion()
    -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_response("recovered provider answer"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "provider-retry",
            "fire-1",
            "recover from provider failure",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider retry run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let state = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider retry state missing".to_string())?;
        let continuation = state
            .continuation
            .ok_or_else(|| "provider retry continuation missing".to_string())?;
        assert!(continuation.provider_retry.is_none());
        assert!(continuation.next_turn_ordinal >= 2);
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        assert!(
            events
                .iter()
                .any(|event| { event.event_type == RuntimeEventKind::RunProviderRetryScheduled })
        );
        assert!(events.iter().any(|event| {
            event.event_type == RuntimeEventKind::RunTurnFinished
                && event
                    .payload
                    .get("agent_failure")
                    .and_then(|failure| failure.get("code"))
                    .and_then(serde_json::Value::as_str)
                    == Some("llm_network")
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
                .count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn direct_completion_rejects_real_apply_patch_tool_path() -> Result<(), String> {
        use echo_agent::testing::{MockLlmClient, MockTool};

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call(
                    "direct-write",
                    "apply_patch",
                    r#"{"path":"src/lib.rs","patch":"unsafe mutation"}"#,
                )
                .with_response("mutation attempted"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .tool(Box::new(
                    MockTool::new("apply_patch")
                        .with_parameters(serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "patch": { "type": "string" }
                            },
                            "required": ["path", "patch"]
                        }))
                        .with_response("must not execute directly"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "direct-write",
            "fire-1",
            "attempt a direct mutation",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct mutation run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Failed);
        assert!(
            store
                .get_plan(&run_id)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        assert!(events.iter().any(|event| {
            event.event_type == RuntimeEventKind::RunTurnFinished
                && event
                    .payload
                    .get("agent_failure")
                    .and_then(|failure| failure.get("code"))
                    .and_then(serde_json::Value::as_str)
                    == Some("direct_mutation_requires_plan")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn agent_run_requires_materialized_plan_when_policy_demands_it() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let run_id = "require-plan-run";
        store
            .create_run(
                run_id,
                "default",
                "conversation:test",
                "message:test",
                DomainProfile::AcademicResearch,
                "review the evidence",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .with_response("direct answer without plan"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        drive_agent_run(
            store.clone(),
            agent,
            run_id,
            "test",
            "fire",
            "materialize and execute a formal plan",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
            RunPlanPolicy::RequirePlan,
            None,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run should exist".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let report = store
            .completion_gate_report(run_id)
            .map_err(|error| error.to_string())?;
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.code == CompletionBlockerCode::NoPlan)
        );
        Ok(())
    }

    #[test]
    fn concurrency_limits_clamp_pool_value() {
        // composite_parallelism reports 0/1/N; Subagents clamp to [1,8].
        // We can't easily build a pool in a unit test, so test the clamp math.
        let clamp = |n: usize| n.clamp(1, 8);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(1), 1);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(20), 8);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_runtime_blocking_adapter_keeps_async_heartbeat_responsive() -> Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let operation = tokio::spawn(async move {
            adapter
                .run("blocking adapter heartbeat test", move |_store| {
                    let _ignored = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            StoreError::InvalidPlan(format!(
                                "blocking adapter test release failed: {error}"
                            ))
                        })?;
                    Ok(())
                })
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx)
            .await
            .map_err(|_| "blocking operation did not start".to_string())?
            .map_err(|_| "blocking operation start signal was dropped".to_string())?;
        let heartbeat_started = std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "async heartbeat stalled behind TaskRuntime file I/O".to_string())?;
        if heartbeat_started.elapsed() >= std::time::Duration::from_millis(250) {
            return Err("async heartbeat did not remain responsive".to_string());
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release blocking operation: {error}"))?;
        operation
            .await
            .map_err(|error| format!("blocking adapter task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_blocking_operation_finishes_after_caller_drop() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store.clone());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_in_operation = completed.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            adapter
                .run_owned("caller drop contract", move || {
                    let _ignored = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
                    completed_in_operation.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        entered_rx
            .await
            .map_err(|_| "blocking operation never started".to_string())?;
        caller.abort();
        let caller_result = caller.await;
        if !caller_result.is_err_and(|error| error.is_cancelled()) {
            return Err("blocking caller was not cancelled".to_string());
        }
        let shutdown_store = store.clone();
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_operations().await });
        tokio::task::yield_now().await;
        if shutdown.is_finished() {
            return Err("operation shutdown ignored the accepted blocking task".to_string());
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release detached operation: {error}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !completed.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "accepted blocking operation stopped with its caller".to_string())?;
        shutdown
            .await
            .map_err(|error| format!("operation shutdown failed to join: {error}"))??;
        if store.active_operation_count() != 0 {
            return Err("operation supervisor did not return to idle".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn sealed_operation_admission_cannot_revive_after_join() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store.clone());
        let parked_adapter = adapter.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let parked = tokio::spawn(async move {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            parked_adapter.reserve_settlement("parked settlement after seal")
        });
        entered_rx
            .await
            .map_err(|_| "parked settlement never reached its admission barrier".to_string())?;
        store.shutdown_operations().await?;
        release_tx
            .send(())
            .map_err(|_| "failed to release parked settlement".to_string())?;
        let result = parked
            .await
            .map_err(|error| format!("parked settlement task failed to join: {error}"))?;
        if !result.is_err_and(|error| error.to_string().contains("admission is closed")) {
            return Err("sealed TaskRuntime admission accepted a post-join settlement".to_string());
        }
        if store.active_operation_count() != 0 {
            return Err("post-join settlement revived TaskRuntime operation activity".to_string());
        }
        Ok(())
    }

    #[test]
    fn task_prompt_is_read_only_for_reviews() -> Result<(), String> {
        let task = PlanTask {
            id: "t1".into(),
            title: "Review chat.rs".into(),
            description: "find bugs".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            files: vec!["chat.rs".into()],
            acceptance_criteria: vec!["report root cause".into()],
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            Some("Fix the GUI context runtime"),
        )?;
        assert!(p.contains("User goal:"));
        assert!(p.contains("Fix the GUI context runtime"));
        assert!(p.contains("READ-ONLY"));
        assert!(p.contains("chat.rs"));
        assert!(p.contains("report root cause"));
        assert!(p.contains("Delegation: disabled"));
        assert!(!p.contains("## Result"));
        Ok(())
    }

    #[test]
    fn task_prompt_marks_empty_writer_scope_as_unknown() -> Result<(), String> {
        let task = PlanTask {
            id: "t2".into(),
            title: "Apply fix".into(),
            description: "patch the bug".into(),
            kind: PlanTaskKind::Implementation,
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            None,
        )?;
        assert!(!p.contains("READ-ONLY"));
        assert!(p.contains("UNKNOWN-SCOPE WRITE"));
        assert!(p.contains("serializes this writer"));
        Ok(())
    }

    #[test]
    fn task_prompt_allows_nested_delegation_when_policy_allows() -> Result<(), String> {
        let task = PlanTask {
            id: "t2_delegate".into(),
            title: "Coordinate review".into(),
            description: "split investigation across specialists".into(),
            kind: PlanTaskKind::Investigation,
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy {
                can_spawn_subagents: true,
                delegate_depth: 0,
                max_delegate_depth: 2,
            },
            None,
        )?;
        assert!(p.contains("tightly scoped child Subagent help is allowed"));
        assert!(p.contains("within this PlanTask"));
        assert!(p.contains("must not control the global plan"));
        assert!(!p.contains("Delegation: disabled"));
        Ok(())
    }

    #[test]
    fn run_outcome_failed_carries_task_id() -> Result<(), String> {
        let o = RunOutcome::Failed {
            failed_task_id: Some("t3".into()),
            error: "boom".into(),
        };
        match o {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id.as_deref(), Some("t3"));
            }
            other => return Err(format!("expected failed outcome, got {other:?}")),
        }
        Ok(())
    }

    /// Integration-ish test: a 4-task read-only wave + 1 implementation
    /// dependent should complete with all todos Completed, using an in-memory
    /// store. We can't run a real agent in a unit test, so this exercises the
    /// store/state-machine side only (the dispatcher path is covered by the
    /// GUI walkthrough in PR 6 + an integration test).
    #[tokio::test]
    async fn store_transitions_through_running_to_completed() -> Result<(), String> {
        use std::sync::Arc;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        // Seed a run + plan via the public store API, then drive the state
        // machine the way the runtime plan adapter would.
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "g",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("g"),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;

        // Simulate the executor: Running, mark task running then
        // completed, then Running → Completed.
        store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Running,
                Some("code_reviewer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Completed,
                Some("code_reviewer"),
                Some("done"),
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .map_err(|error| error.to_string())?;

        let run = store
            .get_run("r1")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run r1 missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let todos = store.list_todos("r1").map_err(|error| error.to_string())?;
        let todo = todos.first().ok_or_else(|| "todo t1 missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert!(todo.summary.as_deref() == Some("done"));
        Ok(())
    }

    // ── Runtime DAG integration tests with a scripted dispatcher ──
    // These exercise the scheduling core — frontier computation, dependency
    // resolution, failure propagation, cancellation, stall detection — without
    // a real LLM. The dispatcher returns scripted results keyed by task id.

    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex as StdMutex;

    struct RecordingExecutionTargetResolver {
        agent: crate::agent_handle::AgentHandle,
        calls: StdMutex<Vec<(crate::agent_router::AgentAddress, TaskExecutionTarget)>>,
    }

    #[async_trait::async_trait]
    impl super::super::execution_target::TaskExecutionTargetResolver
        for RecordingExecutionTargetResolver
    {
        async fn acquire(
            &self,
            leader: &crate::agent_router::AgentAddress,
            target: &TaskExecutionTarget,
        ) -> Result<crate::agent_pool::AgentPoolExecutionLease, String> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((leader.clone(), target.clone()));
            Ok(crate::agent_pool::AgentPoolExecutionLease::unpooled(
                self.agent.clone(),
            ))
        }
    }

    type ScriptedDispatchResult = Result<(SubagentTaskResult, String), String>;

    /// A dispatcher that returns scripted results per task id and records the
    /// order tasks were dispatched. Semaphores/locks are ignored (the mock
    /// answers instantly).
    struct ScriptedDispatcher {
        /// task_id → result to return. Missing id → generic success.
        results: StdMutex<StdHashMap<String, ScriptedDispatchResult>>,
        /// Dispatch order, appended as tasks are picked up.
        order: StdMutex<Vec<String>>,
        /// task_id → integration error returned after review.
        integration_failures: StdMutex<StdHashMap<String, String>>,
        gates: StdMutex<StdHashMap<String, Arc<ScriptedDispatchGate>>>,
        returned_count: std::sync::atomic::AtomicUsize,
        returned: tokio::sync::Notify,
    }

    struct ScriptedDispatchGate {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl ScriptedDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(StdHashMap::new()),
                order: StdMutex::new(Vec::new()),
                integration_failures: StdMutex::new(StdHashMap::new()),
                gates: StdMutex::new(StdHashMap::new()),
                returned_count: std::sync::atomic::AtomicUsize::new(0),
                returned: tokio::sync::Notify::new(),
            })
        }
        /// Script a success result for `id`.
        fn succeed(self: &Arc<Self>, id: &str, summary: &str) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    id.into(),
                    Ok((successful_task_result(summary), summary.to_string())),
                );
        }
        /// Script a structured terminal result for `id`.
        fn respond(self: &Arc<Self>, id: &str, result: SubagentTaskResult) {
            let full_output = result.summary.clone();
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((result, full_output)));
        }
        /// Script a bounded parent summary plus a distinct complete review output.
        fn respond_with_output(
            self: &Arc<Self>,
            id: &str,
            result: SubagentTaskResult,
            full_output: &str,
        ) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((result, full_output.to_string())));
        }
        /// Script a failure result for `id`.
        fn fail(self: &Arc<Self>, id: &str, err: &str) {
            self.results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.into(), Err(err.into()));
        }
        fn order(&self) -> Vec<String> {
            self.order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
        fn fail_integration(self: &Arc<Self>, id: &str, error: &str) {
            self.integration_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_string(), error.to_string());
        }
        fn gate(self: &Arc<Self>, id: &str) -> Arc<ScriptedDispatchGate> {
            let gate = Arc::new(ScriptedDispatchGate {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            self.gates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_string(), gate.clone());
            gate
        }

        async fn wait_for_returns(&self, expected: usize) {
            loop {
                let returned = self.returned.notified();
                if self
                    .returned_count
                    .load(std::sync::atomic::Ordering::Acquire)
                    >= expected
                {
                    return;
                }
                returned.await;
            }
        }
    }

    impl TaskDispatcher for Arc<ScriptedDispatcher> {
        fn dispatch(
            &self,
            _store: Arc<TaskRuntimeStore>,
            _blocking: TaskRuntimeBlockingAdapter,
            context: echo_agent::tasks::TaskSubagentContext,
            _claim: echo_agent::tasks::TaskClaim,
            task: PlanTask,
            _write_sem: Arc<Semaphore>,
            _shell_sem: Arc<Semaphore>,
            _llm_sem: Arc<Semaphore>,
            _file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
            _trace_sink: Option<ExecSink>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>>
        {
            let results = self
                .results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            let gate = self
                .gates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            self.order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(task.id.clone());
            if let Some(gate) = gate.as_ref() {
                gate.started.notify_one();
            }
            let task_id = task.id.clone();
            let dispatcher = self.clone();
            Box::pin(async move {
                if let Some(gate) = gate {
                    tokio::select! {
                        _ = context.cancel.cancelled() => {
                            return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                        }
                        _ = gate.release.notified() => {}
                    }
                }
                // Honor cancellation even in the mock.
                if context.cancel.is_cancelled() {
                    return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                }
                let result = match results {
                    Some(Ok((result, full_output))) => Ok(TaskDispatchSuccess {
                        task_id,
                        result,
                        full_output,
                        suggested_tasks: Vec::new(),
                    }),
                    Some(Err(error)) => Err(TaskDispatchFailure::failed(task_id, error)),
                    // Default: generic success for unscripted tasks.
                    None => Ok(TaskDispatchSuccess {
                        task_id,
                        result: successful_task_result("ok"),
                        full_output: "ok".to_string(),
                        suggested_tasks: Vec::new(),
                    }),
                };
                dispatcher
                    .returned_count
                    .fetch_add(1, std::sync::atomic::Ordering::Release);
                dispatcher.returned.notify_waiters();
                result
            })
        }

        fn integrate(
            &self,
            _store: Arc<TaskRuntimeStore>,
            _blocking: TaskRuntimeBlockingAdapter,
            _run_id: String,
            task: PlanTask,
            _execution_id: String,
            _cancel: CancellationToken,
            _trace_sink: Option<ExecSink>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            Option<
                                crate::tasks::task_runtime::worktree::WorktreeIntegrationOutcome,
                            >,
                            String,
                        >,
                    > + Send,
            >,
        > {
            let error = self
                .integration_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            Box::pin(async move {
                match error {
                    Some(error) => Err(error),
                    None => Ok(None),
                }
            })
        }
    }

    fn successful_task_result(summary: &str) -> SubagentTaskResult {
        SubagentTaskResult {
            contract_version: 1,
            status: SubagentRunStatus::Completed,
            summary: summary.to_string(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        }
    }

    #[test]
    fn execution_check_requires_observed_evidence_and_integrity() -> Result<(), String> {
        assert!(!verification_matches(
            "cargo test --workspace",
            "echo cargo test --workspace"
        ));
        let task = PlanTask {
            id: "contract".to_string(),
            title: "Contract".to_string(),
            execution_checks: vec!["cargo test --workspace".to_string()],
            acceptance_criteria: Vec::new(),
            required_artifacts: vec!["reports/result.json".to_string()],
            ..PlanTask::default()
        };

        // (a) Real failure: remaining_work non-empty.
        let mut result = successful_task_result("work finished");
        result.remaining_work = vec!["write final report".to_string()];
        match assess_task_execution(&task, &result) {
            CompletionAssessment::ExecutionFailed { reason } => {
                assert!(reason.contains("remaining work"), "got {reason:?}");
            }
            other => return Err(format!("expected ExecutionFailed, got {other:?}")),
        }

        // (b) AcceptancePending: completed but verification is Reported only,
        //     and artifact lacks hash/producer. Must NOT be ExecutionFailed
        //     (the Subagent completed) and must NOT pass.
        result.remaining_work.clear();
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "claimed by subagent".to_string(),
            source: SubagentVerificationSource::Reported,
        });
        result.artifacts.push(SubagentArtifactResult {
            path: "reports/result.json".to_string(),
            kind: "report".to_string(),
            bytes: Some(12),
            sha256: None,
            producer_execution_id: None,
            available: true,
        });
        match assess_task_execution(&task, &result) {
            CompletionAssessment::AcceptancePending {
                missing_checks,
                missing_artifacts,
            } => {
                assert!(
                    missing_checks.iter().any(|c| c == "cargo test --workspace"),
                    "got {missing_checks:?}"
                );
                assert!(
                    missing_artifacts.iter().any(|a| a == "reports/result.json"),
                    "got {missing_artifacts:?}"
                );
            }
            other => return Err(format!("expected AcceptancePending, got {other:?}")),
        }

        // (c) Executed: observed pass + integrity metadata present.
        if let Some(verification) = result.verification.first_mut() {
            verification.source = SubagentVerificationSource::Observed;
        }
        if let Some(artifact) = result.artifacts.first_mut() {
            artifact.sha256 = Some("a".repeat(64));
            artifact.producer_execution_id = Some("contract:1".to_string());
        }
        match assess_task_execution(&task, &result) {
            CompletionAssessment::Executed => {}
            other => return Err(format!("expected Executed, got {other:?}")),
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_blocks_completed_result_missing_observed_evidence() -> Result<(), String>
    {
        // M7: a Subagent that returns a text summary but no observed execution
        // evidence for a declared execution_check must NOT be auto-redispatched.
        // The task goes to Blocked and the run to Paused for an explicit retry.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "verify".to_string(),
            title: "Verify".to_string(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".to_string(),
            execution_checks: vec!["cargo test --workspace".to_string()],
            acceptance_criteria: Vec::new(),
            max_retries: 0,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        let mut result = successful_task_result("tests claimed complete");
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "subagent report only".to_string(),
            source: SubagentVerificationSource::Reported,
        });
        dispatcher.respond(&task.id, result);

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher,
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        // Attended run (default) → Blocked + Paused (NOT Failed, NOT auto-retried).
        assert!(
            matches!(outcome, RunOutcome::Paused { .. }),
            "expected Paused, got {outcome:?}"
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "verify")
            .ok_or_else(|| "verify todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Blocked);
        // Plan must still have exactly one task — no fix_task expansion.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(
            plan.tasks.len(),
            1,
            "plan must not expand on acceptance failure"
        );
        assert_eq!(
            plan.tasks
                .first()
                .ok_or_else(|| "plan task missing".to_string())?
                .retry_count,
            0,
            "retry_count must not bump on acceptance failure"
        );
        Ok(())
    }

    #[test]
    fn run_completion_gate_requires_durable_structured_result() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("completed-task");
        let run_id = seed_run(&store, vec![task.clone()])?;
        store
            .set_task_status(
                &run_id,
                &task.id,
                echo_agent::tasks::TaskStatus::Completed,
                Some(&task.agent_role),
                Some("claimed complete"),
            )
            .map_err(|error| error.to_string())?;

        let blockers = run_completion_blockers(&store, &run_id);
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("no structured execution result"))
        );

        store
            .put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task.id.clone(),
                subagent_name: task.agent_role.clone(),
                result: successful_task_result("durable result"),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        assert!(run_completion_blockers(&store, &run_id).is_empty());

        store
            .record_background_cell_started(
                &run_id,
                "cell-running",
                "cargo test --workspace",
                "command-hash",
                Some("turn-1"),
                Some("execution-1"),
                Some("call-1"),
            )
            .map_err(|error| error.to_string())?;
        let blockers = run_completion_blockers(&store, &run_id);
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("cell-running"))
        );
        assert!(
            !store
                .complete_run_if_quiescent(&run_id)
                .map_err(|error| error.to_string())?
        );
        store
            .record_background_cell_finished(
                &run_id,
                "cell-running",
                "cargo test --workspace",
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
                128,
                false,
                Some("128 tests passed"),
                None,
                None,
                Some("call-1"),
            )
            .map_err(|error| error.to_string())?;
        assert!(run_completion_blockers(&store, &run_id).is_empty());
        Ok(())
    }

    /// Helper: a single-task plan (read-only, no review needed) that the
    /// scripted dispatcher can complete.
    fn solo_readonly_task(id: &str) -> PlanTask {
        PlanTask {
            id: id.into(),
            title: id.into(),
            description: "desc".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".into(),
            ..Default::default()
        }
    }

    /// Build a run + plan in the store and return the run id.
    ///
    /// Creates run (Pending), attaches plan (no status change), transitions
    /// Pending → Running so runtime execution can start.
    fn seed_run(store: &Arc<TaskRuntimeStore>, tasks: Vec<PlanTask>) -> Result<String, String> {
        seed_run_with_mode(store, tasks, AttendedMode::Attended)
    }

    fn seed_run_with_mode(
        store: &Arc<TaskRuntimeStore>,
        tasks: Vec<PlanTask>,
        attended_mode: AttendedMode,
    ) -> Result<String, String> {
        let run_id = format!("run_{}", uuid::Uuid::new_v4());
        store
            .create_run(
                &run_id,
                "ws_test",
                "conv_test",
                "msg_test",
                DomainProfile::General,
                "test goal",
                "",
                attended_mode,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: format!("plan_{}", run_id),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("test goal"),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Sequential,
            tasks,
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        Ok(run_id)
    }

    #[tokio::test]
    async fn planned_resume_launcher_rejects_stale_journal_epoch_before_driver_start()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("resume")])?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let snapshot = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "planned resume snapshot missing".to_string())?;
        let expected = TaskRunResumeIdentity::capture(&snapshot);
        store
            .configure_run_continuation(&run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("planned-resume-test")
                .build()
                .map_err(|error| error.to_string())?,
        );

        let error = launch_planned_run_resume(
            store.clone(),
            expected.clone(),
            agent,
            None,
            None,
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .err()
        .ok_or_else(|| "stale planned resume unexpectedly launched".to_string())?;
        assert!(
            error.to_string().contains("identity changed"),
            "stale planned resume failed for the wrong reason: {error}"
        );
        store.wait_for_run_driver_idle(&run_id).await;
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert_eq!(
            store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "planned resume run disappeared".to_string())?
                .status,
            TaskRunStatus::Paused
        );

        store
            .resume_task_run(&run_id)
            .map_err(|error| error.to_string())?;
        let running_event_count = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        let running_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("planned-resume-running-test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let running_error = launch_planned_run_resume(
            store.clone(),
            expected,
            running_agent,
            None,
            None,
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .err()
        .ok_or_else(|| "stale identity unexpectedly relaunched a Running run".to_string())?;
        assert!(running_error.to_string().contains("identity changed"));
        store.wait_for_run_driver_idle(&run_id).await;
        assert_eq!(
            store
                .list_events(&run_id, 0)
                .map_err(|error| error.to_string())?
                .len(),
            running_event_count
        );
        assert_eq!(
            store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Running planned resume run disappeared".to_string())?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn real_dispatcher_executes_frozen_cross_workspace_target_in_leader_run()
    -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        const REMOTE_MARKER: &str = "REMOTE_AGENT_EXECUTED";
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let target = TaskExecutionTarget {
            group_id: "group-alpha".to_string(),
            subagent_role: "verifier".to_string(),
            address: crate::agent_router::AgentAddress::new(
                crate::workspace::WorkspaceId::from_raw("ws_remote".to_string()),
                "conv_remote",
            ),
        };
        let task = PlanTask {
            id: "remote-verification".to_string(),
            title: "Verify remotely".to_string(),
            description: "Return the verification marker".to_string(),
            kind: PlanTaskKind::Verification,
            agent_role: "verifier".to_string(),
            execution_target: Some(target.clone()),
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task])?;

        let remote_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("remote-test")
                .llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_model_name("remote-test")
                        .with_response(REMOTE_MARKER),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let local_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("local-test")
                .llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_model_name("local-test")
                        .with_response("LOCAL_AGENT_MUST_NOT_RUN"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resolver = Arc::new(RecordingExecutionTargetResolver {
            agent: remote_agent,
            calls: StdMutex::new(Vec::new()),
        });
        store.attach_execution_target_resolver(resolver.clone());

        let outcome = execute_runtime_plan(
            store.clone(),
            RealTaskDispatcher {
                primary_agent: local_agent,
                workspace_io: None,
            },
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Completed));

        let calls = resolver
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (leader, acquired_target) = calls
            .first()
            .ok_or_else(|| "cross-workspace resolver was not called".to_string())?;
        assert_eq!(leader.workspace_id.as_str(), "ws_test");
        assert_eq!(leader.conversation_id, "conv_test");
        assert_eq!(acquired_target, &target);
        drop(calls);

        let subagent_runs = store
            .list_subagent_runs(&run_id)
            .map_err(|error| error.to_string())?;
        let subagent_run = subagent_runs
            .first()
            .ok_or_else(|| "leader TaskRun has no SubagentRun".to_string())?;
        assert_eq!(subagent_runs.len(), 1);
        assert_eq!(subagent_run.run_id, run_id);
        assert_eq!(subagent_run.task_id, "remote-verification");
        assert_eq!(subagent_run.status, SubagentRunStatus::Completed);
        let result = subagent_run
            .result
            .as_ref()
            .ok_or_else(|| "SubagentRun result is missing".to_string())?;
        assert!(result.summary.contains(REMOTE_MARKER));
        assert!(!result.summary.contains("LOCAL_AGENT_MUST_NOT_RUN"));
        Ok(())
    }

    #[tokio::test]
    async fn unattended_review_rejections_fail_instead_of_pause() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        for (label, verdict) in [
            (
                "needs-fix",
                r#"{"outcome":"needs_fix","summary":"fix required","failure_fingerprint":"missing-evidence","issues":[]}"#,
            ),
            (
                "blocked",
                r#"{"outcome":"blocked","summary":"evidence unavailable","failure_fingerprint":"blocked","issues":[]}"#,
            ),
        ] {
            let store =
                Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
            let task = PlanTask {
                id: label.to_string(),
                title: label.to_string(),
                description: "review this result".to_string(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "reviewer".to_string(),
                acceptance_criteria: vec!["evidence is complete".to_string()],
                max_retries: 3,
                ..PlanTask::default()
            };
            let run_id = seed_run_with_mode(&store, vec![task.clone()], AttendedMode::Unattended)?;
            let dispatcher = ScriptedDispatcher::new();
            dispatcher.respond(&task.id, successful_task_result("reviewable output"));
            let reviewer = Arc::new(
                MockLlmClient::new()
                    .with_model_name("reviewer-test")
                    .with_response(verdict),
            );

            let outcome = execute_runtime_plan(
                store.clone(),
                dispatcher,
                Some(reviewer),
                &run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
            .map_err(|error| error.to_string())?;

            if !matches!(outcome, RunOutcome::Failed { .. }) {
                return Err(format!(
                    "{label} produced non-terminal outcome: {outcome:?}"
                ));
            }
            let run = store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("run missing for {label}"))?;
            if run.status != TaskRunStatus::Failed {
                return Err(format!("{label} left run in {:?}", run.status));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn review_gate_receives_complete_output_instead_of_bounded_summary() -> Result<(), String>
    {
        use echo_agent::testing::MockLlmClient;

        const FULL_OUTPUT_MARKER: &str = "COMPLETE-OUTPUT-AFTER-SUMMARY-BOUNDARY";
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "full-review".to_string(),
            title: "Review complete analysis".to_string(),
            description: "cover every requested section".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            acceptance_criteria: vec!["the final section is present".to_string()],
            max_retries: 3,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        let full_output = format!("{}\n{FULL_OUTPUT_MARKER}", "analysis ".repeat(180));
        dispatcher.respond_with_output(
            &task.id,
            successful_task_result("bounded parent summary"),
            &full_output,
        );
        let reviewer = Arc::new(MockLlmClient::new().with_response(
            r#"{"outcome":"pass","summary":"complete","failure_fingerprint":null,"issues":[]}"#,
        ));

        let outcome = execute_runtime_plan(
            store,
            dispatcher,
            Some(reviewer.clone()),
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        if !matches!(outcome, RunOutcome::Completed) {
            return Err(format!("reviewed run did not complete: {outcome:?}"));
        }
        let messages = reviewer
            .last_messages()
            .ok_or_else(|| "reviewer received no request".to_string())?;
        let received_full_output = messages.iter().any(|message| {
            message
                .content
                .as_text()
                .is_some_and(|text| text.contains(FULL_OUTPUT_MARKER))
        });
        if !received_full_output {
            return Err("review prompt omitted the complete Subagent output".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_completes_single_task() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("a")])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("a", "reviewed");
        let observed_statuses = Arc::new(std::sync::Mutex::new(Vec::new()));
        let status_store = store.clone();
        let status_run_id = run_id.clone();
        let captured_statuses = observed_statuses.clone();
        let trace_sink: ExecSink = Arc::new(move |event| {
            if event.event == RuntimeEventKind::TaskCompleted
                && let Ok(todos) = status_store.list_todos(&status_run_id)
                && let Some(status) = todos
                    .into_iter()
                    .find(|todo| todo.task_id == "a")
                    .map(|todo| todo.status)
                && let Ok(mut statuses) = captured_statuses.lock()
            {
                statuses.push(status);
            }
        });

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None, // no reviewer LLM → read-only tasks auto-pass review
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            Some(trace_sink),
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let todo = todos.first().ok_or_else(|| "todo a missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert_eq!(
            *observed_statuses
                .lock()
                .map_err(|error| error.to_string())?,
            [TodoStatus::Completed]
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_preserves_completed_tasks_and_finalizes_the_run() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tasks = (0..8)
            .map(|index| solo_readonly_task(&format!("task-{index}")))
            .collect::<Vec<_>>();
        let run_id = seed_run(&store, tasks.clone())?;
        let dispatcher = ScriptedDispatcher::new();
        for task in tasks.iter().take(4) {
            dispatcher.succeed(&task.id, "completed before cancellation");
        }
        let mut cancelled_gates = Vec::new();
        for task in tasks.iter().skip(4) {
            dispatcher.succeed(&task.id, "should be cancelled");
            cancelled_gates.push(dispatcher.gate(&task.id));
        }
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_store = store.clone();
        let run_dispatcher = dispatcher.clone();
        let run_id_for_task = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                run_store,
                run_dispatcher,
                None,
                &run_id_for_task,
                EkoExecutionLimits {
                    max_concurrent_subagents: 8,
                    ..EkoExecutionLimits::default()
                },
                run_cancel,
                None,
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &cancelled_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
        })
        .await
        .map_err(|_| "dispatch/cancellation boundary was not reached".to_string())?;
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime cancellation timed out".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Cancelled));

        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            todos
                .iter()
                .filter(|todo| todo.status == TodoStatus::Completed)
                .count(),
            4
        );
        assert_eq!(
            todos
                .iter()
                .filter(|todo| todo.status == TodoStatus::Cancelled)
                .count(),
            4
        );
        assert!(todos.iter().all(|todo| todo.status != TodoStatus::Running));
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "cancelled run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn mid_wave_pause_preserves_completed_siblings_without_retry() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tasks = (0..8)
            .map(|index| solo_readonly_task(&format!("pause-task-{index}")))
            .collect::<Vec<_>>();
        let run_id = seed_run(&store, tasks.clone())?;
        let dispatcher = ScriptedDispatcher::new();
        for task in tasks.iter().take(4) {
            dispatcher.succeed(&task.id, "completed before pause");
        }
        let mut paused_gates = Vec::new();
        for task in tasks.iter().skip(4) {
            dispatcher.succeed(&task.id, "pending after pause");
            paused_gates.push(dispatcher.gate(&task.id));
        }
        let cancel = CancellationToken::new();
        let execution = {
            let run_store = store.clone();
            let run_dispatcher = dispatcher.clone();
            let run_id = run_id.clone();
            let run_cancel = cancel.clone();
            tokio::spawn(async move {
                execute_runtime_plan(
                    run_store,
                    run_dispatcher,
                    None,
                    &run_id,
                    EkoExecutionLimits {
                        max_concurrent_subagents: 8,
                        ..EkoExecutionLimits::default()
                    },
                    run_cancel,
                    None,
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &paused_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
        })
        .await
        .map_err(|_| "dispatch/pause boundary was not reached".to_string())?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime pause timed out".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Paused { .. }));
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "paused plan missing".to_string())?;
        assert_eq!(
            plan.tasks
                .iter()
                .filter(|task| task.status == echo_agent::tasks::TaskStatus::Completed)
                .count(),
            4
        );
        assert_eq!(
            plan.tasks
                .iter()
                .filter(|task| { matches!(&task.status, echo_agent::tasks::TaskStatus::Paused(_)) })
                .count(),
            4
        );
        assert!(plan.tasks.iter().all(|task| task.retry_count == 0));
        assert!(plan.tasks.iter().all(|task| task.claim.is_none()));
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_reuses_durable_subagent_result_after_restart() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()])?;
        let execution_id = format!("{run_id}:a:1:1");
        store
            .record_subagent_assigned(
                &run_id,
                "a",
                &execution_id,
                "reviewer",
                &task.title,
                1,
                1,
                true,
                true,
            )
            .map_err(|error| error.to_string())?;
        let recovered_result = successful_task_result("recovered summary");
        store
            .record_subagent_released(SubagentReleaseRecord {
                run_id: &run_id,
                task_id: "a",
                execution_id: &execution_id,
                agent_name: "reviewer",
                task_subject: &task.title,
                plan_revision: 1,
                attempt: 1,
                status: "completed",
                result: Some(&recovered_result),
                full_output: Some("recovered full output"),
                usage: None,
                dispatch_hook: true,
            })
            .map_err(|error| error.to_string())?;
        let dispatcher = ScriptedDispatcher::new();

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        assert!(
            dispatcher.order().is_empty(),
            "durable Subagent was dispatched again"
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "a")
            .ok_or_else(|| "todo a missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert_eq!(todo.summary.as_deref(), Some("recovered summary"));
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_respects_dependency_order() -> Result<(), String> {
        // b depends on a → a must be dispatched and completed before b.
        let mut a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let _ = &mut a; // silence unused_mut
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![a.clone(), b.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("a", "done a");
        dispatcher.succeed("b", "done b");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        let order = dispatcher.order();
        // a must appear before b in the dispatch order.
        let pos_a = order
            .iter()
            .position(|x| x == "a")
            .ok_or_else(|| "task a was not dispatched".to_string())?;
        let pos_b = order
            .iter()
            .position(|x| x == "b")
            .ok_or_else(|| "task b was not dispatched".to_string())?;
        assert!(pos_a < pos_b, "dependency violated: b dispatched before a");
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_applies_inserted_revision_after_active_wave() -> Result<(), String> {
        let first = solo_readonly_task("first");
        let mut second = solo_readonly_task("second");
        second.depends_on = vec![first.id.clone()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![first.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("first", "first done");
        dispatcher.succeed("second", "second done");
        let first_gate = dispatcher.gate("first");

        let execution_store = store.clone();
        let execution_dispatcher = dispatcher.clone();
        let execution_run_id = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            first_gate.started.notified(),
        )
        .await
        .map_err(|_| "first task did not enter the active wave".to_string())?;

        store
            .apply_task_patch_for_test(
                &run_id,
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "runtime evidence discovered a required follow-up".to_string(),
                    operations: vec![TaskUpdateOperation::Insert {
                        after_task_id: Some("first".to_string()),
                        task: second.spec(),
                    }],
                },
            )
            .map_err(|error| error.to_string())?;
        first_gate.release.notify_one();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime plan timed out after plan revision".to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Completed));
        assert_eq!(dispatcher.order(), vec!["first", "second"]);
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan disappeared".to_string())?;
        assert_eq!(plan.revision, 2);
        assert!(
            plan.tasks
                .iter()
                .all(|task| task.status == echo_agent::tasks::TaskStatus::Completed)
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_failure_propagates_and_blocks_downstream() -> Result<(), String> {
        // a fails; b depends on a and must be Blocked, run ends Failed
        // (because all non-terminal tasks are Failed/Blocked).
        let a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![a.clone(), b.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.fail("a", "boom");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        match outcome {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id.as_deref(), Some("a"));
            }
            other => return Err(format!("expected Failed, got {other:?}")),
        }
        // b must be Blocked (downstream of failed a).
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let b_todo = todos
            .iter()
            .find(|t| t.task_id == "b")
            .ok_or_else(|| "todo b missing".to_string())?;
        assert_eq!(b_todo.status, TodoStatus::Blocked);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_merge_failure_blocks_downstream() -> Result<(), String> {
        // Use a read-only kind so the review gate auto-passes (no reviewer LLM
        // in this test) and execution reaches integrate_reviewed_task, where
        // the scripted merge failure marks the writer Failed. Downstream is
        // then Blocked by the failed-dependency propagation.
        let writer = solo_readonly_task("writer");
        let mut downstream = solo_readonly_task("downstream");
        downstream.depends_on = vec![writer.id.clone()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![writer.clone(), downstream.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed(&writer.id, "writer completed");
        dispatcher.fail_integration(&writer.id, "synthetic merge conflict");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher,
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        if !matches!(outcome, RunOutcome::Failed { .. }) {
            return Err(format!("expected failed run, got {outcome:?}"));
        }
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let writer_status = todos
            .iter()
            .find(|todo| todo.task_id == "writer")
            .map(|todo| todo.status)
            .ok_or_else(|| "writer todo missing".to_string())?;
        let downstream_status = todos
            .iter()
            .find(|todo| todo.task_id == "downstream")
            .map(|todo| todo.status)
            .ok_or_else(|| "downstream todo missing".to_string())?;
        assert_eq!(writer_status, TodoStatus::Failed);
        assert_eq!(downstream_status, TodoStatus::Blocked);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_cancellation_propagates_to_cancelled_outcome() -> Result<(), String> {
        // Cancel before dispatching; the framework executor observes it at the
        // top of its loop and return Cancelled without running any task.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("a")])?;
        let dispatcher = ScriptedDispatcher::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            cancel,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Cancelled));
        // The Subagent must not have been dispatched.
        assert!(
            dispatcher.order().is_empty(),
            "task ran despite cancellation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_cancellation_preserves_explicit_pause() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()])?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = execute_runtime_plan(
            store,
            ScriptedDispatcher::new(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            cancel,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Paused { .. }));
        Ok(())
    }

    #[test]
    fn invalid_cycle_is_rejected_before_scheduler_dispatch() -> Result<(), String> {
        let mut a = solo_readonly_task("a");
        a.depends_on = vec!["b".into()];
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = format!("run_{}", uuid::Uuid::new_v4());
        store
            .create_run(
                &run_id,
                "ws_test",
                "conv_test",
                "msg_test",
                DomainProfile::General,
                "test goal",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let attach_result = store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("plan_{run_id}"),
            run_id,
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("test goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![a, b],
        });
        let error = match attach_result {
            Ok(()) => return Err("cyclic plan was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, StoreError::InvalidPlan(message) if message.contains("cycle")));
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_does_not_redispatch_in_flight_running_tasks() -> Result<(), String> {
        // Regression: when execution is resumed while an earlier task is still
        // `Running`, a later runtime driver must not dispatch that task again.
        // Without the in_flight
        // guard, the ready filter would re-dispatch the Running task, causing
        // duplicate subagent work. Verify the Running task is left alone, the
        // genuinely-pending sibling is dispatched, and the executor waits for the
        // in_flight task to reach Completed in the store (simulating the
        // sibling instance finishing it) before returning Completed.
        let mut in_flight = solo_readonly_task("in_flight");
        let pending = solo_readonly_task("pending");
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![in_flight.clone(), pending.clone()])?;
        store
            .set_task_status(
                &run_id,
                "in_flight",
                echo_agent::tasks::TaskStatus::Running,
                Some("explorer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        in_flight.status = echo_agent::tasks::TaskStatus::Running;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("pending", "done");
        let pending_gate = dispatcher.gate("pending");

        let execution_store = store.clone();
        let execution_dispatcher = dispatcher.clone();
        let execution_run_id = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            pending_gate.started.notified(),
        )
        .await
        .map_err(|_| "pending task was not dispatched".to_string())?;
        store
            .set_task_status(
                &run_id,
                "in_flight",
                echo_agent::tasks::TaskStatus::Completed,
                Some("explorer"),
                Some("sibling done"),
            )
            .map_err(|error| error.to_string())?;
        pending_gate.release.notify_one();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), execution)
            .await
            .map_err(|_| "runtime plan did not complete within 10s".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;

        // `in_flight` (Running) must NOT have been re-dispatched; only
        // `pending` should appear in the dispatch order.
        let order = dispatcher.order();
        assert!(
            !order.contains(&"in_flight".to_string()),
            "Running task was re-dispatched (regression): {order:?}"
        );
        assert_eq!(order, vec!["pending".to_string()]);
        // The executor waited for the sibling instance to finish `in_flight`, so
        // both tasks are now Completed and the run returns Completed.
        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let in_flight_todo = todos
            .iter()
            .find(|todo| todo.task_id == "in_flight")
            .ok_or_else(|| "in_flight task missing from runtime store".to_string())?;
        assert_eq!(in_flight_todo.status, TodoStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn main_agent_task_streams_tool_events_to_subagent_trace() -> Result<(), String> {
        use crate::agent_handle::AgentHandle;
        use echo_agent::agent::react::builder::ReactAgentBuilder;
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Mutex;

        let llm = MockLlmClient::new()
            .then_tool_call("call_1", "run_code", r#"{"x":6,"y":7}"#)
            .with_response("The result is 42.");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("You are a test assistant.")
            .tool(Box::new(MockTool::new("run_code").with_response("42")))
            .build()
            .map_err(|error| format!("test agent should build: {error}"))?;
        let handle = AgentHandle::new(agent);

        let task = PlanTask {
            id: "implementation-a".into(),
            title: "Run calculation".into(),
            description: "Use the tool and report the result".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            ..Default::default()
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let sink: ExecSink = Arc::new(move |event| {
            if let Ok(mut guard) = captured.lock() {
                guard.push(event);
            }
        });
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| format!("in-memory store should initialize: {error}"))?,
        );
        let run_id = seed_run(&store, vec![task.clone()])?;
        let execution_id = format!("{run_id}:implementation-a:1:1");
        store
            .record_subagent_assigned(
                &run_id,
                &task.id,
                &execution_id,
                &task.agent_role,
                &task.title,
                1,
                1,
                false,
                false,
            )
            .map_err(|error| format!("Subagent boundary should persist: {error}"))?;

        let output = run_main_agent_task(
            &handle,
            TaskRuntimeBlockingAdapter::new(store),
            &run_id,
            &task,
            &execution_id,
            "What is 6 times 7?",
            CancellationToken::new(),
            Some(sink),
            None,
        )
        .await
        .map_err(|error| format!("main agent task should complete: {error}"))?;

        assert!(output.1.contains("42"));
        let events = events
            .lock()
            .map_err(|error| format!("trace events lock poisoned: {error}"))?
            .clone();
        let tool_started_position = events
            .iter()
            .position(|event| event.event == RuntimeEventKind::ToolStarted)
            .ok_or_else(|| "tool_started event was not emitted".to_string())?;
        let tool_completed_position = events
            .iter()
            .position(|event| event.event == RuntimeEventKind::ToolCompleted)
            .ok_or_else(|| "tool_completed event was not emitted".to_string())?;
        assert!(
            tool_started_position < tool_completed_position,
            "tool terminal event overtook its start boundary: {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == RuntimeEventKind::ToolStarted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
                    && event
                        .payload
                        .get("invocation")
                        .and_then(|value| value.get("name"))
                        .and_then(|value| value.as_str())
                        == Some("run_code")
            }),
            "expected tool_started for run_code, got {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == RuntimeEventKind::ToolCompleted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
                    && event
                        .payload
                        .get("result")
                        .and_then(|value| value.get("success"))
                        .and_then(|value| value.as_bool())
                        == Some(true)
                    && event
                        .payload
                        .get("result")
                        .and_then(|value| value.get("output"))
                        .and_then(|value| value.as_str())
                        .is_some_and(|text| text.contains("42"))
            }),
            "expected successful tool_completed with tool output, got {events:?}"
        );
        Ok(())
    }

    // ── M7 acceptance-contract regression tests ────────────────────────────
    //
    // These tests lock in the bug fixes for the contract-validation retry
    // loop: a Subagent that returns a plain-text summary (contract_version=0,
    // no verification array) must complete in exactly one attempt when the
    // task declares no execution_checks, and must never auto-redispatch.

    #[test]
    fn plain_text_summary_passes_when_no_execution_checks() {
        // Mirror of the production bug: 4 analysis tasks returned rich text
        // summaries but contract_version=0 + verification=[]. With no
        // execution_checks declared, this must assess as Executed (the
        // ReviewGate then auto-passes because there are no acceptance_criteria
        // either, so the task reaches Completed without redispatch).
        let task = PlanTask {
            id: "analysis".into(),
            title: "Analyze frontend".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            ..PlanTask::default()
        };
        let result = SubagentTaskResult {
            contract_version: 0, // plain-text fallback, NOT a failure
            status: SubagentRunStatus::Completed,
            summary: "Frontend uses React 19 + Zustand.".into(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        };
        assert!(matches!(
            assess_task_execution(&task, &result),
            CompletionAssessment::Executed
        ));
    }

    #[test]
    fn contract_version_zero_is_not_an_execution_failure() -> Result<(), String> {
        let task = PlanTask {
            id: "t".into(),
            execution_checks: vec!["cargo test".into()],
            acceptance_criteria: Vec::new(),
            ..PlanTask::default()
        };
        let result = SubagentTaskResult {
            contract_version: 0,
            status: SubagentRunStatus::Completed,
            summary: "done".into(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            verification: Vec::new(), // no observed pass for "cargo test"
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        };
        // cv=0 itself is fine; what blocks is the missing observed check.
        // Crucially this is AcceptancePending, NOT ExecutionFailed — the
        // Subagent completed, so auto-retry would just reproduce the gap.
        match assess_task_execution(&task, &result) {
            CompletionAssessment::AcceptancePending { missing_checks, .. } => {
                assert_eq!(missing_checks, vec!["cargo test".to_string()]);
            }
            other => return Err(format!("expected AcceptancePending, got {other:?}")),
        }
        Ok(())
    }

    #[tokio::test]
    async fn completed_subagent_with_text_summary_runs_single_attempt() -> Result<(), String> {
        // Reproduction of the original loop scenario: a task with semantic
        // acceptance only (no execution_checks) must dispatch exactly once.
        // Before the fix this looped up to max_retries because cv=0 was
        // treated as a retryable contract failure.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let task = PlanTask {
            id: "analyze".into(),
            title: "Analyze backend".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "explorer".into(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            max_retries: 3,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        // Plain text result, no JSON contract — exactly what the production
        // Subagents returned.
        dispatcher.respond(
            &task.id,
            SubagentTaskResult {
                contract_version: 0,
                status: SubagentRunStatus::Completed,
                summary: "Backend has 4 modules.".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: Vec::new(),
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        assert!(
            matches!(outcome, RunOutcome::Completed),
            "expected Completed, got {outcome:?}"
        );
        // No reviewer LLM, but no acceptance_criteria either → auto-pass.
        // Exactly one dispatch happened.
        assert_eq!(dispatcher.order().len(), 1, "expected single dispatch");
        // Plan still has 1 task, retry_count 0.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        let task = plan
            .tasks
            .first()
            .ok_or_else(|| "plan task missing".to_string())?;
        assert_eq!(task.retry_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn real_execution_failure_retries_within_budget() -> Result<(), String> {
        // Sanity check that the ExecutionFailed path still auto-retries.
        // A Subagent returning remaining_work is a real failure (not a
        // contract-format issue) and should retry up to max_retries.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let task = PlanTask {
            id: "flaky".into(),
            title: "Flaky".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "explorer".into(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            max_retries: 2,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        // Always report remaining_work → ExecutionFailed every time.
        dispatcher.respond(
            &task.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "partial".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: Vec::new(),
                remaining_work: vec!["not done".into()],
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Exhausted budget → Failed.
        assert!(
            matches!(outcome, RunOutcome::Failed { .. }),
            "expected Failed after retries, got {outcome:?}"
        );
        // Initial attempt + 2 retries = 3 dispatches.
        assert_eq!(
            dispatcher.order().len(),
            3,
            "expected 3 dispatches (1 + 2 retries)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn wave_processes_all_results_when_one_task_blocks() -> Result<(), String> {
        // Regression: when one task in a parallel wave resolves to Blocked
        // (acceptance pending), sibling tasks that completed in the SAME wave
        // must still be marked Completed and persisted. The early-return bug
        // left siblings in Running, the resume path reset them to Pending,
        // and the next attempt redispatched the entire wave — duplicating
        // already-finished Subagent work.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let clean = solo_readonly_task("clean"); // no execution_checks → Executed
        let mut blocked = solo_readonly_task("blocked");
        blocked.execution_checks = vec!["cargo test".to_string()];
        blocked.acceptance_criteria = Vec::new();
        let run_id = seed_run(&store, vec![clean.clone(), blocked.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.respond(
            &clean.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "clean run".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: Vec::new(),
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );
        dispatcher.respond(
            &blocked.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "blocked run".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: Vec::new(), // execution_check has no observed pass
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Run Paused (acceptance failure on attended run).
        assert!(
            matches!(outcome, RunOutcome::Paused { .. }),
            "expected Paused, got {outcome:?}"
        );
        // CRITICAL: the clean task must be Completed, not Running/Pending.
        let todos = store.list_todos(&run_id).map_err(|e| e.to_string())?;
        let clean_status = todos
            .iter()
            .find(|t| t.task_id == "clean")
            .map(|t| t.status)
            .ok_or_else(|| "clean todo missing".to_string())?;
        assert_eq!(
            clean_status,
            TodoStatus::Completed,
            "sibling completed task must persist as Completed, got {clean_status:?}"
        );
        // Blocked task is Blocked.
        let blocked_status = todos
            .iter()
            .find(|t| t.task_id == "blocked")
            .map(|t| t.status)
            .ok_or_else(|| "blocked todo missing".to_string())?;
        assert_eq!(blocked_status, TodoStatus::Blocked);
        // Plan size unchanged.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(plan.tasks.len(), 2);
        // Exactly one dispatch per task — no redispatch.
        assert_eq!(
            dispatcher.order().len(),
            2,
            "each task dispatched exactly once"
        );
        Ok(())
    }
}
