#[cfg(test)]
#[allow(clippy::items_after_test_module)] // usage-record impls below are production code kept here for locality with their tests; reordering is pure churn
mod tests {
    use super::*;

    struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl RunDriverExecutionReceipt for DropFlag {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                drop(self);
            })
        }
    }

    struct ReleaseOrder(
        std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
        &'static str,
    );

    impl RunDriverExecutionReceipt for ReleaseOrder {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                if let Ok(mut order) = self.0.lock() {
                    order.push(self.1);
                }
            })
        }
    }

    fn fresh() -> Result<TaskRuntimeStore, StoreError> {
        TaskRuntimeStore::new_in_memory()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))
    }

    #[test]
    fn bounded_agent_queries_scan_growth_without_unbounded_results() -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "bounded-run",
            "global",
            "conversation",
            "message",
            DomainProfile::General,
            "goal",
            "task",
            AttendedMode::Attended,
        )?;
        let mut events = (0..600)
            .map(|index| {
                RuntimeJournalEvent::for_append(
                    "bounded-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({ "message": format!("before-{index}") }),
                )
            })
            .collect::<Vec<_>>();
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-z"),
            Some("execution-z"),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": "execution-z",
                "agent_name": "reviewer",
                "plan_revision": 7,
                "attempt": 2,
            }),
        ));
        events.extend((0..400).map(|index| {
            RuntimeJournalEvent::for_append(
                "bounded-run",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "message": format!("middle-{index}") }),
            )
        }));
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-a"),
            Some("execution-a"),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": "execution-a",
                "agent_name": "implementer",
                "plan_revision": 9,
                "attempt": 3,
            }),
        ));
        events.extend((0..300).map(|index| {
            RuntimeJournalEvent::for_append(
                "bounded-run",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "message": format!("after-{index}") }),
            )
        }));
        let result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "完成 ✅ bounded result",
            Vec::new(),
        );
        let usage = ExecutionUsage {
            duration_ms: Some(41),
            tokens_used: Some(73),
            iterations: Some(5),
        };
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-a"),
            Some("execution-a"),
            RuntimeEventKind::SubagentReleased,
            serde_json::json!({
                "status": "completed",
                "outcome": result,
                "usage": usage,
            }),
        ));
        store.commit_runtime_events("bounded-run", events)?;

        let released = store.query_events_bounded(
            "bounded-run",
            RuntimeEventQuery::new(0, 1)
                .for_execution("execution-a")
                .with_event_types(vec![RuntimeEventKind::SubagentReleased]),
        )?;
        assert_eq!(released.len(), 1);
        assert_eq!(
            released.first().map(|event| event.event_type),
            Some(RuntimeEventKind::SubagentReleased)
        );

        let snapshots = store.list_subagent_run_snapshots("bounded-run", 1)?;
        let snapshot = snapshots
            .first()
            .ok_or_else(|| StoreError::InvalidPlan("bounded snapshot missing".to_string()))?;
        assert_eq!(snapshot.run.subagent_run_id, "execution-a");
        assert_eq!(snapshot.plan_revision, Some(9));
        assert_eq!(snapshot.run.attempt, 3);
        assert_eq!(snapshot.run.usage.tokens_used, Some(73));
        assert_eq!(snapshot.run.usage.iterations, Some(5));
        assert_eq!(
            snapshot
                .run
                .outcome
                .as_ref()
                .map(|value| value.summary.as_str()),
            Some("完成 ✅ bounded result")
        );
        assert_eq!(
            snapshot.latest_event.event_type,
            RuntimeEventKind::SubagentReleased
        );

        let exact = store
            .get_subagent_run_snapshot("bounded-run", "execution-z")?
            .ok_or_else(|| StoreError::InvalidPlan("exact snapshot missing".to_string()))?;
        assert_eq!(exact.run.task_id, "task-z");
        assert_eq!(exact.plan_revision, Some(7));
        let encoded = serde_json::to_value(&exact.run)?;
        let decoded = serde_json::from_value::<SubagentRun>(encoded.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, encoded);
        Ok(())
    }

    #[test]
    fn public_run_state_and_cell_queries_do_not_request_full_journal_replay()
    -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "checkpoint-read-run",
            "global",
            "conversation",
            "message",
            DomainProfile::General,
            "read checkpointed state",
            "task",
            AttendedMode::Attended,
        )?;
        store.record_background_cell_started(
            "checkpoint-read-run",
            "cell-1",
            "probe",
            "sha256:probe",
            Some("turn-1"),
            None,
            None,
        )?;
        store
            .shadow
            .reset_full_replay_requests_for_test("checkpoint-read-run")?;

        assert!(store.get_run_state("checkpoint-read-run")?.is_some());
        assert_eq!(store.list_background_cells("checkpoint-read-run")?.len(), 1);
        assert_eq!(
            store
                .shadow
                .full_replay_requests_for_test("checkpoint-read-run")?,
            0
        );

        assert!(!store.list_events("checkpoint-read-run", 0)?.is_empty());
        assert_eq!(
            store
                .shadow
                .full_replay_requests_for_test("checkpoint-read-run")?,
            1,
            "the replay probe did not observe an explicit sequence-zero query"
        );
        Ok(())
    }

    #[test]
    fn create_run_rejects_same_id_with_different_identity() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "same-run",
                "global",
                "conversation-a",
                "root-a",
                DomainProfile::General,
                "goal-a",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let duplicate = store.create_run(
            "same-run",
            "global",
            "conversation-b",
            "root-b",
            DomainProfile::General,
            "goal-b",
            "task",
            AttendedMode::Attended,
        );
        assert!(duplicate.is_err_and(|error| error.to_string().contains("different immutable")));
        Ok(())
    }

    #[test]
    fn create_run_retry_remains_idempotent_after_goal_update() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "goal-run",
                "global",
                "conversation",
                "root",
                DomainProfile::General,
                "goal A",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        store
            .update_run_goal(
                "goal-run",
                1,
                "goal B",
                "user refined goal",
                RunGoalActorSource::Cli,
            )
            .map_err(|error| error.to_string())?;

        let existing = store
            .create_run(
                "goal-run",
                "global",
                "conversation",
                "root",
                DomainProfile::General,
                "goal A",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(existing.goal, "goal B");
        assert_eq!(existing.goal_revision, 2);
        Ok(())
    }

    fn last_frame_event_types(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<Vec<String>, String> {
        let contents =
            std::fs::read_to_string(store.active_shadow_root().join(run_id).join("events.jsonl"))
                .map_err(|error| error.to_string())?;
        let line = contents
            .lines()
            .last()
            .ok_or_else(|| "TaskRuntime journal has no frame".to_string())?;
        let frame: serde_json::Value =
            serde_json::from_str(line).map_err(|error| error.to_string())?;
        let batch_id = frame
            .get("batch_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "TaskRuntime journal frame has no batch id".to_string())?;
        let first_sequence = frame
            .get("first_sequence")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "TaskRuntime journal frame has no first sequence".to_string())?;
        let records = frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "TaskRuntime journal frame has no records".to_string())?;
        for (index, record) in records.iter().enumerate() {
            let expected = first_sequence
                .checked_add(u64::try_from(index).map_err(|error| error.to_string())?)
                .ok_or_else(|| "TaskRuntime test sequence overflow".to_string())?;
            if record.get("batch_id").and_then(serde_json::Value::as_str) != Some(batch_id)
                || record.get("sequence").and_then(serde_json::Value::as_u64) != Some(expected)
            {
                return Err("TaskRuntime batch record identity is not contiguous".to_string());
            }
        }
        records
            .iter()
            .map(|record| {
                record
                    .get("event")
                    .and_then(|event| event.get("event_type"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "TaskRuntime record has no event type".to_string())
            })
            .collect()
    }

    fn seed_public_state_fixture(
        event_count: usize,
    ) -> Result<(tempfile::TempDir, TaskRuntimeStore, String), String> {
        if event_count == 0 {
            return Err("performance fixture requires at least one event".to_string());
        }
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("public-state-{event_count}");
        let mut pending = Vec::with_capacity(512);
        for index in 0..event_count {
            let (task_id, step_id, event_type, payload) = match index {
                0 => (
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({
                        "goal": "public state performance",
                        "goal_revision": 1,
                        "goal_sha256": task_goal_sha256("public state performance"),
                        "domain_profile": "general",
                        "workspace_id": "test",
                        "conversation_id": "ordinary-conversation",
                        "root_message_id": "root-message",
                        "route": "task",
                        "attended_mode": "unattended",
                    }),
                ),
                1 => (
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({"enabled": true}),
                ),
                2 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnStarted,
                    serde_json::json!({
                        "turn_id": "fixture-turn",
                        "ordinal": 1,
                        "origin": "continuation",
                        "transcript_visibility": "internal",
                    }),
                ),
                3 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": "fixture-usage",
                        "turn_id": "fixture-turn",
                        "input_tokens": 2,
                        "output_tokens": 3,
                        "elapsed_seconds": 1,
                    }),
                ),
                4 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnCompacted,
                    serde_json::json!({
                        "event_id": "fixture-compaction",
                        "turn_id": "fixture-turn",
                    }),
                ),
                5 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "turn_id": "fixture-turn",
                        "status": "ended",
                        "elapsed_seconds": 1,
                        "made_progress": true,
                    }),
                ),
                6 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-execution-a".to_string()),
                    RuntimeEventKind::SubagentAssigned,
                    serde_json::json!({"replay_safe": false}),
                ),
                7 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "execution_id": "fixture-execution-a",
                        "call_id": "fixture-call-a",
                        "tool_name": "write_file",
                        "replay_safe": false,
                    }),
                ),
                8 => (
                    Some("fixture-task-a".to_string()),
                    None,
                    RuntimeEventKind::RecoveryBlocked,
                    serde_json::json!({
                        "execution_id": "fixture-execution-a",
                        "call_id": "fixture-call-a",
                        "tool_name": "write_file",
                        "reason": "fixture uncertain side effect",
                    }),
                ),
                9 => (
                    None,
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::BackgroundCellStarted,
                    serde_json::json!({
                        "cell_id": "fixture-cell",
                        "name": "fixture command",
                        "command_hash": "fixture-hash",
                        "phase": "running",
                        "artifact_status": "not_requested",
                    }),
                ),
                10 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::ToolCompleted,
                    serde_json::json!({"call_id": "fixture-call-a"}),
                ),
                11 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-execution-a".to_string()),
                    RuntimeEventKind::SubagentReleased,
                    serde_json::json!({}),
                ),
                12 => (
                    Some("fixture-task-a".to_string()),
                    None,
                    RuntimeEventKind::RecoveryResolved,
                    serde_json::json!({}),
                ),
                13 => (
                    None,
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::BackgroundCellFinished,
                    serde_json::json!({
                        "cell_id": "fixture-cell",
                        "phase": "succeeded",
                        "terminal_cause": "exited",
                        "exit_code": 0,
                        "artifact_status": "not_requested",
                    }),
                ),
                14 => (
                    Some("fixture-task-b".to_string()),
                    Some("fixture-execution-b".to_string()),
                    RuntimeEventKind::SubagentAssigned,
                    serde_json::json!({"replay_safe": false}),
                ),
                15 => (
                    Some("fixture-task-b".to_string()),
                    Some("fixture-call-b".to_string()),
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "execution_id": "fixture-execution-b",
                        "call_id": "fixture-call-b",
                        "tool_name": "shell",
                        "replay_safe": false,
                    }),
                ),
                16 => (
                    Some("fixture-task-b".to_string()),
                    None,
                    RuntimeEventKind::RecoveryBlocked,
                    serde_json::json!({
                        "execution_id": "fixture-execution-b",
                        "call_id": "fixture-call-b",
                        "tool_name": "shell",
                        "reason": "fixture active recovery blocker",
                    }),
                ),
                _ => (
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "kind": "performance_fixture",
                        "ordinal": index,
                        "detail": "fixed diagnostic payload for public checkpoint-backed state reads",
                    }),
                ),
            };
            pending.push(RuntimeJournalEvent::for_append(
                &run_id,
                task_id.as_deref(),
                step_id.as_deref(),
                event_type,
                payload,
            ));
            if pending.len() == 512 || index.saturating_add(1) == event_count {
                store
                    .shadow
                    .append_event_batch(&run_id, std::mem::take(&mut pending))
                    .map_err(|error| error.to_string())?;
            }
        }
        store
            .shadow
            .rewrite_plan(&run_id)
            .map_err(|error| error.to_string())?;
        Ok((temp, store, run_id))
    }

    fn median_duration(samples: &mut [std::time::Duration]) -> Option<std::time::Duration> {
        samples.sort_unstable();
        samples.get(samples.len() / 2).copied()
    }

    fn seed_public_query_fixture(
        event_count: usize,
    ) -> Result<(tempfile::TempDir, TaskRuntimeStore, String), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("public-query-{event_count}");
        store
            .create_run(
                &run_id,
                "test",
                "conversation",
                "root-message",
                DomainProfile::General,
                "bounded public query",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: format!("plan-{event_count}"),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("bounded public query"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "task-a".to_string(),
                title: "Project bounded state".to_string(),
                description: "Exercise the production Todo and completion queries".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "subagent".to_string(),
                domain_profile: DomainProfile::General,
                sort_order: 1,
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                &run_id,
                "task-a",
                echo_agent::tasks::TaskStatus::Completed,
                Some("subagent"),
                Some("projected"),
            )
            .map_err(|error| error.to_string())?;
        store
            .put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: "task-a".to_string(),
                subagent_name: "subagent".to_string(),
                outcome: SubagentOutcome::terminal(
                    SubagentStatus::Completed,
                    "bounded query complete",
                    Vec::new(),
                ),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        store
            .add_artifact(&Artifact {
                id: "artifact-a".to_string(),
                run_id: run_id.clone(),
                task_id: Some("task-a".to_string()),
                kind: ArtifactKind::Report,
                title: "Bounded report".to_string(),
                path: None,
                metadata: serde_json::json!({"source": "fixture"}),
            })
            .map_err(|error| error.to_string())?;
        store
            .add_review(&ReviewResult {
                id: "review-a".to_string(),
                run_id: run_id.clone(),
                task_id: "task-a".to_string(),
                reviewer_agent: "reviewer".to_string(),
                outcome: ReviewOutcome::Pass,
                issues: Vec::new(),
                failure_fingerprint: None,
                created_fix_task_id: None,
                created_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        let current = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        if current > event_count {
            return Err(format!(
                "query fixture needs {current} semantic events, target was {event_count}"
            ));
        }
        let mut pending = Vec::with_capacity(512);
        for ordinal in current..event_count {
            pending.push(RuntimeJournalEvent::for_append(
                &run_id,
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"kind": "bounded_query_fixture", "ordinal": ordinal}),
            ));
            if pending.len() == 512 || ordinal.saturating_add(1) == event_count {
                store
                    .shadow
                    .append_event_batch(&run_id, std::mem::take(&mut pending))
                    .map_err(|error| error.to_string())?;
            }
        }
        store
            .shadow
            .rewrite_plan(&run_id)
            .map_err(|error| error.to_string())?;
        Ok((temp, store, run_id))
    }

    fn seed_history_plan(store: &TaskRuntimeStore, run_id: &str) -> Result<(), String> {
        store
            .create_run(
                run_id,
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "history projection",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("history projection"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: ["other-task", "target-task"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, task_id)| PlanTask {
                        id: task_id.to_string(),
                        title: task_id.to_string(),
                        description: "history scale".to_string(),
                        kind: PlanTaskKind::Investigation,
                        agent_role: "subagent".to_string(),
                        domain_profile: DomainProfile::General,
                        sort_order: i64::try_from(index).unwrap_or_default(),
                        ..Default::default()
                    })
                    .collect(),
            })
            .map_err(|error| error.to_string())
    }

    fn history_artifact(run_id: &str, id: &str) -> Artifact {
        Artifact {
            id: id.to_string(),
            run_id: run_id.to_string(),
            task_id: Some("target-task".to_string()),
            kind: ArtifactKind::Report,
            title: id.to_string(),
            path: None,
            metadata: serde_json::json!({"fixture": true}),
        }
    }

    fn history_review(run_id: &str, task_id: &str, id: &str) -> ReviewResult {
        ReviewResult {
            id: id.to_string(),
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::Pass,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: Utc::now(),
        }
    }

    fn artifact_history_event(run_id: &str, id: &str) -> RuntimeJournalEvent {
        let artifact = history_artifact(run_id, id);
        RuntimeJournalEvent::for_append(
            run_id,
            artifact.task_id.as_deref(),
            None,
            RuntimeEventKind::ArtifactProduced,
            serde_json::json!({
                "artifact_id": artifact.id,
                "kind": artifact.kind.as_str(),
                "title": artifact.title,
                "task_id": artifact.task_id,
                "path": artifact.path,
                "metadata": artifact.metadata,
            }),
        )
    }

    fn review_history_event(run_id: &str, task_id: &str, id: &str) -> RuntimeJournalEvent {
        review_runtime_event(&history_review(run_id, task_id, id), None)
    }

    fn line_count(path: &std::path::Path) -> Result<usize, String> {
        Ok(std::fs::read(path)
            .map_err(|error| error.to_string())?
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count())
    }

    fn retain_first_jsonl_record(path: &std::path::Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let first = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .ok_or_else(|| "history segment has no first record".to_string())?;
        std::fs::write(path, first).map_err(|error| error.to_string())
    }

    fn history_cursor_sequence(path: &std::path::Path) -> Result<u64, String> {
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
        .get("through_sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "history cursor has no through_sequence".to_string())
    }

    fn flush_history_batch(
        store: &TaskRuntimeStore,
        run_id: &str,
        pending: &mut Vec<RuntimeJournalEvent>,
        samples: &mut Vec<std::time::Duration>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        store
            .commit_runtime_events(run_id, std::mem::take(pending))
            .map_err(|error| error.to_string())?;
        samples.push(started.elapsed());
        Ok(())
    }

    type HistoryScaleFixture = (
        tempfile::TempDir,
        TaskRuntimeStore,
        String,
        std::time::Duration,
        std::time::Duration,
    );

    fn seed_history_scale_fixture(review_count: usize) -> Result<HistoryScaleFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("history-scale-{review_count}");
        seed_history_plan(&store, &run_id)?;

        let split = review_count / 2;
        let mut pending = vec![
            artifact_history_event(&run_id, "only-artifact"),
            review_history_event(&run_id, "target-task", "only-target-review"),
        ];
        let mut first_half = Vec::new();
        let mut second_half = Vec::new();
        for ordinal in 0..review_count {
            pending.push(review_history_event(
                &run_id,
                "other-task",
                &format!("other-review-{ordinal}"),
            ));
            if pending.len() == 512 {
                let samples = if ordinal < split {
                    &mut first_half
                } else {
                    &mut second_half
                };
                flush_history_batch(&store, &run_id, &mut pending, samples)?;
            }
        }
        flush_history_batch(&store, &run_id, &mut pending, &mut second_half)?;
        let first_median = median_duration(&mut first_half)
            .ok_or_else(|| "history first-half append samples are empty".to_string())?;
        let second_median = median_duration(&mut second_half)
            .ok_or_else(|| "history second-half append samples are empty".to_string())?;
        Ok((temp, store, run_id, first_median, second_median))
    }

    type ArtifactScaleFixture = (
        tempfile::TempDir,
        TaskRuntimeStore,
        String,
        std::time::Duration,
        std::time::Duration,
        usize,
        u64,
    );

    fn seed_artifact_scale_fixture(artifact_count: usize) -> Result<ArtifactScaleFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("artifact-scale-{artifact_count}");
        seed_history_plan(&store, &run_id)?;
        let (scans_before, bytes_before) = store
            .shadow
            .history_stats_for_test(&run_id)
            .map_err(|error| error.to_string())?;
        let split = artifact_count / 2;
        let mut pending = Vec::with_capacity(512);
        let mut first_half = Vec::new();
        let mut second_half = Vec::new();
        for ordinal in 0..artifact_count {
            pending.push(artifact_history_event(
                &run_id,
                &format!("artifact-{ordinal}"),
            ));
            if pending.len() == 512 {
                let samples = if ordinal < split {
                    &mut first_half
                } else {
                    &mut second_half
                };
                flush_history_batch(&store, &run_id, &mut pending, samples)?;
            }
        }
        flush_history_batch(&store, &run_id, &mut pending, &mut second_half)?;
        let first_median = median_duration(&mut first_half)
            .ok_or_else(|| "artifact first-half append samples are empty".to_string())?;
        let second_median = median_duration(&mut second_half)
            .ok_or_else(|| "artifact second-half append samples are empty".to_string())?;
        let (scans_after, bytes_after) = store
            .shadow
            .history_stats_for_test(&run_id)
            .map_err(|error| error.to_string())?;
        let scan_delta = scans_after.saturating_sub(scans_before);
        let byte_delta = bytes_after.saturating_sub(bytes_before);
        Ok((
            temp,
            store,
            run_id,
            first_median,
            second_median,
            scan_delta,
            byte_delta,
        ))
    }

    fn time_history_target_queries(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<std::time::Duration, String> {
        let started = std::time::Instant::now();
        let artifacts = store
            .list_artifacts(run_id)
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews(run_id, "target-task")
            .map_err(|error| error.to_string())?;
        if artifacts.len() != 1 || reviews.len() != 1 {
            return Err("targeted history projection returned incomplete facts".to_string());
        }
        Ok(started.elapsed())
    }

    fn time_public_queries(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<std::time::Duration, String> {
        let started = std::time::Instant::now();
        let todos = store
            .list_todos(run_id)
            .map_err(|error| error.to_string())?;
        let artifacts = store
            .list_artifacts(run_id)
            .map_err(|error| error.to_string())?;
        let completion = store
            .completion_gate_report(run_id)
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews(run_id, "task-a")
            .map_err(|error| error.to_string())?;
        let summary = store
            .get_summary(run_id, "task-a")
            .map_err(|error| error.to_string())?;
        if todos.len() != 1
            || artifacts.len() != 1
            || reviews.len() != 1
            || summary.is_none()
            || !completion.ready
        {
            return Err("production TaskRuntime query projection is incomplete".to_string());
        }
        Ok(started.elapsed())
    }

    fn boot_recovery_event_count(store: &TaskRuntimeStore) -> Result<usize, StoreError> {
        Ok(store
            .list_events("r1", 0)?
            .iter()
            .filter(|event| boot_recovery_payload(event).is_some())
            .count())
    }

    fn create_paused_run(store: &TaskRuntimeStore, run_id: &str) -> Result<TaskRun, StoreError> {
        store.create_run(
            run_id,
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "original goal",
            "",
            AttendedMode::Attended,
        )?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.transition_run(run_id, TaskRunStatus::Paused)
    }

    fn test_driver_admission(
        store: &std::sync::Arc<TaskRuntimeStore>,
        run_id: &str,
    ) -> Result<RunDriverAdmissionReservation, String> {
        store
            .reserve_run_driver_admission(
                run_id.to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .map_err(|error| error.to_string())
    }

    fn prepare_retryable_run(
        store: &TaskRuntimeStore,
        run_id: &str,
        task_id: &str,
    ) -> Result<(), StoreError> {
        store.create_run(
            run_id,
            "workspace-a",
            "conversation",
            "message",
            DomainProfile::General,
            "retry through the TUI facade",
            "",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("{run_id}-plan"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("retry through the TUI facade"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: task_id.to_string(),
                title: "Retry task".to_string(),
                max_retries: 2,
                ..PlanTask::default()
            }],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.set_task_status(
            run_id,
            task_id,
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            None,
            Some("acceptance failed"),
        )?;
        store.transition_run(run_id, TaskRunStatus::Failed)?;
        Ok(())
    }

    fn retry_state_snapshot(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<(serde_json::Value, serde_json::Value, serde_json::Value), String> {
        Ok((
            serde_json::to_value(
                store
                    .list_events(run_id, 0)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
            serde_json::to_value(store.get_run(run_id).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            serde_json::to_value(store.get_plan(run_id).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
        ))
    }

    #[test]
    fn tui_retry_registration_failure_leaves_events_run_and_plan_unchanged() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_retryable_run(&store, "registration-failure", "retry-task")
            .map_err(|error| error.to_string())?;
        let before = retry_state_snapshot(&store, "registration-failure")?;
        store.fail_next_run_driver_registration_for_test();

        let error = store
            .spawn_supervised_task_retry(
                "registration-failure".to_string(),
                "retry-task".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                |(), _receipt_owner| async { Ok(()) },
            )
            .err()
            .ok_or_else(|| "injected driver registration unexpectedly succeeded".to_string())?;
        assert!(
            error
                .to_string()
                .contains("injected TaskRun driver registration failure")
        );
        assert_eq!(
            before,
            retry_state_snapshot(&store, "registration-failure")?
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tui_retry_registration_pins_generation_before_recovery_classification()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_retryable_run(&store, "generation-race", "retry-task")
            .map_err(|error| error.to_string())?;
        let before = retry_state_snapshot(&store, "generation-race")?;
        let (registered, release) = store.park_next_run_driver_registration_for_test()?;
        let retry_store = std::sync::Arc::clone(&store);
        let retry = tokio::spawn(async move {
            retry_store.spawn_supervised_task_retry(
                "generation-race".to_string(),
                "retry-task".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                |(), _receipt_owner| async { Ok(()) },
            )
        });
        tokio::task::spawn_blocking(move || {
            registered
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| format!("retry registration was not parked: {error}"))
        })
        .await
        .map_err(|error| error.to_string())??;

        let transition_error = store
            .begin_workspace_transition()
            .await
            .err()
            .ok_or_else(|| "workspace transition overtook registered TUI retry".to_string())?;
        assert!(matches!(
            transition_error,
            StoreError::WorkspaceTransitionBusy { .. }
        ));
        assert_eq!(before, retry_state_snapshot(&store, "generation-race")?);

        release
            .send(())
            .map_err(|_| "retry registration release receiver closed".to_string())?;
        let (preparation, waiter) = retry
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(
            preparation,
            TaskRetryPreparation::Acceptance { next_attempt: 1 }
        );
        let _driver_result = waiter.await.map_err(|error| error.to_string())?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_write_debt_retains_execution_receipt_until_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "receipt-debt",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "retain execution receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("receipt-debt", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_driver = std::sync::Arc::clone(&dropped);
        let admission = test_driver_admission(&store, "receipt-debt")?;
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(dropped_for_driver));
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let driver_error = waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "driver failure was not reported".to_string())?;
        assert!(driver_error.contains("terminal settlement failed"));
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(store.begin_workspace_transition().await.is_err());

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        store
            .retry_run_settlement_debts()
            .await
            .map_err(|error| error.to_string())?;
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        let transition = store
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        drop(transition);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn dropped_waiter_shutdown_settles_run_and_releases_execution_receipt()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "dropped-waiter",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "settle dropped waiter",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("dropped-waiter", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let cancel = echo_agent::agent::CancellationToken::new();
        let driver_cancel = cancel.clone();
        let admission = store
            .reserve_run_driver_admission("dropped-waiter".to_string(), cancel)
            .map_err(|error| error.to_string())?;
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_driver = std::sync::Arc::clone(&dropped);
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(dropped_for_driver));
                    driver_cancel.cancelled().await;
                    Err::<(), String>("driver cancelled during shutdown".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        drop(waiter);

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.shutdown_run_drivers(),
        )
        .await
        .map_err(|_| "TaskRun driver shutdown timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run("dropped-waiter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "settled run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Cancelled);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_reporter_failure_is_published_once_to_all_waiters() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store.abort_next_run_driver_shutdown_reporter_for_test();

        let first_store = std::sync::Arc::clone(&store);
        let second_store = std::sync::Arc::clone(&store);
        let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            tokio::join!(
                first_store.shutdown_run_drivers(),
                second_store.shutdown_run_drivers()
            )
        })
        .await
        .map_err(|_| "concurrent TaskRun shutdown waiters timed out".to_string())?;
        let first = first.err().ok_or_else(|| {
            "first shutdown waiter did not observe the reporter failure".to_string()
        })?;
        let second = second.err().ok_or_else(|| {
            "second shutdown waiter did not observe the reporter failure".to_string()
        })?;
        assert_eq!(first, second);
        assert!(
            first
                .driver_errors
                .iter()
                .any(|error| error.contains("shutdown reporter failed"))
        );

        let repeated = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.shutdown_run_drivers(),
        )
        .await
        .map_err(|_| "repeated TaskRun shutdown waiter timed out".to_string())?
        .err()
        .ok_or_else(|| "repeated shutdown waiter lost the reporter failure".to_string())?;
        assert_eq!(first, repeated);
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_parked_prepare_and_reports_its_permanent_debt() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        let canonical_root = std::fs::canonicalize(temp.path())
            .map_err(|error| error.to_string())?
            .join("tasks");
        store
            .create_run(
                "parked-prepare",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "settle an accepted preparation",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "parked-prepare-plan".to_string(),
                run_id: "parked-prepare".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("settle an accepted preparation"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "parked-prepare-task".to_string(),
                    title: "Settle preparation".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("parked-prepare", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("parked-prepare", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let (prepare_started_tx, prepare_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (continue_prepare_tx, continue_prepare_rx) = std::sync::mpsc::channel::<()>();
        let preparation_store = std::sync::Arc::clone(&store);
        let run_store = std::sync::Arc::clone(&store);
        let root_for_driver = root.clone();
        let blocked_root_for_driver = blocked_root.clone();
        let preparation = tokio::task::spawn_blocking(move || {
            preparation_store.spawn_supervised_run_driver(
                "parked-prepare".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                move |()| {
                    prepare_started_tx.send(()).map_err(|_| {
                        StoreError::InvalidPlan(
                            "shutdown test stopped before prepare admission".to_string(),
                        )
                    })?;
                    continue_prepare_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            StoreError::InvalidPlan(format!(
                                "parked prepare was not released: {error}"
                            ))
                        })?;
                    run_store.resume_task_run("parked-prepare")?;
                    Ok(((), move |_receipt_owner| async move {
                        std::fs::rename(&root_for_driver, &blocked_root_for_driver)
                            .map_err(|error| format!("block task root: {error}"))?;
                        std::fs::write(&root_for_driver, b"block directory recreation")
                            .map_err(|error| format!("replace task root: {error}"))?;
                        Err::<(), String>("injected prepared driver failure".to_string())
                    }))
                },
            )
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), prepare_started_rx)
            .await
            .map_err(|_| "parked prepare did not start".to_string())?
            .map_err(|_| "parked prepare start sender closed".to_string())?;

        let shutdown_store = std::sync::Arc::clone(&store);
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_run_drivers().await });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_run_driver_shutdown_started(),
        )
        .await
        .map_err(|_| "TaskRun shutdown did not close driver admission".to_string())?;
        if store
            .reserve_run_driver_admission(
                "late-after-shutdown".to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .is_ok()
        {
            return Err("TaskRun shutdown accepted a late driver reservation".to_string());
        }
        if shutdown.is_finished() {
            return Err("shutdown overtook an accepted parked preparation".to_string());
        }

        continue_prepare_tx
            .send(())
            .map_err(|_| "parked prepare receiver closed".to_string())?;
        let (_, result_waiter) =
            tokio::time::timeout(std::time::Duration::from_secs(2), preparation)
                .await
                .map_err(|_| "parked prepare did not register its driver".to_string())?
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
        drop(result_waiter);

        let shutdown_error = tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .map_err(|_| "TaskRun shutdown did not settle parked preparation".to_string())?
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "permanent prepared driver debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let abandoned = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "prepared driver abandonment is missing".to_string())?;
        assert_eq!(abandoned.run_id, "parked-prepare");
        assert_eq!(abandoned.target, TaskRunStatus::Cancelled);
        assert_eq!(abandoned.root, canonical_root);
        assert!(abandoned.driver_token.is_some());
        assert!(!abandoned.error.is_empty());
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(blocked_root, temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("parked-prepare")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "parked prepared run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Running);
        Ok(())
    }

    #[tokio::test]
    async fn overlapping_same_run_drivers_release_only_their_exact_receipts() -> Result<(), String>
    {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "overlap",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "exact driver receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let first_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel::<()>();
        let first_flag = std::sync::Arc::clone(&first_dropped);
        let first_admission = test_driver_admission(&store, "overlap")?;
        let first_waiter = store
            .spawn_run_driver(
                first_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(first_flag));
                    first_rx.await.map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let second_flag = std::sync::Arc::clone(&second_dropped);
        let second_admission = test_driver_admission(&store, "overlap")?;
        let second_waiter = store
            .spawn_run_driver(
                second_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(second_flag));
                    second_rx.await.map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;

        first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        first_waiter.await.map_err(|error| error.to_string())??;
        assert!(first_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_dropped.load(std::sync::atomic::Ordering::SeqCst));

        second_tx
            .send(())
            .map_err(|_| "second driver receiver closed".to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        store.wait_for_run_driver_idle("overlap").await;
        assert!(!store.is_run_active("overlap"));
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[test]
    fn overlapping_run_cancellation_tokens_release_in_any_order_and_cancel_together()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cancel-overlap",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "cancel every active driver",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cancel-overlap", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let first = echo_agent::agent::CancellationToken::new();
        let second = echo_agent::agent::CancellationToken::new();
        let first_guard = store
            .register_run_cancellation("cancel-overlap", first.clone())
            .map_err(|error| error.to_string())?;
        let second_guard = store
            .register_run_cancellation("cancel-overlap", second.clone())
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_pause("cancel-overlap")
                .map_err(|error| error.to_string())?
        );
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());

        drop(first_guard);
        assert!(store.is_run_active("cancel-overlap"));
        drop(second_guard);
        assert!(!store.is_run_active("cancel-overlap"));

        let shared = echo_agent::agent::CancellationToken::new();
        let third_guard = store
            .register_run_cancellation("cancel-overlap", shared.clone())
            .map_err(|error| error.to_string())?;
        let fourth_guard = store
            .register_run_cancellation("cancel-overlap", shared)
            .map_err(|error| error.to_string())?;
        drop(fourth_guard);
        assert!(store.is_run_active("cancel-overlap"));
        drop(third_guard);
        assert!(!store.is_run_active("cancel-overlap"));
        Ok(())
    }

    #[tokio::test]
    async fn opaque_driver_context_rejects_forged_wrong_stale_and_cross_run_receipts()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        for run_id in ["context-overlap", "context-other"] {
            store
                .create_run(
                    run_id,
                    "workspace-a",
                    "conversation",
                    "message",
                    DomainProfile::General,
                    "opaque driver execution context",
                    "",
                    AttendedMode::Unattended,
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run(run_id, TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
        }

        let (first_context_tx, first_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_store = std::sync::Arc::clone(&store);
        let first_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-overlap")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = first_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "first context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_first_rx.await.map_err(|error| error.to_string())?;
                        first_store
                            .finalize_run("context-overlap", TaskRunStatus::Completed, None)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let first_context = first_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("first driver context was not published: {error}"))?;

        let (second_context_tx, second_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_second_tx, finish_second_rx) = tokio::sync::oneshot::channel::<()>();
        let second_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-overlap")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = second_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "second context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_second_rx.await.map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let second_context = second_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("second driver context was not published: {error}"))?;
        assert_ne!(first_context, second_context);

        let (other_context_tx, other_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_other_tx, finish_other_rx) = tokio::sync::oneshot::channel::<()>();
        let other_store = std::sync::Arc::clone(&store);
        let other_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-other")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = other_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "other context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_other_rx.await.map_err(|error| error.to_string())?;
                        other_store
                            .finalize_run("context-other", TaskRunStatus::Completed, None)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let other_context = other_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("other driver context was not published: {error}"))?;

        let first_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &first_context,
                DropFlag(std::sync::Arc::clone(&first_released)),
            )
            .map_err(|_| "first exact context was rejected".to_string())?;
        let second_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &second_context,
                DropFlag(std::sync::Arc::clone(&second_released)),
            )
            .map_err(|_| "second exact context was rejected".to_string())?;

        for (label, run_id, context_id) in [
            (
                "forged sequential token",
                "context-overlap",
                "eko-task-driver:2".to_string(),
            ),
            (
                "wrong nonce",
                "context-overlap",
                format!("{first_context}-wrong"),
            ),
            (
                "cross-run context",
                "context-overlap",
                other_context.clone(),
            ),
        ] {
            let rejected_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let rejected = store
                .retain_run_driver_receipt_from_context(
                    run_id,
                    &context_id,
                    DropFlag(std::sync::Arc::clone(&rejected_released)),
                )
                .err()
                .ok_or_else(|| format!("{label} unexpectedly retained a receipt"))?;
            drop(rejected);
            assert!(rejected_released.load(std::sync::atomic::Ordering::SeqCst));
        }
        assert_eq!(store.active_run_driver_receipt_count()?, 2);

        finish_first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        first_waiter.await.map_err(|error| error.to_string())??;
        assert!(first_released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_released.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(store.active_run_driver_receipt_count()?, 1);

        let stale_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stale = store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &first_context,
                DropFlag(std::sync::Arc::clone(&stale_released)),
            )
            .err()
            .ok_or_else(|| "stale driver context unexpectedly retained a receipt".to_string())?;
        drop(stale);
        assert!(stale_released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_released.load(std::sync::atomic::Ordering::SeqCst));

        finish_second_tx
            .send(())
            .map_err(|_| "second driver receiver closed".to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_released.load(std::sync::atomic::Ordering::SeqCst));

        finish_other_tx
            .send(())
            .map_err(|_| "other driver receiver closed".to_string())?;
        other_waiter.await.map_err(|error| error.to_string())??;
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn abandoned_same_run_driver_releases_only_its_exact_receipt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "overlap-abandon",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "exact abandoned driver receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap-abandon", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;

        let first_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_flag = std::sync::Arc::clone(&first_dropped);
        let first_admission = test_driver_admission(&store, "overlap-abandon")?;
        let first_waiter = store
            .spawn_run_driver(
                first_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(first_flag));
                    first_started_tx
                        .send(())
                        .map_err(|_| "first driver start receiver closed".to_string())?;
                    finish_first_rx.await.map_err(|error| error.to_string())?;
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected first driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        first_started_rx.await.map_err(|error| error.to_string())?;

        let second_flag = std::sync::Arc::clone(&second_dropped);
        let store_for_second = std::sync::Arc::clone(&store);
        let second_admission = test_driver_admission(&store, "overlap-abandon")?;
        let second_waiter = store
            .spawn_run_driver(
                second_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(second_flag));
                    store_for_second
                        .finalize_run("overlap-abandon", TaskRunStatus::Completed, None)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!first_dropped.load(std::sync::atomic::Ordering::SeqCst));

        finish_first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        let first_error = first_waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "first driver failure was not reported".to_string())?;
        assert!(first_error.contains("terminal settlement failed"));
        assert_eq!(store.active_run_driver_receipt_count()?, 1);

        let shutdown_error = store
            .shutdown_run_drivers()
            .await
            .err()
            .ok_or_else(|| "abandoned first driver debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "first driver abandonment diagnostic is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "overlap-abandon");
        assert_eq!(diagnostic.driver_token, Some(1));
        assert!(first_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("overlap-abandon")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "overlap run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn exact_driver_releases_pool_before_memory_generation() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "lifo-receipts",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "release pool before memory",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("lifo-receipts", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("lifo-receipts", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let release_order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let memory_order = std::sync::Arc::clone(&release_order);
        let pool_order = std::sync::Arc::clone(&release_order);
        let admission = test_driver_admission(&store, "lifo-receipts")?;
        let waiter = store
            .spawn_run_driver(
                admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(ReleaseOrder(memory_order, "memory"));
                    receipt_owner.retain(ReleaseOrder(pool_order, "pool"));
                    Ok::<(), String>(())
                },
            )
            .map_err(|error| error.to_string())?;
        waiter.await.map_err(|error| error.to_string())??;
        let observed = release_order
            .lock()
            .map_err(|_| "release order lock is poisoned".to_string())?
            .clone();
        assert_eq!(observed, ["pool", "memory"]);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[test]
    fn create_run_emits_run_created_event() -> Result<(), StoreError> {
        let s = fresh()?;
        let run = s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::AiCoding,
            "review runtime",
            "",
            AttendedMode::Attended,
        )?;
        assert_eq!(run.status, TaskRunStatus::Pending);
        let evs = s.list_events("r1", 0)?;
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, RuntimeEventKind::RunCreated);
        Ok(())
    }

    #[test]
    fn artifact_round_trip_preserves_path_and_metadata() -> std::result::Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "artifact-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "artifact round trip",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let artifact = Artifact {
            id: "artifact-1".to_string(),
            run_id: "artifact-run".to_string(),
            task_id: None,
            kind: ArtifactKind::Trace,
            title: "Complete tool output".to_string(),
            path: Some("/tmp/tool-output.log".to_string()),
            metadata: serde_json::json!({
                "sha256": "abcdef",
                "retention": "conversation_or_30d",
            }),
        };
        store
            .add_artifact(&artifact)
            .map_err(|error| error.to_string())?;

        let artifacts = store
            .list_artifacts("artifact-run")
            .map_err(|error| error.to_string())?;
        let restored = artifacts
            .first()
            .ok_or_else(|| "artifact was not restored".to_string())?;
        assert_eq!(restored.path, artifact.path);
        assert_eq!(restored.metadata, artifact.metadata);
        Ok(())
    }

    #[test]
    fn transition_run_appends_status_event_atomically() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        let run = s.transition_run("r1", TaskRunStatus::Running)?;
        assert_eq!(run.status, TaskRunStatus::Running);
        let evs = s.list_events("r1", 0)?;
        // RunCreated + RunStatusChanged
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].event_type, RuntimeEventKind::RunStatusChanged);
        Ok(())
    }

    #[test]
    fn run_goal_update_is_revisioned_audited_and_deferred() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        let created = store
            .create_run(
                "goal-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "original goal",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(created.goal_revision, 1);
        assert_eq!(created.goal_sha256, task_goal_sha256("original goal"));
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "goal-plan".to_string(),
                run_id: "goal-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("original goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "goal-task".to_string(),
                    title: "Satisfy the original goal".to_string(),
                    description: "Produce traceable evidence".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let updated = store
            .update_run_goal(
                "goal-run",
                1,
                "revised goal",
                "user narrowed the requested scope",
                RunGoalActorSource::Cli,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.goal, "revised goal");
        assert_eq!(updated.goal_revision, 2);
        assert_eq!(updated.goal_sha256, task_goal_sha256("revised goal"));

        let event = store
            .list_events("goal-run", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|event| event.event_type == RuntimeEventKind::RunGoalUpdated)
            .ok_or_else(|| "RunGoalUpdated was not persisted".to_string())?;
        assert_eq!(event.payload["old_goal_revision"], 1);
        assert_eq!(event.payload["new_goal_revision"], 2);
        assert_eq!(
            event.payload["old_goal_sha256"],
            task_goal_sha256("original goal")
        );
        assert_eq!(
            event.payload["new_goal_sha256"],
            task_goal_sha256("revised goal")
        );
        assert_eq!(event.payload["actor_source"], "cli");
        assert!(
            event
                .payload
                .get("actor_user_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        );

        let continuation = store
            .get_run_state("goal-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation projection was not created".to_string())?;
        assert!(continuation.deferred);
        assert_eq!(
            continuation.deferred_reason.as_deref(),
            Some("goal_revision_unbound")
        );
        assert_eq!(
            last_frame_event_types(&store, "goal-run")?,
            ["run_goal_updated", "requirement_evidence_invalidated"]
        );
        Ok(())
    }

    #[test]
    fn run_goal_update_rejects_stale_revision_without_an_event() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&store, "goal-conflict").map_err(|error| error.to_string())?;
        let before = store
            .list_events("goal-conflict", 0)
            .map_err(|error| error.to_string())?
            .len();

        let error = store
            .update_run_goal(
                "goal-conflict",
                9,
                "revised goal",
                "explicit correction",
                RunGoalActorSource::Tui,
            )
            .err()
            .ok_or_else(|| "stale goal revision was accepted".to_string())?;
        assert!(matches!(error, StoreError::GoalConflict { .. }));
        assert_eq!(
            store
                .list_events("goal-conflict", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before
        );
        Ok(())
    }

    #[test]
    fn run_goal_update_requires_a_quiescent_paused_run() -> Result<(), String> {
        let active_turn = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        active_turn
            .create_run(
                "active-turn",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "original goal",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        active_turn
            .configure_run_continuation("active-turn", true, false, None, None)
            .map_err(|error| error.to_string())?;
        active_turn
            .transition_run("active-turn", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let claim = active_turn
            .claim_run_turn(
                "active-turn",
                "turn-1",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(claim, RunTurnClaimOutcome::Started(_)));
        active_turn
            .transition_run("active-turn", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let active_subagent =
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&active_subagent, "active-subagent")
            .map_err(|error| error.to_string())?;
        active_subagent
            .record_subagent_assigned(
                "active-subagent",
                "task-1",
                "execution-1",
                "researcher",
                "research",
                1,
                1,
                false,
                false,
            )
            .map_err(|error| error.to_string())?;

        let active_cell = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&active_cell, "active-cell").map_err(|error| error.to_string())?;
        active_cell
            .record_background_cell_started(
                "active-cell",
                "cell-1",
                "test cell",
                "command-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;

        let active_driver = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        create_paused_run(active_driver.as_ref(), "active-driver")
            .map_err(|error| error.to_string())?;
        let _driver_registration = active_driver
            .register_run_cancellation("active-driver", echo_agent::agent::CancellationToken::new())
            .map_err(|error| error.to_string())?;

        for (store, run_id, expected_reason) in [
            (&active_turn, "active-turn", "active RunTurn"),
            (&active_subagent, "active-subagent", "active Subagent"),
            (&active_cell, "active-cell", "active command cell"),
            (active_driver.as_ref(), "active-driver", "active driver"),
        ] {
            let error = store
                .update_run_goal(
                    run_id,
                    1,
                    "revised goal",
                    "explicit correction",
                    RunGoalActorSource::Gui,
                )
                .err()
                .ok_or_else(|| format!("goal update was accepted for {run_id}"))?;
            assert!(matches!(
                error,
                StoreError::GoalUpdateRejected { reason, .. }
                    if reason.contains(expected_reason)
            ));
        }
        Ok(())
    }

    #[test]
    fn task_update_rebinds_plan_before_goal_updated_run_can_resume() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.update_run_goal(
            "r1",
            1,
            "revised goal",
            "user changed the acceptance target",
            RunGoalActorSource::Tui,
        )?;

        let stale_plan = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert_eq!(stale_plan.goal_revision, 1);
        let resume_error = store
            .resume_task_run("r1")
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("stale plan resumed".to_string()))?;
        assert!(
            matches!(
                &resume_error,
                StoreError::PlanGoalMismatch {
                    plan_goal_revision: 1,
                    run_goal_revision: 2,
                    ..
                }
            ),
            "unexpected resume error: {resume_error}"
        );

        let rebound = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "align the task graph with goal revision 2".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        title: Some("Review revised runtime scope".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        assert_eq!(rebound.revision, 2);
        assert_eq!(rebound.goal_revision, 2);
        assert_eq!(rebound.goal_sha256, task_goal_sha256("revised goal"));
        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);

        let latest_plan_event = store
            .list_events("r1", 0)?
            .into_iter()
            .rev()
            .find(|event| event.event_type == RuntimeEventKind::PlanRevisionCommitted)
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert!(latest_plan_event.payload["plan"].get("goal").is_none());
        Ok(())
    }

    #[test]
    fn illegal_transition_is_rejected_and_leaves_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        // First transition to Running (was Pending → now legal).
        s.transition_run("r1", TaskRunStatus::Running)?;
        // Running → Completed is legal. Now test that Completed → Running is
        // illegal (terminal state → non-terminal is always rejected).
        let _before = s.list_events("r1", 0)?.len();
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let before_terminal = s.list_events("r1", 0)?.len();
        let err = s
            .transition_run("r1", TaskRunStatus::Running)
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("illegal transition unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event was appended — the tx rolled back.
        assert_eq!(s.list_events("r1", 0)?.len(), before_terminal);
        Ok(())
    }

    #[test]
    fn attach_plan_creates_tasks_and_todos() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        // attach_plan no longer changes the run status; caller decides.
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: vec!["a".into()],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan_for_test(&plan)?;

        let loaded = s
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].id, "t1");

        let todos = s.list_todos("r1")?;
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].task_id, "t1");
        assert_eq!(todos[0].status, TodoStatus::Pending);

        let run = s
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        // attach_plan no longer transitions status; run stays Pending.
        assert_eq!(run.status, TaskRunStatus::Pending);
        assert_eq!(run.plan_id.as_deref(), Some("p1"));
        Ok(())
    }

    #[test]
    fn set_task_status_updates_task_todo_and_event_together() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("code_reviewer"),
            None,
        )?;
        let todos = s.list_todos("r1")?;
        assert_eq!(todos[0].status, TodoStatus::Running);
        assert_eq!(todos[0].owner_agent.as_deref(), Some("code_reviewer"));
        assert!(todos[0].started_at.is_some());

        let evs = s.list_events("r1", 0)?;
        assert!(
            evs.iter()
                .any(|e| e.event_type == RuntimeEventKind::TaskStarted)
        );
        Ok(())
    }

    #[test]
    fn task_terminal_events_follow_typed_status_not_detail_text() -> Result<(), StoreError> {
        let failed = fresh()?;
        seed_plan(&failed)?;
        failed.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("code_reviewer"),
            Some("the report mentions timeout and cancelled behavior"),
        )?;
        let failed_events = failed.list_events("r1", 0)?;
        assert!(
            failed_events
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskFailed)
        );
        assert!(failed_events.iter().all(|event| {
            !matches!(
                event.event_type,
                RuntimeEventKind::TaskTimedOut | RuntimeEventKind::TaskCancelled
            )
        }));

        let timed_out = fresh()?;
        seed_plan(&timed_out)?;
        timed_out.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::TimedOut {
                error: String::new(),
            },
            Some("code_reviewer"),
            Some("provider deadline elapsed"),
        )?;
        assert!(
            timed_out
                .list_events("r1", 0)?
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskTimedOut)
        );

        let cancelled = fresh()?;
        seed_plan(&cancelled)?;
        cancelled.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Cancelled,
            Some("code_reviewer"),
            Some("stopped by parent run"),
        )?;
        assert!(
            cancelled
                .list_events("r1", 0)?
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskCancelled)
        );
        Ok(())
    }

    #[test]
    fn task_todo_characterization_tracks_dynamic_graph_run_state_todo_and_recovery()
    -> Result<(), StoreError> {
        let temp =
            tempfile::tempdir().map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let run_id = "task-todo-characterization";
        store.create_run(
            run_id,
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "characterize TaskRuntime projections",
            "task",
            AttendedMode::Attended,
        )?;

        let mut upstream = sample_task_body("upstream");
        upstream.max_retries = 2;
        let mut dependent = sample_task_body("dependent");
        dependent.depends_on = vec![upstream.id.clone()];
        let blocked = sample_task_body("blocked");
        let timed_out = sample_task_body("timed-out");
        let skipped = sample_task_body("skipped");
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "task-todo-characterization-plan".to_string(),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("characterize TaskRuntime projections"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![upstream, dependent, blocked, timed_out, skipped],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;

        let assert_graph_and_projections =
            |store: &TaskRuntimeStore, expected_revision: u64| -> Result<(), StoreError> {
                let plan = store
                    .get_plan(run_id)?
                    .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
                let snapshot = store
                    .get_run_state(run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                let runtime = store.load_runtime_plan_snapshot(run_id)?;
                let todos = store.list_todos(run_id)?;
                assert_eq!(plan.revision, expected_revision);
                assert_eq!(runtime.revision, expected_revision);
                assert_eq!(plan.tasks.len(), snapshot.tasks.len());
                assert_eq!(plan.tasks.len(), runtime.tasks.len());
                assert_eq!(plan.tasks.len(), todos.len());
                for task in &plan.tasks {
                    if !snapshot.tasks.iter().any(|state| state.task_id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "run-state task {} missing",
                            task.id
                        )));
                    }
                    if !runtime.tasks.iter().any(|state| state.spec.id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "runtime graph task {} missing",
                            task.id
                        )));
                    }
                    if !todos.iter().any(|todo| todo.task_id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "Todo projection {} missing",
                            task.id
                        )));
                    }
                }
                Ok(())
            };

        assert_graph_and_projections(&store, 1)?;

        let mut patched = sample_task_body("patched");
        patched.title = "Inserted during characterization".to_string();
        let patched_plan = store.apply_task_patch_for_test(
            run_id,
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise dynamic graph insertion".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("upstream".to_string()),
                    task: patched.spec(),
                }],
            },
        )?;
        assert_eq!(patched_plan.revision, 2);
        assert_graph_and_projections(&store, 2)?;
        assert_eq!(
            store
                .list_todos(run_id)?
                .iter()
                .find(|todo| todo.task_id == "patched")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );

        store.set_task_status(
            run_id,
            "upstream",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("implementer"),
            Some("upstream failed"),
        )?;
        store.set_task_status(
            run_id,
            "blocked",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("awaiting explicit decision"),
        )?;
        store.set_task_status(
            run_id,
            "timed-out",
            echo_agent::tasks::TaskStatus::TimedOut {
                error: String::new(),
            },
            Some("reviewer"),
            Some("deadline elapsed"),
        )?;
        let skipped_plan = store.apply_task_patch_for_test(
            run_id,
            &TaskUpdateRequest {
                base_revision: 2,
                reason: "exercise explicit skip projection".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "skipped".to_string(),
                }],
            },
        )?;
        assert_eq!(skipped_plan.revision, 3);
        assert_graph_and_projections(&store, 3)?;

        let runtime = store.load_runtime_plan_snapshot(run_id)?;
        let runtime_status = |task_id: &str| {
            runtime
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .map(|task| &task.execution.status)
        };
        assert!(matches!(
            runtime_status("upstream"),
            Some(echo_agent::tasks::TaskStatus::Failed(detail)) if detail == "upstream failed"
        ));
        assert_eq!(
            runtime_status("dependent"),
            Some(&echo_agent::tasks::TaskStatus::Pending)
        );
        assert!(matches!(
            runtime_status("blocked"),
            Some(echo_agent::tasks::TaskStatus::Blocked(detail)) if detail == "awaiting explicit decision"
        ));
        assert!(matches!(
            runtime_status("timed-out"),
            Some(echo_agent::tasks::TaskStatus::TimedOut { error }) if error == "deadline elapsed"
        ));
        assert_eq!(
            runtime_status("skipped"),
            Some(&echo_agent::tasks::TaskStatus::Skipped)
        );
        let todos = store.list_todos(run_id)?;
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "dependent")
                .map(|todo| todo.status),
            Some(TodoStatus::Blocked)
        );
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "timed-out")
                .map(|todo| todo.status),
            Some(TodoStatus::TimedOut)
        );
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "skipped")
                .map(|todo| todo.status),
            Some(TodoStatus::Skipped)
        );

        store.transition_run(run_id, TaskRunStatus::Failed)?;
        assert_eq!(store.retry_blocked_task(run_id, "upstream")?, 1);
        assert_graph_and_projections(&store, 3)?;
        let retried = store.load_runtime_plan_snapshot(run_id)?;
        let retried_upstream = retried
            .tasks
            .iter()
            .find(|task| task.spec.id == "upstream")
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        assert_eq!(
            &retried_upstream.execution.status,
            &echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(retried_upstream.execution.retry_count, 1);
        assert_eq!(
            store
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            store
                .list_todos(run_id)?
                .iter()
                .find(|todo| todo.task_id == "dependent")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );

        assert!(store.request_cancel(run_id)?);
        let cancelled_runtime = store.load_runtime_plan_snapshot(run_id)?;
        for task in &cancelled_runtime.tasks {
            if task.spec.id == "timed-out" || task.spec.id == "skipped" {
                continue;
            }
            assert_eq!(
                &task.execution.status,
                &echo_agent::tasks::TaskStatus::Cancelled
            );
        }
        assert_eq!(
            store
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        let cancelled_todos = store.list_todos(run_id)?;
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "upstream")
                .map(|todo| todo.status),
            Some(TodoStatus::Cancelled)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "blocked")
                .map(|todo| todo.status),
            Some(TodoStatus::Cancelled)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "timed-out")
                .map(|todo| todo.status),
            Some(TodoStatus::TimedOut)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "skipped")
                .map(|todo| todo.status),
            Some(TodoStatus::Skipped)
        );

        let recovery_id = "restart-recovery-characterization";
        store.create_run(
            recovery_id,
            "ws",
            "conversation",
            "recovery-message",
            DomainProfile::General,
            "characterize restart recovery",
            "task",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "restart-recovery-characterization-plan".to_string(),
            run_id: recovery_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("characterize restart recovery"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![sample_task_body("recovery-task")],
        })?;
        store.transition_run(recovery_id, TaskRunStatus::Running)?;
        store.set_task_status(
            recovery_id,
            "recovery-task",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            Some("interrupted before completion"),
        )?;
        drop(store);

        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(
            restarted
                .get_run(recovery_id)?
                .ok_or_else(|| StoreError::RunNotFound(recovery_id.to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            restarted
                .list_todos(recovery_id)?
                .iter()
                .find(|todo| todo.task_id == "recovery-task")
                .map(|todo| todo.status),
            Some(TodoStatus::Running)
        );
        assert_eq!(restarted.recover_incomplete()?, 1);
        let recovered_state = restarted
            .get_run_state(recovery_id)?
            .ok_or_else(|| StoreError::RunNotFound(recovery_id.to_string()))?;
        assert_eq!(recovered_state.run.status, TaskRunStatus::Paused);
        let recovered_task = recovered_state
            .tasks
            .iter()
            .find(|task| task.task_id == "recovery-task")
            .ok_or_else(|| StoreError::TaskNotFound("recovery-task".to_string()))?;
        // Current recovery pauses the Run but resets an interrupted task to
        // Pending so it can be reclaimed explicitly on resume. Todo is the
        // matching read-only projection of that canonical task status.
        assert_eq!(
            recovered_task.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(
            restarted
                .list_todos(recovery_id)?
                .iter()
                .find(|todo| todo.task_id == "recovery-task")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );
        Ok(())
    }

    #[test]
    fn list_todos_is_read_only_and_does_not_append_journal() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let events_path = store.active_shadow_root().join("r1").join("events.jsonl");
        let before_events = std::fs::read(&events_path)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let before_count = store.list_events("r1", 0)?.len();
        let first = store.list_todos("r1")?;
        let second = store.list_todos("r1")?;
        let first_value = serde_json::to_value(&first)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let second_value = serde_json::to_value(&second)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(first_value, second_value);
        assert_eq!(store.list_events("r1", 0)?.len(), before_count);
        let after_events = std::fs::read(&events_path)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(before_events, after_events);
        Ok(())
    }

    #[test]
    fn put_summary_upserts_and_get_summary_reads() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let sum = TaskExecutionSummary {
            run_id: "r1".into(),
            task_id: "t1".into(),
            subagent_name: "code_reviewer".into(),
            outcome: SubagentOutcome {
                contract_version: 1,
                status: SubagentStatus::Completed,
                summary: "read chat.rs".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: vec![SubagentVerification {
                    check: "cargo check".into(),
                    status: SubagentVerificationStatus::Passed,
                    details: String::new(),
                    source: SubagentEvidenceSource::Observed,
                }],
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles {
                    read: vec!["chat.rs".into()],
                    written: Vec::new(),
                },
            },
            decisions: vec!["route via TaskRuntime".into()],
            next_implications: vec!["implement router".into()],
            suggested_tasks: vec![],
            created_at: Utc::now(),
        };
        s.put_summary(&sum)?;
        let got = s
            .get_summary("r1", "t1")?
            .ok_or_else(|| StoreError::TaskNotFound("t1 summary".to_string()))?;
        assert_eq!(got.outcome.summary, "read chat.rs");
        assert_eq!(got.next_implications.len(), 1);
        Ok(())
    }

    #[test]
    fn latest_run_for_conversation_orders_by_created_desc() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g1",
            "",
            AttendedMode::Attended,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.create_run(
            "r2",
            "ws",
            "c1",
            "m2",
            DomainProfile::General,
            "g2",
            "",
            AttendedMode::Attended,
        )?;
        let latest = s
            .latest_run_for_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("latest run for c1".to_string()))?;
        assert_eq!(latest.run_id, "r2");
        Ok(())
    }

    fn seed_plan(s: &TaskRuntimeStore) -> Result<(), StoreError> {
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan_for_test(&plan)?;
        s.transition_run("r1", TaskRunStatus::Running)?;
        Ok(())
    }

    #[test]
    fn budget_update_requires_existing_continuation_and_preserves_policy() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        assert!(
            store
                .update_run_continuation_budgets("r1", Some(100), Some(60))
                .is_err()
        );
        store.configure_run_continuation("r1", true, true, None, None)?;
        let updated = store.update_run_continuation_budgets("r1", Some(100), Some(60))?;
        assert_eq!(updated.token_budget, Some(100));
        assert_eq!(updated.time_budget_seconds, Some(60));
        assert!(updated.auto_resume_after_restart);
        assert!(
            store
                .update_run_continuation_budgets("r1", Some(0), None)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn lowering_budget_atomically_pauses_and_cancels_active_driver() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, true, None, None)?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 40, 20)?);
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        let updated = store.update_run_continuation_budgets("r1", Some(60), None)?;

        assert!(token.is_cancelled());
        assert_eq!(updated.token_budget, Some(60));
        assert_eq!(
            updated.pause.as_ref().map(|pause| pause.reason),
            Some(RunPauseReason::TokenBudget)
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let budget_event = store
            .list_events("r1", 0)?
            .into_iter()
            .rev()
            .find(|event| event.event_type == RuntimeEventKind::RunContinuationConfigured)
            .ok_or_else(|| StoreError::InvalidPlan("budget event missing".to_string()))?;
        assert_eq!(
            budget_event
                .payload
                .get("pause_reason")
                .and_then(serde_json::Value::as_str),
            Some("token_budget")
        );
        drop(registration);
        Ok(())
    }

    #[test]
    fn subagent_usage_is_idempotent_and_parent_turn_owns_wall_clock() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, Some(100), Some(20))?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.record_subagent_assigned(
            "r1",
            "t1",
            "execution-1",
            "code_reviewer",
            "Review runtime",
            1,
            1,
            true,
            false,
        )?;

        assert!(!store.account_subagent_usage(
            "r1",
            "execution-1",
            "provider-total",
            12,
            8,
            2_500,
        )?);
        assert!(!store.account_subagent_usage(
            "r1",
            "execution-1",
            "provider-total",
            12,
            8,
            2_500,
        )?);
        let during_turn = store
            .get_run_state("r1")?
            .and_then(|state| state.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert_eq!(during_turn.tokens_used, 20);
        assert_eq!(during_turn.time_used_seconds, 0);
        let subagent_runs = store.list_subagent_runs("r1")?;
        let subagent_run = subagent_runs
            .first()
            .ok_or_else(|| StoreError::InvalidPlan("SubagentRun projection missing".to_string()))?;
        assert_eq!(subagent_run.subagent_run_id, "execution-1");
        assert_eq!(subagent_run.usage.tokens_used, Some(20));
        assert_eq!(subagent_run.usage.duration_ms, Some(2_500));
        let result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "review complete",
            Vec::new(),
        );
        let terminal_usage = ExecutionUsage {
            duration_ms: Some(2_500),
            tokens_used: Some(20),
            iterations: Some(2),
        };
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: "execution-1",
            agent_name: "code_reviewer",
            task_subject: "Review runtime",
            plan_revision: 1,
            attempt: 1,
            status: "completed",
            outcome: Some(&result),
            full_output: Some("review complete"),
            usage: Some(&terminal_usage),
            dispatch_hook: false,
        })?;
        let settled_runs = store.list_subagent_runs("r1")?;
        let settled = settled_runs.first().ok_or_else(|| {
            StoreError::InvalidPlan("settled SubagentRun projection missing".to_string())
        })?;
        assert_eq!(settled.status, SubagentStatus::Completed);
        assert_eq!(settled.outcome.as_ref(), Some(&result));
        assert_eq!(settled.usage, terminal_usage);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::RunTurnUsageAccounted
                        && event
                            .payload
                            .get("source_scope")
                            .and_then(serde_json::Value::as_str)
                            == Some("subagent")
                })
                .count(),
            1
        );

        let finished = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 3,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(finished.tokens_used, 20);
        assert_eq!(finished.time_used_seconds, 3);
        Ok(())
    }

    #[test]
    fn cell_terminal_and_defer_race_cannot_leave_lost_wakeup() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("r1", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .record_background_cell_started(
                "r1",
                "cell-1",
                "cargo test",
                "hash",
                Some("turn-1"),
                None,
                Some("call-1"),
            )
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let defer_store = store.clone();
        let defer_barrier = barrier.clone();
        let defer = std::thread::spawn(move || {
            defer_barrier.wait();
            defer_store.defer_continuation_for_active_cells("r1")
        });
        let terminal_store = store.clone();
        let terminal = std::thread::spawn(move || {
            barrier.wait();
            terminal_store.record_background_cell_finished(
                "r1",
                "cell-1",
                "cargo test",
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
                2,
                false,
                Some("ok"),
                None,
                None,
                Some("call-1"),
            )?;
            super::super::continuation::wake_after_cell_terminal(&terminal_store, "r1");
            Ok::<(), StoreError>(())
        });

        defer
            .join()
            .map_err(|_| "defer thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        terminal
            .join()
            .map_err(|_| "terminal thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        let continuation = store
            .get_run_state("r1")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation missing".to_string())?;
        assert!(!continuation.deferred);
        Ok(())
    }

    #[test]
    fn cell_terminal_retry_uses_the_exact_checkpointed_terminal_fact() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "cell-retry",
            "prepared name",
            "hash",
            None,
            None,
            Some("start-call"),
        )?;

        for _ in 0..2 {
            store.record_background_cell_finished(
                "r1",
                "cell-retry",
                "terminal name",
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
                2,
                false,
                Some("ok"),
                None,
                None,
                None,
            )?;
        }

        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "cell-retry")
            .ok_or_else(|| StoreError::InvalidPlan("terminal cell missing".to_string()))?;
        assert_eq!(cell.name, "terminal name");
        assert_eq!(cell.call_id, None);
        assert_eq!(cell.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::BackgroundCellFinished
                        && event
                            .payload
                            .get("cell_id")
                            .and_then(serde_json::Value::as_str)
                            == Some("cell-retry")
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn concurrent_run_turn_claim_has_one_authoritative_winner() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("r1", true, false, None, None)
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let mut threads = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                store.claim_run_turn(
                    "r1",
                    &format!("turn-{index}"),
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )
            }));
        }
        let mut started = Vec::new();
        let mut already_running = 0_usize;
        for thread in threads {
            let outcome = thread
                .join()
                .map_err(|_| "RunTurn claim thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
            match outcome {
                RunTurnClaimOutcome::Started(summary) => started.push(summary),
                RunTurnClaimOutcome::NotSubmitted(
                    ContinuationNotSubmittedReason::AlreadyRunning,
                ) => already_running = already_running.saturating_add(1),
                other => return Err(format!("unexpected claim outcome: {other:?}")),
            }
        }
        assert_eq!(started.len(), 1);
        assert_eq!(already_running, 15);
        assert_eq!(started.first().map(|turn| turn.ordinal), Some(1));
        assert_eq!(
            store
                .get_run_state("r1")
                .map_err(|error| error.to_string())?
                .and_then(|state| state.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.ordinal),
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn run_turn_accounting_is_idempotent_and_rejects_cross_turn_events() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, Some(100), None)?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible,)?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert!(
            store
                .account_run_turn_usage("r1", "wrong-turn", "usage-1", 10, 20)
                .is_err()
        );
        assert!(
            store
                .record_run_turn_compaction("r1", "wrong-turn", "compact-1")
                .is_err()
        );
        assert!(
            store
                .finish_run_turn(
                    "r1",
                    RunTurnCompletion {
                        turn_id: "wrong-turn",
                        status: RunTurnStatus::Ended,
                        elapsed_seconds: 7,
                        final_message_id: None,
                        error_fingerprint: None,
                    },
                )
                .is_err()
        );
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 10, 20)?);
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 10, 20)?);
        store.record_run_turn_compaction("r1", "turn-1", "compact-1")?;
        store.record_run_turn_compaction("r1", "turn-1", "compact-1")?;
        let first = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 7,
                final_message_id: Some("message-1"),
                error_fingerprint: None,
            },
        )?;
        let replay = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Failed,
                elapsed_seconds: 99,
                final_message_id: None,
                error_fingerprint: Some("must-not-overwrite"),
            },
        )?;
        assert_eq!(first, replay);
        assert_eq!(replay.tokens_used, 30);
        assert_eq!(replay.time_used_seconds, 7);
        assert_eq!(replay.compaction_count, 1);
        let last = replay
            .last_turn
            .ok_or_else(|| StoreError::InvalidPlan("finished RunTurn missing".to_string()))?;
        assert_eq!(last.input_tokens, 10);
        assert_eq!(last.output_tokens, 20);
        assert_eq!(last.compaction_count, 1);
        assert_eq!(last.status, RunTurnStatus::Ended);
        assert_eq!(last.final_message_id.as_deref(), Some("message-1"));
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-1",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            ),
            Err(StoreError::InvalidPlan(_))
        ));
        Ok(())
    }

    #[test]
    fn time_budget_stops_at_exact_boundary_and_cannot_be_bypassed_by_resume()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, Some(7))?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        let state = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 7,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(state.time_budget_seconds, Some(7));
        assert_eq!(state.time_used_seconds, 7);
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-2",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::NotSubmitted(ContinuationNotSubmittedReason::TimeBudgetExhausted)
        ));

        assert!(store.request_pause_with_reason(
            "r1",
            RunPauseReason::TimeBudget,
            Some("configured time budget exhausted"),
        )?);
        let error = store
            .resume_task_run("r1")
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("resume unexpectedly succeeded".to_string()))?;
        assert!(error.to_string().contains("time budget"));
        Ok(())
    }

    #[test]
    fn one_hundred_turns_and_compactions_replay_without_double_accounting() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial_goal_sha256 = store
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
            .goal_sha256;
        store.configure_run_continuation("r1", true, false, None, None)?;
        for ordinal in 1..=100_u64 {
            let turn_id = format!("soak-turn-{ordinal}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            let provider_event_id = format!("usage-{ordinal}");
            assert!(!store.account_run_turn_usage("r1", &turn_id, &provider_event_id, 1, 2,)?);
            assert!(!store.account_run_turn_usage("r1", &turn_id, &provider_event_id, 1, 2,)?);
            let compaction_event_id = format!("compact-{ordinal}");
            store.record_run_turn_compaction("r1", &turn_id, &compaction_event_id)?;
            store.record_run_turn_compaction("r1", &turn_id, &compaction_event_id)?;
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Ended,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: None,
                },
            )?;
        }

        let events = store.list_events("r1", 0)?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| StoreError::InvalidPlan("replay sequence is unavailable".to_string()))?;
        let replayed = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .run_state_with_sequence(journal_sequence)
            .continuation
            .ok_or_else(|| StoreError::InvalidPlan("soak continuation missing".to_string()))?;
        assert_eq!(replayed.tokens_used, 300);
        assert_eq!(replayed.time_used_seconds, 100);
        assert_eq!(replayed.compaction_count, 100);
        assert_eq!(replayed.next_turn_ordinal, 101);
        assert!(replayed.active_turn.is_none());
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .goal_sha256,
            initial_goal_sha256
        );
        Ok(())
    }

    #[test]
    fn provider_retry_schedule_rebuilds_and_counts_across_fingerprints() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        let base = Utc::now() - chrono::Duration::hours(1);

        for (turn_id, fingerprint, expected_attempt, offset) in [
            ("retry-turn-1", "provider-a", 1_u32, 0_i64),
            ("retry-turn-2", "provider-a", 2_u32, 1_i64),
            ("retry-turn-3", "provider-b", 3_u32, 2_i64),
        ] {
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some(fingerprint),
                },
            )?;
            let scheduled = store.schedule_provider_retry_at(
                "r1",
                fingerprint,
                base + chrono::Duration::seconds(offset),
            )?;
            assert_eq!(scheduled.state().attempt_count, expected_attempt);
            assert_eq!(scheduled.state().error_fingerprint, fingerprint);
        }

        let events = store.list_events("r1", 0)?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| StoreError::InvalidPlan("replay sequence is unavailable".to_string()))?;
        let replayed = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .run_state_with_sequence(journal_sequence)
            .continuation
            .and_then(|state| state.provider_retry)
            .ok_or_else(|| StoreError::InvalidPlan("provider retry did not rebuild".to_string()))?;
        assert_eq!(replayed.attempt_count, 3);
        assert_eq!(replayed.error_fingerprint, "provider-b");
        assert_eq!(replayed.first_failure_at, base);
        assert_eq!(
            stable_provider_retry_delay_millis("r1", "provider-b", 1),
            stable_provider_retry_delay_millis("r1", "provider-b", 1)
        );
        Ok(())
    }

    #[test]
    fn provider_retry_claim_waits_then_success_clears_schedule() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "failed-turn",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "failed-turn",
                status: RunTurnStatus::Failed,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: Some("provider-a"),
            },
        )?;
        store.schedule_provider_retry_at("r1", "provider-a", Utc::now())?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "too-early",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::NotSubmitted(ContinuationNotSubmittedReason::ProviderRetryBackoff)
        ));

        let past = Utc::now() - chrono::Duration::hours(1);
        store.schedule_provider_retry_at("r1", "provider-b", past)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "successful-retry",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        let state = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "successful-retry",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert!(state.provider_retry.is_none());
        Ok(())
    }

    #[test]
    fn fifth_provider_failure_atomically_pauses_and_explicit_resume_resets_retry()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, true, None, None)?;
        let base = Utc::now() - chrono::Duration::hours(1);
        for attempt in 1..=MAX_PROVIDER_RETRY_ATTEMPTS {
            let turn_id = format!("retry-exhaustion-{attempt}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some("provider-a"),
                },
            )?;
            let disposition = store.schedule_provider_retry_at(
                "r1",
                "provider-a",
                base + chrono::Duration::seconds(i64::from(attempt)),
            )?;
            assert_eq!(disposition.state().attempt_count, attempt);
        }
        let snapshot = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(snapshot.run.status, TaskRunStatus::Paused);
        let continuation = snapshot
            .continuation
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert!(
            continuation
                .provider_retry
                .as_ref()
                .is_some_and(|retry| retry.exhausted)
        );
        assert_eq!(
            continuation.pause.map(|pause| pause.reason),
            Some(RunPauseReason::ProviderUnavailable)
        );

        store.resume_task_run("r1")?;
        let resumed = store
            .get_run_state("r1")?
            .and_then(|snapshot| snapshot.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("resumed continuation missing".to_string()))?;
        assert!(resumed.provider_retry.is_none());
        Ok(())
    }

    fn prepare_boot_auto_resume_run(
        store: &TaskRuntimeStore,
        run_id: &str,
        attended_mode: AttendedMode,
    ) -> Result<(), StoreError> {
        let workspace_id = store.active_workspace_id();
        store.create_run(
            run_id,
            &workspace_id,
            &format!("background:test:{run_id}"),
            "root",
            DomainProfile::General,
            "boot goal",
            "bg:kind:test",
            attended_mode,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("{run_id}-plan"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("boot goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: format!("{run_id}-task"),
                title: "Resume safely".to_string(),
                ..PlanTask::default()
            }],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.configure_run_continuation(run_id, true, true, None, None)?;
        store.record_run_pause_reason(
            run_id,
            RunPauseReason::BootRecovery,
            Some("test process interruption"),
        )?;
        store.transition_run(run_id, TaskRunStatus::Paused)?;
        Ok(())
    }

    #[test]
    fn boot_auto_resume_admission_rejects_missing_owner_workspace_and_unsafe_boundary()
    -> Result<(), StoreError> {
        let store = fresh()?;
        prepare_boot_auto_resume_run(&store, "attended", AttendedMode::Attended)?;
        let attended = store.boot_auto_resume_decision("attended", true, false)?;
        assert!(matches!(
            attended,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::InteractiveOwnerUnavailable)
        ));

        prepare_boot_auto_resume_run(&store, "disabled", AttendedMode::Unattended)?;
        store.configure_run_continuation("disabled", true, false, None, None)?;
        assert!(matches!(
            store.boot_auto_resume_decision("disabled", true, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::AutoResumeDisabled)
        ));

        prepare_boot_auto_resume_run(&store, "launcher", AttendedMode::Unattended)?;
        assert!(matches!(
            store.boot_auto_resume_decision("launcher", false, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::LauncherUnavailable)
        ));

        prepare_boot_auto_resume_run(&store, "unsafe", AttendedMode::Unattended)?;
        store.record_recovery_blocker(
            "unsafe",
            "unsafe-task",
            Some("execution"),
            Some("call"),
            Some("shell"),
            "indeterminate side effect",
        )?;
        let unsafe_decision = store.boot_auto_resume_decision("unsafe", true, false)?;
        assert!(matches!(
            unsafe_decision,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::RecoveryBlocker)
        ));

        let mismatched = fresh()?;
        mismatched.create_run(
            "mismatch",
            "different-workspace",
            "background:test:mismatch",
            "root",
            DomainProfile::General,
            "boot goal",
            "bg:kind:test",
            AttendedMode::Unattended,
        )?;
        mismatched.attach_plan_for_test(&TaskPlan {
            plan_id: "mismatch-plan".to_string(),
            run_id: "mismatch".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("boot goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "mismatch-task".to_string(),
                title: "Stay paused".to_string(),
                ..PlanTask::default()
            }],
        })?;
        mismatched.transition_run("mismatch", TaskRunStatus::Running)?;
        mismatched.configure_run_continuation("mismatch", true, true, None, None)?;
        mismatched.record_run_pause_reason(
            "mismatch",
            RunPauseReason::BootRecovery,
            Some("test process interruption"),
        )?;
        mismatched.transition_run("mismatch", TaskRunStatus::Paused)?;
        assert!(matches!(
            mismatched.boot_auto_resume_decision("mismatch", true, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::WorkspaceMismatch)
        ));
        Ok(())
    }

    #[test]
    fn competing_boot_launchers_have_one_atomic_resume_winner() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_boot_auto_resume_run(&store, "race", AttendedMode::Unattended)
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let thread_store = std::sync::Arc::clone(&store);
            let thread_barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                thread_barrier.wait();
                thread_store.resume_task_run_after_boot("race", true, false)
            }));
        }
        barrier.wait();
        let mut resumed = 0_usize;
        for thread in threads {
            let outcome = thread
                .join()
                .map_err(|_| "boot resume thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
            if matches!(outcome, BootAutoResumeOutcome::Resumed(_)) {
                resumed = resumed.saturating_add(1);
            }
        }
        assert_eq!(resumed, 1);
        Ok(())
    }

    #[test]
    fn resume_task_run_transitions_paused_to_running() -> Result<(), String> {
        let s = fresh().map_err(|error| error.to_string())?;
        seed_plan(&s).map_err(|error| error.to_string())?;
        // Simulate user interrupt: Running -> Paused.
        s.transition_run("r1", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let run = s
            .get_run("r1")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "resumable TaskRun missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Paused);

        // Resume: Paused -> Running.
        let run = s.resume_task_run("r1").map_err(|error| error.to_string())?;
        assert_eq!(run.status, TaskRunStatus::Running);

        // Event log contains the Paused and Running transitions.
        let evs = s.list_events("r1", 0).map_err(|error| error.to_string())?;
        let status_changes: Vec<_> = evs
            .iter()
            .filter(|e| e.event_type == RuntimeEventKind::RunStatusChanged)
            .collect();
        assert!(status_changes.len() >= 2);
        assert_eq!(
            last_frame_event_types(&s, "r1")?,
            [
                "run_status_changed",
                "run_pause_reason_changed",
                "run_continuation_resumed",
            ]
        );
        Ok(())
    }

    #[test]
    fn expected_resume_and_run_turn_claim_commit_in_one_frame() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let expected = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        let before_events = store.list_events("r1", 0)?.len();
        assert!(
            store
                .resume_and_claim_run_turn_expected(
                    &expected,
                    "invalid-origin-turn",
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )
                .is_err()
        );
        assert_eq!(store.list_events("r1", 0)?.len(), before_events);

        assert!(matches!(
            store.resume_and_claim_run_turn_expected(
                &expected,
                "resume-turn",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(ref turn) if turn.turn_id == "resume-turn"
        ));
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            [
                "run_status_changed",
                "run_pause_reason_changed",
                "run_continuation_resumed",
                "run_turn_started",
            ]
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
    }

    #[test]
    fn expected_resume_allows_only_execution_path_diagnostic_suffix() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let expected = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.record_execution_path("r1", "formal_plan")?;

        assert!(matches!(
            store.resume_and_claim_run_turn_expected(
                &expected,
                "diagnostic-race-resume",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(ref turn)
                if turn.turn_id == "diagnostic-race-resume"
        ));
        Ok(())
    }

    #[test]
    fn expected_resume_sequence_rejects_status_attachment_and_continuation_aba()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let first = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        let status_epoch = TaskRunResumeIdentity::capture(&first);
        store.resume_task_run_expected(&status_epoch)?;
        store.request_pause("r1")?;
        let before_status_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&status_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_status_retry);

        let attachment_epoch = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.set_run_attachments(
            "r1",
            &[crate::attachments::AttachmentRef {
                path: std::path::PathBuf::from("/tmp/resume-epoch.txt"),
                name: "resume-epoch.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: crate::types::AttachmentSource::default(),
            }],
        )?;
        let before_attachment_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&attachment_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_attachment_retry);

        let continuation_epoch = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.configure_run_continuation("r1", true, false, Some(100), None)?;
        let before_continuation_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&continuation_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_continuation_retry);
        Ok(())
    }

    #[test]
    fn stale_expected_resume_cannot_mutate_recreated_run() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let stale = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );

        store.shadow.remove_runs(&["r1".to_string()])?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let before_events = store.list_events("r1", 0)?.len();
        let before = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;

        let error = store
            .resume_and_claim_run_turn_expected(
                &stale,
                "stale-resume-turn",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("stale resume unexpectedly won".to_string()))?;
        assert!(error.to_string().contains("identity changed"));
        assert_eq!(store.list_events("r1", 0)?.len(), before_events);
        let after = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(after.run.status, TaskRunStatus::Paused);
        assert!(before.run.attachments.is_empty());
        assert!(after.run.attachments.is_empty());
        assert_eq!(after.continuation, before.continuation);
        assert!(
            after
                .continuation
                .as_ref()
                .is_none_or(|continuation| continuation.active_turn.is_none())
        );
        Ok(())
    }

    #[test]
    fn inactive_cancel_terminalizes_todos_and_run_in_one_frame() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Running,
                Some("subagent"),
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?
        );
        assert_eq!(
            last_frame_event_types(&store, "r1")?,
            ["task_cancelled", "run_status_changed", "run_cancelled"]
        );
        let todo = store
            .list_todos("r1")
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| "cancelled Todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Cancelled);
        assert_eq!(todo.owner_agent.as_deref(), Some("code_reviewer"));
        assert_eq!(
            store
                .get_run("r1")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "cancelled TaskRun missing".to_string())?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn idle_long_horizon_run_accepts_pause_resume_and_cancel() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        for ordinal in 1..=3 {
            let turn_id = format!("turn-{ordinal}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Ended,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: None,
                },
            )?;
        }
        assert_eq!(
            store
                .get_run_state("r1")?
                .and_then(|state| state.continuation)
                .and_then(|state| state.blocker_audit)
                .map(|audit| audit.consecutive_turns),
            Some(3)
        );

        assert!(store.request_pause("r1")?);
        let paused = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(paused.run.status, TaskRunStatus::Paused);
        assert_eq!(
            paused
                .continuation
                .as_ref()
                .and_then(|state| state.pause.as_ref())
                .map(|pause| pause.reason),
            Some(RunPauseReason::User)
        );

        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);
        assert!(
            store
                .get_run_state("r1")?
                .and_then(|state| state.continuation)
                .and_then(|state| state.blocker_audit)
                .is_none()
        );
        assert!(store.request_cancel("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn blocker_audit_resets_on_progress_and_distinguishes_error_fingerprints()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;

        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "stalled-before-progress",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        let stalled = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "stalled-before-progress",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(
            stalled
                .blocker_audit
                .as_ref()
                .map(|audit| audit.consecutive_turns),
            Some(1)
        );

        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "progress-turn",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("code_reviewer"),
            Some("started review"),
        )?;
        let progressed = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "progress-turn",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert!(progressed.blocker_audit.is_none());

        for (turn_id, fingerprint, expected) in [
            ("provider-a", "provider_a", 1_u32),
            ("provider-b-1", "provider_b", 1_u32),
            ("provider-b-2", "provider_b", 2_u32),
            ("provider-b-3", "provider_b", 3_u32),
        ] {
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            let state = store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some(fingerprint),
                },
            )?;
            let audit = state.blocker_audit.ok_or_else(|| {
                StoreError::InvalidPlan(format!("blocker audit missing for {turn_id}"))
            })?;
            assert_eq!(audit.fingerprint, format!("error:{fingerprint}"));
            assert_eq!(audit.consecutive_turns, expected);
        }
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_retry_keeps_dependency_blockers_derived() -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "retry-run",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "retry a failed dependency chain",
            "",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "retry-plan".to_string(),
            run_id: "retry-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("retry a failed dependency chain"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![
                PlanTask {
                    id: "upstream".to_string(),
                    agent_role: "implementer".to_string(),
                    max_retries: 2,
                    ..sample_task_body("upstream")
                },
                PlanTask {
                    id: "child".to_string(),
                    agent_role: "reviewer".to_string(),
                    depends_on: vec!["upstream".to_string()],
                    ..sample_task_body("child")
                },
                PlanTask {
                    id: "acceptance-blocked".to_string(),
                    agent_role: "reviewer".to_string(),
                    ..sample_task_body("acceptance-blocked")
                },
            ],
        })?;
        store.transition_run("retry-run", TaskRunStatus::Running)?;
        store.set_task_status(
            "retry-run",
            "upstream",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("implementer"),
            Some("execution failed"),
        )?;
        store.set_task_status(
            "retry-run",
            "acceptance-blocked",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("review needs fix; awaiting explicit retry"),
        )?;
        store.transition_run("retry-run", TaskRunStatus::Failed)?;

        let before = store.load_runtime_plan_snapshot("retry-run")?;
        let expected_task = before
            .tasks
            .iter()
            .find(|task| task.spec.id == "upstream")
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        let mut expected = before.clone();
        let expected_outcome =
            echo_agent::tasks::retry_runtime_task(&mut expected, &expected_task, before.revision)?;
        assert_eq!(
            expected_outcome,
            echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count: 1 }
        );
        assert_eq!(store.retry_blocked_task("retry-run", "upstream")?, 1);
        let stored = store.load_runtime_plan_snapshot("retry-run")?;
        assert_eq!(stored.revision, expected.revision);
        assert_eq!(stored.tasks, expected.tasks);
        let child = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "child")
            .ok_or_else(|| StoreError::TaskNotFound("child".to_string()))?;
        assert_eq!(child.status, TodoStatus::Pending);
        let independent = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "acceptance-blocked")
            .ok_or_else(|| StoreError::TaskNotFound("acceptance-blocked".to_string()))?;
        assert_eq!(independent.status, TodoStatus::Blocked);
        let upstream = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "upstream")
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        assert_eq!(upstream.owner_agent.as_deref(), Some("implementer"));
        assert_eq!(
            store
                .get_run("retry-run")?
                .ok_or_else(|| StoreError::RunNotFound("retry-run".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            last_frame_event_types(&store, "retry-run").map_err(StoreError::InvalidPlan)?,
            ["task_status_changed", "note", "run_status_changed"]
        );
        assert!(store.list_events("retry-run", 0)?.iter().all(|event| {
            event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|summary| !summary.contains("unblocked after retrying upstream"))
        }));
        Ok(())
    }

    #[test]
    fn boot_recovery_pauses_run_and_preserves_completed_tasks() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            Some("verified"),
        )?;

        assert_eq!(s.recover_incomplete()?, 1);
        let run = s
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let todos = s.list_todos("r1")?;
        let task = todos
            .iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, TodoStatus::Completed);
        assert_eq!(task.summary.as_deref(), Some("verified"));
        Ok(())
    }

    #[test]
    fn boot_recovery_failure_keeps_running_marker_and_is_retryable() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let event_count_before = store.list_events("r1", 0)?.len();
        store.fail_next_recovery_commit_for_test();

        assert!(matches!(
            store.recover_incomplete(),
            Err(StoreError::InvalidPlan(message)) if message == "injected recovery commit failure"
        ));
        assert_eq!(store.list_events("r1", 0)?.len(), event_count_before);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            store
                .list_todos("r1")?
                .into_iter()
                .find(|todo| todo.task_id == "t1")
                .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
                .status,
            TodoStatus::Running
        );

        assert_eq!(store.recover_incomplete()?, 1);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_repairs_projection_after_atomic_event_commit() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.fail_next_recovery_projection_for_test();

        assert_eq!(store.recover_incomplete()?, 1);
        let stale_projection = store
            .shadow
            .read_run_state("r1")
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(stale_projection.run.status, TaskRunStatus::Paused);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let authoritative = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(authoritative.run.status, TaskRunStatus::Paused);
        assert_eq!(
            authoritative
                .tasks
                .iter()
                .find(|task| task.task_id == "t1")
                .map(|task| TodoStatus::project_task_status(&task.status)),
            Some(TodoStatus::Pending)
        );
        assert_eq!(boot_recovery_event_count(&store)?, 1);

        assert_eq!(store.recover_incomplete()?, 0);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.summary.as_deref(), Some("interrupted; pending resume"));
        assert_eq!(boot_recovery_event_count(&store)?, 1);
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_orphan_turn_and_records_pause_reason() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-before-restart",
                RunTurnOrigin::User,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.account_run_turn_usage("r1", "turn-before-restart", "usage-1", 40, 2)?;

        assert_eq!(store.recover_incomplete()?, 1);
        let state = store
            .get_run_state("r1")?
            .and_then(|state| state.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert!(state.active_turn.is_none());
        assert_eq!(state.tokens_used, 42);
        assert_eq!(
            state.last_turn.as_ref().map(|turn| turn.status),
            Some(RunTurnStatus::Failed)
        );
        assert_eq!(
            state
                .last_turn
                .as_ref()
                .and_then(|turn| turn.error_fingerprint.as_deref()),
            Some("process_interrupted")
        );
        assert_eq!(
            state.pause.as_ref().map(|pause| pause.reason),
            Some(RunPauseReason::BootRecovery)
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_orphan_cell_without_replaying_it() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "orphan-cell",
            "cargo test --workspace",
            "command-hash",
            Some("turn-before-restart"),
            None,
            Some("call-before-restart"),
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "orphan-cell")
            .ok_or_else(|| StoreError::InvalidPlan("orphan cell was not rebuilt".to_string()))?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert!(!cell.is_active());
        let recovered_cell_count = store
            .list_events("r1", 0)?
            .iter()
            .filter(|event| {
                boot_recovery_payload(event)
                    .and_then(|recovery| recovery.get("cells"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|cells| {
                        cells.iter().any(|cell| {
                            json_string(cell, "cell_id").as_deref() == Some("orphan-cell")
                        })
                    })
            })
            .count();
        assert_eq!(recovered_cell_count, 1);
        assert_eq!(store.recover_incomplete()?, 0);
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_active_cell_owned_by_already_paused_run_once() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "paused-orphan-cell",
            "sleep 30",
            "hash",
            Some("turn-before-pause"),
            None,
            Some("call-before-pause"),
        )?;
        store.transition_run("r1", TaskRunStatus::Paused)?;

        assert_eq!(store.recover_incomplete()?, 1);
        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "paused-orphan-cell")
            .ok_or_else(|| StoreError::InvalidPlan("paused orphan cell missing".to_string()))?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert_eq!(store.recover_incomplete()?, 0);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .into_iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::BackgroundCellFinished
                        && json_string(&event.payload, "cell_id").as_deref()
                            == Some("paused-orphan-cell")
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn pause_request_stops_driver_and_keeps_run_resumable() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        assert!(store.request_pause("r1")?);
        assert!(token.is_cancelled());
        drop(registration);
        let run = store
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        Ok(())
    }

    #[test]
    fn active_cancel_durably_overrides_a_prior_pause_intent() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        let snapshot = store.load_runtime_plan_snapshot("r1")?;
        let task = snapshot
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let _claim = match store.claim_runtime_task("r1", &task, snapshot.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "claim unexpectedly reloaded".to_string(),
                ));
            }
        };
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        assert!(store.request_pause("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        assert!(store.request_cancel("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        assert!(
            store
                .load_runtime_plan_snapshot("r1")?
                .tasks
                .iter()
                .all(|task| task.execution.status.is_terminal())
        );
        drop(registration);
        Ok(())
    }

    #[test]
    fn cancelled_registration_drop_only_releases_in_memory_ownership() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "cancelled-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel interrupted driver",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("cancelled-driver", TaskRunStatus::Running)?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("cancelled-driver", token.clone())?;

        token.cancel();
        drop(registration);

        let run = store
            .get_run("cancelled-driver")?
            .ok_or_else(|| StoreError::RunNotFound("cancelled-driver".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Running);
        assert!(!store.is_run_active("cancelled-driver"));
        Ok(())
    }

    #[test]
    fn cancel_request_retains_driver_ownership_until_registration_drop() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "cancel-request-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel active driver without releasing its registry slot",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("cancel-request-driver", TaskRunStatus::Running)?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration =
            store.register_run_cancellation("cancel-request-driver", token.clone())?;

        assert!(store.request_cancel("cancel-request-driver")?);
        assert!(token.is_cancelled());
        assert!(store.is_run_active("cancel-request-driver"));
        assert_eq!(
            store
                .get_run("cancel-request-driver")?
                .ok_or_else(|| StoreError::RunNotFound("cancel-request-driver".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );

        drop(registration);
        assert!(!store.is_run_active("cancel-request-driver"));
        assert_eq!(
            store
                .get_run("cancel-request-driver")?
                .ok_or_else(|| StoreError::RunNotFound("cancel-request-driver".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn cancelled_nested_registration_restores_outer_driver() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "nested-cancelled-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel nested driver",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("nested-cancelled-driver", TaskRunStatus::Running)?;
        let outer_token = echo_agent::agent::CancellationToken::new();
        let outer_registration =
            store.register_run_cancellation("nested-cancelled-driver", outer_token.clone())?;
        let inner_token = outer_token.child_token();
        let inner_registration =
            store.register_run_cancellation("nested-cancelled-driver", inner_token.clone())?;

        inner_token.cancel();
        drop(inner_registration);

        let run = store
            .get_run("nested-cancelled-driver")?
            .ok_or_else(|| StoreError::RunNotFound("nested-cancelled-driver".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Running);
        assert!(store.is_run_active("nested-cancelled-driver"));
        assert!(!outer_token.is_cancelled());

        drop(outer_registration);
        assert!(!store.is_run_active("nested-cancelled-driver"));
        Ok(())
    }

    #[test]
    fn boot_recovery_requeues_orphaned_running_task() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.summary.as_deref(), Some("interrupted; pending resume"));
        Ok(())
    }

    #[test]
    fn boot_recovery_terminalizes_replay_safe_orphan_subagent_without_blocker()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let task = store
            .get_plan("r1")?
            .and_then(|plan| plan.tasks.into_iter().next())
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let runtime_task = echo_agent::tasks::Task::try_from(&task)
            .map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &runtime_task, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let execution_id = claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &execution_id,
            "subagent",
            "Task 1",
            claim.revision,
            claim.attempt,
            true,
            true,
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert!(store.active_subagent_boundaries("r1")?.is_empty());
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        let subagent = store
            .list_subagent_runs("r1")?
            .into_iter()
            .find(|run| run.subagent_run_id == execution_id)
            .ok_or_else(|| StoreError::InvalidPlan("orphan Subagent missing".to_string()))?;
        assert_eq!(subagent.status, SubagentStatus::Failed);
        let recovery = store
            .list_events("r1", 0)?
            .into_iter()
            .find_map(|event| boot_recovery_payload(&event).cloned())
            .ok_or_else(|| StoreError::InvalidPlan("recovery event missing".to_string()))?;
        assert_eq!(
            recovery
                .get("subagents")
                .and_then(serde_json::Value::as_array)
                .and_then(|subagents| {
                    subagents.iter().find(|subagent| {
                        json_string(subagent, "execution_id").as_deref()
                            == Some(execution_id.as_str())
                    })
                })
                .and_then(|subagent| subagent.get("terminal_cause"))
                .and_then(serde_json::Value::as_str),
            Some("process_interrupted")
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_reuses_completed_subagent_without_redispatch() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let runtime_task = echo_agent::tasks::Task::try_from(&task)
            .map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &runtime_task, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let execution_id = claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &execution_id,
            "subagent",
            "Task 1",
            claim.revision,
            claim.attempt,
            true,
            true,
        )?;
        let result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "durable result",
            Vec::new(),
        );
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &execution_id,
            agent_name: "subagent",
            task_subject: "Task 1",
            plan_revision: claim.revision,
            attempt: claim.attempt,
            status: "completed",
            outcome: Some(&result),
            full_output: Some("durable full output"),
            usage: None,
            dispatch_hook: true,
        })?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert_eq!(
            store.recoverable_subagent_outcome_for_attempt(
                "r1",
                "t1",
                &execution_id,
                claim.revision,
                claim.attempt,
            )?,
            Some(RecoverableSubagentOutcome {
                outcome: result,
                full_output: "durable full output".to_string(),
            })
        );
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(
            todo.summary.as_deref(),
            Some("Subagent completed before interruption; pending review")
        );
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        Ok(())
    }

    #[test]
    fn mutating_in_doubt_subagent_blocks_resume_until_user_decides() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise mutating recovery".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.record_subagent_assigned(
            "r1", "t1", "t1:1", "subagent", "Task 1", 1, 1, false, true,
        )?;
        store.record_tool_started("r1", "t1", "t1:1", "call-write", "write_file", false)?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert!(store.active_subagent_boundaries("r1")?.is_empty());
        let blockers = store.list_recovery_blockers("r1")?;
        assert_eq!(blockers.len(), 1);
        assert_eq!(
            blockers.first().and_then(|b| b.call_id.as_deref()),
            Some("call-write")
        );
        assert!(matches!(
            store.resume_task_run("r1"),
            Err(StoreError::RecoveryBlocked { .. })
        ));

        store.resolve_recovery_task("r1", "t1", RecoveryDecision::Retry)?;
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            ["recovery_resolved", "task_status_changed"]
        );
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);
        Ok(())
    }

    #[test]
    fn canonical_recovery_skip_commits_revision_and_resolution_in_one_batch()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise canonical recovery skip".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.record_subagent_assigned(
            "r1", "t1", "t1:1", "subagent", "Task 1", 2, 1, false, true,
        )?;
        store.record_tool_started("r1", "t1", "t1:1", "call-write", "write_file", false)?;
        assert_eq!(store.recover_incomplete()?, 1);
        let before = store.load_runtime_plan_snapshot("r1")?;
        let expected_revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPlan("test plan revision overflow".to_string()))?;

        store.resolve_recovery_task("r1", "t1", RecoveryDecision::Skip)?;

        let after = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(after.revision, expected_revision);
        let skipped = after
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            skipped.execution.status,
            echo_agent::tasks::TaskStatus::Skipped
        );
        assert!(skipped.execution.claim.is_none());
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            [
                "plan_revision_committed",
                "recovery_resolved",
                "task_skipped",
            ]
        );
        Ok(())
    }

    #[test]
    fn tool_failure_boundary_persists_recovery_contract() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let failure = echo_agent::tools::ToolFailure::new(
            echo_agent::tools::ToolFailureCategory::PartialSideEffect,
        )
        .with_postcondition("verify target hash");

        store.record_tool_started("r1", "t1", "t1:1", "call-1", "write_file", false)?;
        store.record_tool_finished(
            "r1",
            "t1",
            "t1:1",
            "call-1",
            "write_file",
            false,
            "write interrupted",
            Some(&failure),
        )?;

        let event = store
            .list_events("r1", 0)?
            .into_iter()
            .find(|event| event.event_type == RuntimeEventKind::ToolFailed)
            .ok_or_else(|| StoreError::TaskNotFound("tool failure event".to_string()))?;
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("category"))
                .and_then(serde_json::Value::as_str),
            Some("partial_side_effect")
        );
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("postcondition"))
                .and_then(serde_json::Value::as_str),
            Some("verify target hash")
        );
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_running() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?; // run "r1" in conversation "c1" is now Running.
        let found = s
            .find_in_progress_run_by_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("in-progress run for c1".to_string()))?;
        assert_eq!(found.run_id, "r1");
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_paused() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.transition_run("r1", TaskRunStatus::Paused)?;
        let found = s
            .find_in_progress_run_by_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("paused run for c1".to_string()))?;
        assert_eq!(found.run_id, "r1");
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_none_for_completed() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let found = s.find_in_progress_run_by_conversation("c1")?;
        assert!(found.is_none());
        Ok(())
    }

    #[test]
    fn task_update_inserts_task_and_commits_one_revision() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let t2 = PlanTask {
            id: "t2".into(),
            title: "Second task".into(),
            description: "implement the second task".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            depends_on: vec!["t1".into()],
            ..Default::default()
        };
        let before = s.list_events("r1", 0)?.len();
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "new implementation dependency".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("t1".to_string()),
                    task: t2.spec(),
                }],
            },
        )?;

        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].id, "t1");
        assert_eq!(plan.tasks[1].id, "t2");
        let evs = s.list_events("r1", 0)?;
        assert_eq!(evs.len(), before + 1);
        assert_eq!(
            evs.last().map(|event| event.event_type),
            Some(RuntimeEventKind::PlanRevisionCommitted)
        );
        Ok(())
    }

    #[test]
    fn reorder_keeps_plan_todos_and_framework_graph_in_one_order() -> Result<(), StoreError> {
        let store = TaskRuntimeStore::new_in_memory()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        store.create_run(
            "reorder-run",
            "ws",
            "c-reorder",
            "m-reorder",
            DomainProfile::General,
            "reorder",
            "",
            AttendedMode::Attended,
        )?;
        let plan = TaskPlan {
            plan_id: "reorder-plan".to_string(),
            run_id: "reorder-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("reorder"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![
                PlanTask {
                    id: "first".to_string(),
                    title: "First".to_string(),
                    sort_order: 0,
                    ..PlanTask::default()
                },
                PlanTask {
                    id: "second".to_string(),
                    title: "Second".to_string(),
                    sort_order: 1,
                    ..PlanTask::default()
                },
            ],
        };
        store.attach_plan_for_test(&plan)?;
        store.transition_run("reorder-run", TaskRunStatus::Running)?;

        let reordered = store.apply_task_patch_for_test(
            "reorder-run",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "surface parity reorder".to_string(),
                operations: vec![TaskUpdateOperation::Reorder {
                    task_ids: vec!["second".to_string(), "first".to_string()],
                }],
            },
        )?;
        assert_eq!(
            reordered
                .tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        assert_eq!(
            reordered
                .tasks
                .iter()
                .map(|task| task.sort_order)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        let todos = store.list_todos("reorder-run")?;
        assert_eq!(
            todos
                .iter()
                .map(|todo| todo.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        let graph = store
            .load_revisioned_task_graph("reorder-run")?
            .ok_or_else(|| StoreError::PlanNotFound("reorder-run".to_string()))?;
        assert_eq!(
            graph
                .snapshot
                .tasks
                .iter()
                .map(|task| task.spec.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        Ok(())
    }

    #[test]
    fn task_update_rejects_missing_run() -> std::result::Result<(), String> {
        let s = TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?;
        let err = s
            .apply_task_patch_for_test(
                "missing-run",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "invalid".to_string(),
                    operations: vec![TaskUpdateOperation::Reorder {
                        task_ids: Vec::new(),
                    }],
                },
            )
            .err()
            .ok_or_else(|| "task_update unexpectedly succeeded without a run".to_string())?;
        assert!(matches!(err, StoreError::RunNotFound(run_id) if run_id == "missing-run"));
        Ok(())
    }

    #[test]
    fn task_update_rejects_stale_revision_without_appending_event() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let before = s.list_events("r1", 0)?.len();
        let error = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 0,
                    reason: "stale edit".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "t1".to_string(),
                    }],
                },
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("stale update unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(error, StoreError::PlanConflict { .. }));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_claim_and_settlement_match_framework_fields()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let before = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = before
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let mut framework_claimed = before.clone();
        let framework_claim = match echo_agent::tasks::claim_runtime_task(
            &mut framework_claimed,
            &expected_task,
            before.revision,
        )? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "framework unexpectedly rejected a fresh claim".to_string(),
                ));
            }
        };
        store.fail_next_runtime_mutation_projection_for_test();
        let stored_claim = match store.claim_runtime_task("r1", &expected_task, before.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "store unexpectedly rejected a fresh claim".to_string(),
                ));
            }
        };
        assert_eq!(stored_claim.revision, framework_claim.revision);
        assert_eq!(stored_claim.attempt, framework_claim.attempt);
        assert_eq!(stored_claim.spec_hash, framework_claim.spec_hash);
        let framework_task = framework_claimed
            .tasks
            .iter_mut()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        framework_task.execution.claim = Some(stored_claim.clone());
        let stored_claimed = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_claimed.revision, framework_claimed.revision);
        assert_eq!(stored_claimed.tasks, framework_claimed.tasks);

        let claim_event = store
            .list_events("r1", 0)?
            .into_iter()
            .last()
            .ok_or_else(|| StoreError::InvalidPlan("claim event missing".to_string()))?;
        for field in [
            "status",
            "status_detail",
            "claim",
            "retry_count",
            "failure_fingerprint",
            "owner_agent",
            "summary",
        ] {
            assert!(claim_event.payload.get(field).is_some(), "missing {field}");
        }

        let mut framework_settled = framework_claimed;
        assert_eq!(
            echo_agent::tasks::settle_runtime_claim(
                &mut framework_settled,
                "t1",
                &stored_claim,
                echo_agent::tasks::TaskStatus::Completed,
            )?,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
        );
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_claim(
                "r1",
                "t1",
                &stored_claim,
                echo_agent::tasks::TaskStatus::Completed,
                Some("reviewed completion".to_string()),
            )?,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
        );
        let stored = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored.revision, framework_settled.revision);
        assert_eq!(stored.tasks, framework_settled.tasks);
        let terminal = stored
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert!(terminal.execution.claim.is_none());
        assert_eq!(
            terminal.execution.status,
            echo_agent::tasks::TaskStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_requeue_matches_framework_and_rejects_aba_claim()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let first_claim = match store.claim_runtime_task("r1", &expected_task, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh requeue claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let claimed = store.load_runtime_plan_snapshot("r1")?;
        let request = echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
            failure_fingerprint: Some("compile-fingerprint".to_string()),
            error: "cargo check failed".to_string(),
            exhaustion: echo_agent::tasks::RuntimeRetryExhaustion::Failed,
        };
        let mut framework_requeued = claimed.clone();
        let framework_resolution = echo_agent::tasks::settle_runtime_resolution(
            &mut framework_requeued,
            "t1",
            &first_claim,
            request.clone(),
        )?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &first_claim,
                request,
                RuntimeTaskProductSettlement {
                    summary: Some("retry after cargo check".to_string()),
                    ..RuntimeTaskProductSettlement::default()
                },
            )?,
            framework_resolution
        );
        let stored_requeued = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_requeued.revision, framework_requeued.revision);
        assert_eq!(stored_requeued.tasks, framework_requeued.tasks);
        let requeued = framework_requeued
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            requeued.execution.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(requeued.execution.retry_count, 1);
        assert_eq!(
            requeued.execution.failure_fingerprint.as_deref(),
            Some("compile-fingerprint")
        );
        assert!(requeued.execution.claim.is_none());

        let second_claim = match store.claim_runtime_task("r1", requeued, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "requeued task unexpectedly required reload".to_string(),
                ));
            }
        };
        assert_ne!(first_claim.claim_id, second_claim.claim_id);
        let completed_summary = TaskExecutionSummary {
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            subagent_name: "subagent".to_string(),
            outcome: SubagentOutcome::terminal(
                SubagentStatus::Completed,
                "new physical claim result",
                Vec::new(),
            ),
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        };
        let current_review = ReviewResult {
            id: "review-new-claim".to_string(),
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::Pass,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &second_claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Completed,
                RuntimeTaskProductSettlement {
                    summary: Some("new physical claim result".to_string()),
                    execution_summary: Some(completed_summary.clone()),
                    review: Some(current_review),
                    diagnostic_note: Some("accepted new claim review".to_string()),
                    typed_terminal: None,
                },
            )?,
            echo_agent::tasks::RuntimeTaskResolution::Completed
        );
        let stale_summary = TaskExecutionSummary {
            outcome: SubagentOutcome::terminal(
                SubagentStatus::Completed,
                "stale physical claim result",
                Vec::new(),
            ),
            ..completed_summary
        };
        let stale_review = ReviewResult {
            id: "review-stale-claim".to_string(),
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::NeedsFix,
            issues: Vec::new(),
            failure_fingerprint: Some("stale-fingerprint".to_string()),
            created_fix_task_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &first_claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Completed,
                RuntimeTaskProductSettlement {
                    summary: Some("stale physical claim result".to_string()),
                    execution_summary: Some(stale_summary),
                    review: Some(stale_review),
                    diagnostic_note: Some("stale claim review note".to_string()),
                    typed_terminal: None,
                },
            )?,
            echo_agent::tasks::RuntimeTaskResolution::Superseded
        );
        assert_eq!(
            store
                .get_summary("r1", "t1")?
                .ok_or_else(|| StoreError::TaskNotFound("t1 summary".to_string()))?
                .outcome
                .summary,
            "new physical claim result"
        );
        let reviews = store.list_reviews("r1", "t1")?;
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            reviews.first().map(|review| review.id.as_str()),
            Some("review-new-claim")
        );
        let notes = store.list_events("r1", 0)?;
        assert!(notes.iter().any(|event| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                == Some("accepted new claim review")
        }));
        assert!(!notes.iter().any(|event| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                == Some("stale claim review note")
        }));
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_pause_interruption_is_lossless() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let claim = match store.claim_runtime_task("r1", &expected_task, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh interruption claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let claimed = store.load_runtime_plan_snapshot("r1")?;
        let mut framework_paused = claimed.clone();
        let disposition = echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
            reason: "user requested pause".to_string(),
        };
        let framework_outcome = echo_agent::tasks::settle_runtime_interruption(
            &mut framework_paused,
            claimed.revision,
            disposition.clone(),
        )?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_interruption("r1", claimed.revision, disposition)?,
            framework_outcome
        );
        let stored_paused = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_paused.revision, framework_paused.revision);
        assert!(!store.runtime_task_claim_is_current("r1", "t1", &claim)?);
        let paused = framework_paused
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            paused.execution.status,
            echo_agent::tasks::TaskStatus::Paused("user requested pause".to_string())
        );
        assert!(paused.execution.claim.is_none());
        let retry_count_before_resume = paused.execution.retry_count;
        let persisted = stored_paused
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            persisted.execution.status,
            echo_agent::tasks::TaskStatus::Paused("user requested pause".to_string())
        );
        assert_eq!(persisted.execution.retry_count, retry_count_before_resume);
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.resume_task_run("r1")?;
        let resumed = store
            .load_runtime_plan_snapshot("r1")?
            .tasks
            .into_iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            resumed.execution.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(resumed.execution.retry_count, retry_count_before_resume);
        assert!(resumed.execution.claim.is_none());
        Ok(())
    }

    #[test]
    fn recovered_result_requires_exact_physical_claim_after_pause_resume() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let old_claim = match store.claim_runtime_task("r1", &expected, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "initial claim reloaded".to_string(),
                ));
            }
        };
        let old_execution_id = old_claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &old_execution_id,
            "subagent",
            "Review runtime",
            old_claim.revision,
            old_claim.attempt,
            true,
            true,
        )?;
        store.settle_runtime_task_interruption(
            "r1",
            initial.revision,
            echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
                reason: "pause for exact recovery test".to_string(),
            },
        )?;
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.resume_task_run("r1")?;
        let resumed = store.load_runtime_plan_snapshot("r1")?;
        let resumed_task = resumed
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let new_claim = match store.claim_runtime_task("r1", &resumed_task, resumed.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "resumed claim reloaded".to_string(),
                ));
            }
        };
        assert_eq!(new_claim.attempt, old_claim.attempt);
        let new_execution_id = new_claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &new_execution_id,
            "subagent",
            "Review runtime",
            new_claim.revision,
            new_claim.attempt,
            true,
            true,
        )?;
        let stale_result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "late result from old physical claim",
            Vec::new(),
        );
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &old_execution_id,
            agent_name: "subagent",
            task_subject: "Review runtime",
            plan_revision: old_claim.revision,
            attempt: old_claim.attempt,
            status: "completed",
            outcome: Some(&stale_result),
            full_output: Some("late result from old physical claim"),
            usage: None,
            dispatch_hook: true,
        })?;

        assert!(
            store
                .recoverable_subagent_outcome_for_attempt(
                    "r1",
                    "t1",
                    &new_execution_id,
                    new_claim.revision,
                    new_claim.attempt,
                )?
                .is_none()
        );
        assert!(
            store
                .recoverable_subagent_outcome_for_attempt(
                    "r1",
                    "t1",
                    &old_execution_id,
                    old_claim.revision,
                    old_claim.attempt,
                )?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_revision_reload_wins_task_update_race() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .try_into()
            .map_err(StoreError::InvalidPlan)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "skip before stale dispatch claims task".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;

        let outcome = store.claim_runtime_task("r1", &expected, 1)?;

        assert_eq!(
            outcome,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot
        );
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, echo_agent::tasks::TaskStatus::Skipped);
        assert!(task.claim.is_none());
        Ok(())
    }

    #[test]
    fn stale_claim_cannot_overwrite_cancelled_task() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .try_into()
            .map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &expected, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Skipped,
            None,
            Some("cancelled by user"),
        )?;

        let outcome = store.settle_runtime_task_claim(
            "r1",
            "t1",
            &claim,
            echo_agent::tasks::TaskStatus::Completed,
            Some("stale completion".to_string()),
        )?;

        assert_eq!(
            outcome,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Superseded
        );
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, echo_agent::tasks::TaskStatus::Skipped);
        Ok(())
    }

    #[test]
    fn patched_spec_uses_new_execution_identity_without_retry_bump() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let original = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let original_runtime = echo_agent::tasks::Task::try_from(&original)
            .map_err(StoreError::InvalidPlan)?;
        let old_claim = echo_agent::tasks::TaskClaim::new(
            1,
            1,
            original_runtime
                .spec
                .stable_hash()
                .map_err(StoreError::InvalidPlan)?,
        );
        let old_execution_id = old_claim.execution_id("r1", &original.id);
        let durable_result = SubagentOutcome::terminal(
            SubagentStatus::Completed,
            "old spec result",
            Vec::new(),
        );
        store.record_subagent_assigned(
            "r1",
            "t1",
            &old_execution_id,
            "code_reviewer",
            &original.title,
            old_claim.revision,
            old_claim.attempt,
            true,
            true,
        )?;
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &old_execution_id,
            agent_name: "code_reviewer",
            task_subject: &original.title,
            plan_revision: old_claim.revision,
            attempt: old_claim.attempt,
            status: "completed",
            outcome: Some(&durable_result),
            full_output: Some("old spec full output"),
            usage: None,
            dispatch_hook: true,
        })?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("code_reviewer"),
            Some("requires a revised contract"),
        )?;
        let patched = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change blocked task contract".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        description: Some("review the revised runtime contract".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        let patched_task = patched
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(patched_task.retry_count, 0);
        let patched_runtime = echo_agent::tasks::Task::try_from(&patched_task)
            .map_err(StoreError::InvalidPlan)?;
        let new_claim = match store.claim_runtime_task("r1", &patched_runtime, patched.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "patched task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let new_execution_id = new_claim.execution_id("r1", &patched_task.id);

        assert_ne!(old_execution_id, new_execution_id);
        assert_ne!(old_claim.spec_hash, new_claim.spec_hash);
        assert!(
            store
                .recoverable_subagent_outcome_for_attempt(
                    "r1",
                    "t1",
                    &old_execution_id,
                    old_claim.revision,
                    old_claim.attempt,
                )?
                .is_some()
        );
        assert!(
            store
                .recoverable_subagent_outcome_for_attempt(
                    "r1",
                    "t1",
                    &new_execution_id,
                    new_claim.revision,
                    new_claim.attempt,
                )?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn task_update_skip_preserves_spec_and_updates_execution() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "task no longer required".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].status, echo_agent::tasks::TaskStatus::Skipped);
        Ok(())
    }

    #[test]
    fn task_update_update_requeues_blocked_task() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("needs a clearer brief"),
        )?;
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "clarify the blocked task".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        description: Some("Review the clarified runtime boundary".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks[0].status, echo_agent::tasks::TaskStatus::Pending);
        assert_eq!(
            plan.tasks[0].description,
            "Review the clarified runtime boundary"
        );
        Ok(())
    }

    #[test]
    fn completion_gate_rechecks_latest_plan_revision() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let persist_summary = |task_id: &str| {
            s.put_summary(&TaskExecutionSummary {
                run_id: "r1".to_string(),
                task_id: task_id.to_string(),
                subagent_name: "explorer".to_string(),
                outcome: SubagentOutcome::terminal(
                    SubagentStatus::Completed,
                    "verified task result",
                    Vec::new(),
                ),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: Utc::now(),
            })
        };
        persist_summary("t1")?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            None,
        )?;
        let follow_up = PlanTask {
            id: "t2".to_string(),
            title: "Verify follow-up".to_string(),
            description: "Verify evidence discovered by t1".to_string(),
            kind: PlanTaskKind::Verification,
            agent_role: "explorer".to_string(),
            depends_on: vec!["t1".to_string()],
            ..Default::default()
        };
        s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "new evidence requires verification".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("t1".to_string()),
                    task: follow_up.spec(),
                }],
            },
        )?;
        assert!(!s.complete_run_if_quiescent("r1")?);
        persist_summary("t2")?;
        s.set_task_status(
            "r1",
            "t2",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            None,
        )?;
        assert!(s.complete_run_if_quiescent("r1")?);
        assert_eq!(
            s.get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn task_update_rejects_running_task_contract_change() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let result = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change active ownership".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        files: Some(vec!["src/new-owner.rs".to_string()]),
                        ..Default::default()
                    },
                }],
            },
        );
        assert!(matches!(result, Err(StoreError::InvalidPlan(_))));
        Ok(())
    }

    // ── review #4: intent-visible tests that validation fires on the FILE
    //    authority path (not just transitively). Each asserts the error is
    //    returned AND no event line was appended — proving the file-path
    //    validation branch rejected before writing. ──────────────────────

    /// `transition_run` rejects an illegal transition on the file path and
    /// appends no event. (Completed → Running is always illegal.)
    #[test]
    fn file_path_rejects_illegal_transition_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        s.transition_run("r1", TaskRunStatus::Running)?;
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let before = s.list_events("r1", 0)?.len();
        let err = s
            .transition_run("r1", TaskRunStatus::Running)
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan(
                    "illegal file transition unexpectedly succeeded".to_string(),
                )
            })?;
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event appended — the file-path validation rejected before writing.
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    /// `task_update` rejects a dependency cycle and appends no revision event.
    #[test]
    fn file_path_rejects_dependency_cycle_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        s.attach_plan_for_test(&TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                PlanTask {
                    id: "t1".into(),
                    depends_on: Vec::new(),
                    ..sample_task_body("t1")
                },
                PlanTask {
                    id: "t2".into(),
                    depends_on: vec!["t1".into()],
                    ..sample_task_body("t2")
                },
            ],
        })?;
        let before = s.list_events("r1", 0)?.len();
        // Now make t1 depend on t2 → cycle.
        let err = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "introduce invalid cycle".to_string(),
                    operations: vec![TaskUpdateOperation::Update {
                        task_id: "t1".to_string(),
                        patch: TaskPatch {
                            depends_on: Some(vec!["t2".into()]),
                            ..Default::default()
                        },
                    }],
                },
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("dependency cycle unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::InvalidPlan(_)));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    /// `set_task_status` rejects an unknown task on the file path and appends
    /// no event.
    #[test]
    fn file_path_rejects_unknown_task_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let before = s.list_events("r1", 0)?.len();
        let err = s
            .set_task_status(
                "r1",
                "nope",
                echo_agent::tasks::TaskStatus::Running,
                None,
                None,
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("unknown task unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::TaskNotFound(_)));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    #[tokio::test]
    async fn generation_rebind_rejects_active_operation_then_isolates_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            &first_root,
        )?);
        let operation = store.lease_active_workspace_generation()?;
        assert!(
            matches!(
                store.rebind_shadow_root(&second_root, "workspace-b").await,
                Err(StoreError::WorkspaceTransitionBusy {
                    active_operations: 1
                })
            ),
            "workspace transition must fail fast while a generation lease is active"
        );
        drop(operation);
        store
            .rebind_shadow_root(&second_root, "workspace-b")
            .await?;

        store.create_run_for_active_workspace(
            "run-b",
            "conversation-b",
            "message-b",
            DomainProfile::General,
            "generation isolation",
            "task",
            AttendedMode::Attended,
        )?;
        assert!(!first_root.join("run-b").exists());
        assert!(temp.path().join("second/run-b/events.jsonl").is_file());
        assert_eq!(
            store.get_run("run-b")?.map(|run| run.workspace_id),
            Some("workspace-b".to_string())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_transition_rejects_operations_without_blocking_single_thread_runtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            &first_root,
        )?);
        let transition = store.begin_workspace_transition().await?;

        assert!(matches!(
            store.create_run_for_active_workspace(
                "run-b",
                "conversation-b",
                "message-b",
                DomainProfile::General,
                "generation admission",
                "task",
                AttendedMode::Attended,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.transition_run("run-b", TaskRunStatus::Running),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.set_task_status(
                "run-b",
                "task-b",
                echo_agent::tasks::TaskStatus::Running,
                None,
                None
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.note("run-b", None, "must not reach the old root"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.record_subagent_assigned(
                "run-b",
                "task-b",
                "execution-b",
                "subagent-b",
                "Task B",
                1,
                1,
                true,
                true,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.record_tool_started(
                "run-b",
                "task-b",
                "execution-b",
                "call-b",
                "read_file",
                true,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.get_run("run-b"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.lease_active_workspace_generation(),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));

        transition.rebind_shadow_root(&second_root, "workspace-b")?;
        assert!(matches!(
            store.note("run-b", None, "must wait for generation publication"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        drop(transition);

        store.create_run_for_active_workspace(
            "run-b",
            "conversation-b",
            "message-b",
            DomainProfile::General,
            "generation admission",
            "task",
            AttendedMode::Attended,
        )?;

        assert!(!first_root.join("run-b").exists());
        assert!(second_root.join("run-b/events.jsonl").is_file());
        assert_eq!(store.active_workspace_id(), "workspace-b");
        Ok(())
    }

    #[tokio::test]
    async fn failed_generation_rebind_keeps_previous_root_and_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let invalid_root = temp.path().join("not-a-directory");
        std::fs::write(&invalid_root, "file")?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&first_root)?;

        assert!(
            store
                .rebind_shadow_root(&invalid_root, "workspace-b")
                .await
                .is_err()
        );
        assert_eq!(store.active_workspace_id(), "test");
        store.create_run_for_active_workspace(
            "run-a",
            "conversation-a",
            "message-a",
            DomainProfile::General,
            "failed rebind",
            "task",
            AttendedMode::Attended,
        )?;
        assert!(first_root.join("run-a/events.jsonl").is_file());
        assert_eq!(
            store.get_run("run-a")?.map(|run| run.workspace_id),
            Some("test".to_string())
        );
        Ok(())
    }

    #[test]
    fn conversation_removal_deletes_only_its_task_runs() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))?;
        for (run_id, conversation_id) in [
            ("conversation-run-a", "conversation-delete"),
            ("conversation-run-b", "conversation-delete"),
            ("retained-run", "conversation-keep"),
        ] {
            store.create_run(
                run_id,
                "workspace",
                conversation_id,
                "message",
                DomainProfile::General,
                run_id,
                "chat",
                AttendedMode::Attended,
            )?;
        }

        store.remove_conversation("conversation-delete")?;

        assert!(store.get_run("conversation-run-a")?.is_none());
        assert!(store.get_run("conversation-run-b")?.is_none());
        assert!(store.get_run("retained-run")?.is_some());
        assert!(
            store
                .list_runs_for_conversation("conversation-delete")?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn degraded_conversation_delete_clears_local_state_before_same_id_recreate()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))?;
        store.create_run(
            "degraded-delete",
            "workspace",
            "conversation-delete",
            "message",
            DomainProfile::General,
            "degraded delete",
            "chat",
            AttendedMode::Attended,
        )?;
        store
            .task_cancel_tokens
            .lock()
            .map_err(|_| "task token lock poisoned")?
            .insert(
                "degraded-delete::task".to_string(),
                echo_agent::agent::CancellationToken::new(),
            );
        assert!(store.plan_locks.contains_key("degraded-delete"));
        store.shadow.fail_root_sync_on_call_for_test(2);
        assert!(matches!(
            store.remove_conversation("conversation-delete"),
            Err(StoreError::Shadow(
                super::super::file_shadow::ShadowError::CommittedDeletionDegraded { .. }
            ))
        ));
        assert!(!store.plan_locks.contains_key("degraded-delete"));
        assert!(
            !store
                .task_cancel_tokens
                .lock()
                .map_err(|_| "task token lock poisoned")?
                .contains_key("degraded-delete::task")
        );
        assert!(store.get_run("degraded-delete")?.is_none());
        let recreated = store.create_run(
            "degraded-delete",
            "workspace",
            "conversation-new",
            "message-new",
            DomainProfile::General,
            "recreated",
            "chat",
            AttendedMode::Attended,
        )?;
        assert_eq!(recreated.conversation_id, "conversation-new");
        Ok(())
    }

    #[test]
    fn conversation_removal_fails_closed_while_a_driver_is_active()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            temp.path().join("tasks"),
        )?);
        store.create_run(
            "active-delete-run",
            "workspace",
            "conversation-delete",
            "message",
            DomainProfile::General,
            "active run",
            "chat",
            AttendedMode::Attended,
        )?;
        let registration = store.register_run_cancellation(
            "active-delete-run",
            echo_agent::agent::CancellationToken::new(),
        )?;

        assert!(matches!(
            store.remove_conversation("conversation-delete"),
            Err(StoreError::ConversationHasActiveRuns { .. })
        ));
        assert!(store.get_run("active-delete-run")?.is_some());
        drop(registration);
        store.remove_conversation("conversation-delete")?;
        assert!(store.get_run("active-delete-run")?.is_none());
        Ok(())
    }

    #[test]
    fn checkpoint_state_is_canonical_byte_equivalent_to_full_replay_and_repairs_corruption()
    -> Result<(), String> {
        let (temp, store, run_id) = seed_public_state_fixture(256)?;
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| "full replay sequence is unavailable".to_string())?;
        let full = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| error.to_string())?
            .run_state_with_sequence(journal_sequence);
        let warm = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "checkpoint-backed state is missing".to_string())?;
        let full_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&full)
            .map_err(|error| error.to_string())?;
        let warm_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&warm)
            .map_err(|error| error.to_string())?;
        assert_eq!(warm_bytes, full_bytes);

        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let repaired = reopened
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired state is missing".to_string())?;
        let repaired_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&repaired)
            .map_err(|error| error.to_string())?;
        assert_eq!(repaired_bytes, full_bytes);
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
        let decoded = FileCheckpointStore::<serde_json::Value>::open(&checkpoint_path)
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired checkpoint is missing".to_string())?;
        assert_eq!(decoded.sequence, 256);
        assert!(decoded.state.is_object());
        Ok(())
    }

    #[test]
    #[ignore = "LH5 release performance gate; run with --release --ignored --nocapture"]
    fn performance_public_get_run_state_10k_100k_release_gate() -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("LH5 performance gate must run with --release".to_string());
        }
        let (_ten_temp, ten_store, ten_run) = seed_public_state_fixture(10_000)?;
        let (hundred_temp, hundred_store, hundred_run) = seed_public_state_fixture(100_000)?;

        let mut ten_samples = Vec::with_capacity(5);
        let mut hundred_samples = Vec::with_capacity(5);
        for _ in 0..5 {
            let started = std::time::Instant::now();
            assert!(
                ten_store
                    .get_run_state(&ten_run)
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            ten_samples.push(started.elapsed());

            let started = std::time::Instant::now();
            assert!(
                hundred_store
                    .get_run_state(&hundred_run)
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            hundred_samples.push(started.elapsed());
        }
        let ten_median = median_duration(&mut ten_samples)
            .ok_or_else(|| "10k sample set is empty".to_string())?;
        let hundred_median = median_duration(&mut hundred_samples)
            .ok_or_else(|| "100k sample set is empty".to_string())?;
        let ten_worst = ten_samples.iter().copied().max().unwrap_or_default();
        let hundred_worst = hundred_samples.iter().copied().max().unwrap_or_default();

        let append_started = std::time::Instant::now();
        hundred_store
            .note(&hundred_run, None, "one appended public state event")
            .map_err(|error| error.to_string())?;
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let append_read = append_started.elapsed();

        let checkpoint_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("checkpoint.json");
        let events_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("events.jsonl");
        drop(hundred_store);
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        let hundred_store =
            TaskRuntimeStore::new_in_memory_with_shadow_root(hundred_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        let rebuild_started = std::time::Instant::now();
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let corrupt_rebuild = rebuild_started.elapsed();
        let warm_started = std::time::Instant::now();
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let repaired_warm = warm_started.elapsed();
        let checkpoint_bytes = std::fs::metadata(&checkpoint_path)
            .map_err(|error| error.to_string())?
            .len();
        let event_bytes = std::fs::metadata(&events_path)
            .map_err(|error| error.to_string())?
            .len();

        println!(
            "{}",
            serde_json::json!({
                "ten_k_median_ms": ten_median.as_secs_f64() * 1_000.0,
                "ten_k_worst_ms": ten_worst.as_secs_f64() * 1_000.0,
                "hundred_k_median_ms": hundred_median.as_secs_f64() * 1_000.0,
                "hundred_k_worst_ms": hundred_worst.as_secs_f64() * 1_000.0,
                "append_read_ms": append_read.as_secs_f64() * 1_000.0,
                "corrupt_rebuild_ms": corrupt_rebuild.as_secs_f64() * 1_000.0,
                "repaired_warm_ms": repaired_warm.as_secs_f64() * 1_000.0,
                "checkpoint_bytes": checkpoint_bytes,
                "event_bytes": event_bytes,
            })
        );
        assert!(ten_median <= std::time::Duration::from_millis(2));
        assert!(hundred_median <= std::time::Duration::from_millis(2));
        assert!(hundred_median.as_nanos() <= ten_median.as_nanos().saturating_mul(2).max(1));
        assert!(append_read <= std::time::Duration::from_millis(50));
        assert!(corrupt_rebuild <= std::time::Duration::from_secs(5));
        assert!(repaired_warm <= std::time::Duration::from_millis(2));
        assert!(checkpoint_bytes <= 256 * 1024);
        assert!(checkpoint_bytes.saturating_mul(20) < event_bytes);
        Ok(())
    }

    #[test]
    #[ignore = "final 10k/100k scale gate; run explicitly after the refactor"]
    fn production_todo_artifact_review_summary_completion_queries_are_bounded_at_10k_and_100k()
    -> Result<(), String> {
        let (_ten_temp, ten_store, ten_run) = seed_public_query_fixture(10_000)?;
        let (_hundred_temp, hundred_store, hundred_run) = seed_public_query_fixture(100_000)?;
        let mut ten_samples = Vec::with_capacity(5);
        let mut hundred_samples = Vec::with_capacity(5);
        for _ in 0..5 {
            ten_samples.push(time_public_queries(&ten_store, &ten_run)?);
            hundred_samples.push(time_public_queries(&hundred_store, &hundred_run)?);
        }
        let ten_median = median_duration(&mut ten_samples)
            .ok_or_else(|| "10k query sample set is empty".to_string())?;
        let hundred_median = median_duration(&mut hundred_samples)
            .ok_or_else(|| "100k query sample set is empty".to_string())?;
        let ratio_budget = ten_median
            .saturating_mul(4)
            .max(std::time::Duration::from_millis(25));
        assert!(hundred_median <= std::time::Duration::from_millis(200));
        assert!(hundred_median <= ratio_budget);
        Ok(())
    }

    /// FINAL performance gate for artifact and per-task review history.
    ///
    /// Run explicitly with:
    /// `cargo test -p echo-agent-app-core artifact_and_per_task_review_history_scale_at_10k_and_100k --lib --release -- --ignored --nocapture`
    #[test]
    #[ignore = "final 10k/100k performance gate; run explicitly with --release --ignored --nocapture"]
    fn artifact_and_per_task_review_history_scale_at_10k_and_100k() -> Result<(), String> {
        let (_ten_temp, ten_store, ten_run, ten_first, ten_second) =
            seed_history_scale_fixture(10_000)?;
        let (hundred_temp, hundred_store, hundred_run, hundred_first, hundred_second) =
            seed_history_scale_fixture(100_000)?;
        let (
            _ten_artifact_temp,
            _ten_artifact_store,
            _ten_artifact_run,
            ten_artifact_first,
            ten_artifact_second,
            ten_artifact_scans,
            _,
        ) = seed_artifact_scale_fixture(10_000)?;
        let (
            hundred_artifact_temp,
            hundred_artifact_store,
            hundred_artifact_run,
            hundred_artifact_first,
            hundred_artifact_second,
            hundred_artifact_scans,
            hundred_artifact_appended_bytes,
        ) = seed_artifact_scale_fixture(100_000)?;

        let mut ten_queries = Vec::with_capacity(5);
        let mut hundred_queries = Vec::with_capacity(5);
        for _ in 0..5 {
            ten_queries.push(time_history_target_queries(&ten_store, &ten_run)?);
            hundred_queries.push(time_history_target_queries(&hundred_store, &hundred_run)?);
        }
        let ten_query = median_duration(&mut ten_queries)
            .ok_or_else(|| "10k history query samples are empty".to_string())?;
        let hundred_query = median_duration(&mut hundred_queries)
            .ok_or_else(|| "100k history query samples are empty".to_string())?;
        assert_eq!(
            hundred_store
                .list_reviews(&hundred_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        let artifact_query_started = std::time::Instant::now();
        assert_eq!(
            hundred_artifact_store
                .list_artifacts(&hundred_artifact_run)
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        let hundred_artifact_query = artifact_query_started.elapsed();

        let (artifact_path, target_review_path, _) = hundred_store
            .shadow
            .history_paths_for_test(&hundred_run, "target-task")
            .map_err(|error| error.to_string())?;
        let (_, other_review_path, _) = hundred_store
            .shadow
            .history_paths_for_test(&hundred_run, "other-task")
            .map_err(|error| error.to_string())?;
        let checkpoint_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("checkpoint.json");
        let artifact_bytes = std::fs::metadata(&artifact_path)
            .map_err(|error| error.to_string())?
            .len();
        let target_review_bytes = std::fs::metadata(&target_review_path)
            .map_err(|error| error.to_string())?
            .len();
        let other_review_bytes = std::fs::metadata(&other_review_path)
            .map_err(|error| error.to_string())?
            .len();
        let checkpoint_bytes = std::fs::metadata(&checkpoint_path)
            .map_err(|error| error.to_string())?
            .len();
        let (hundred_artifact_path, _, _) = hundred_artifact_store
            .shadow
            .history_paths_for_test(&hundred_artifact_run, "target-task")
            .map_err(|error| error.to_string())?;
        let hundred_artifact_segment_bytes = std::fs::metadata(&hundred_artifact_path)
            .map_err(|error| error.to_string())?
            .len();
        println!(
            "{}",
            serde_json::json!({
                "ten_k_append_first_half_median_ms": ten_first.as_secs_f64() * 1_000.0,
                "ten_k_append_second_half_median_ms": ten_second.as_secs_f64() * 1_000.0,
                "hundred_k_append_first_half_median_ms": hundred_first.as_secs_f64() * 1_000.0,
                "hundred_k_append_second_half_median_ms": hundred_second.as_secs_f64() * 1_000.0,
                "ten_k_artifact_append_first_half_median_ms": ten_artifact_first.as_secs_f64() * 1_000.0,
                "ten_k_artifact_append_second_half_median_ms": ten_artifact_second.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_append_first_half_median_ms": hundred_artifact_first.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_append_second_half_median_ms": hundred_artifact_second.as_secs_f64() * 1_000.0,
                "ten_k_target_query_median_ms": ten_query.as_secs_f64() * 1_000.0,
                "hundred_k_target_query_median_ms": hundred_query.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_query_ms": hundred_artifact_query.as_secs_f64() * 1_000.0,
                "ten_k_artifact_segment_scans": ten_artifact_scans,
                "hundred_k_artifact_segment_scans": hundred_artifact_scans,
                "artifact_segment_bytes": artifact_bytes,
                "hundred_k_artifact_segment_bytes": hundred_artifact_segment_bytes,
                "hundred_k_artifact_appended_bytes": hundred_artifact_appended_bytes,
                "target_review_segment_bytes": target_review_bytes,
                "other_review_segment_bytes": other_review_bytes,
                "hot_checkpoint_bytes": checkpoint_bytes,
            })
        );
        assert!(artifact_bytes <= 64 * 1024);
        assert!(target_review_bytes <= 64 * 1024);
        assert!(other_review_bytes > target_review_bytes);
        assert!(checkpoint_bytes <= 256 * 1024);
        assert!(ten_artifact_scans <= 1);
        assert!(hundred_artifact_scans <= 1);
        assert_eq!(
            hundred_artifact_segment_bytes,
            hundred_artifact_appended_bytes
        );
        assert!(
            ten_second
                <= ten_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_second
                <= hundred_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(hundred_second <= std::time::Duration::from_secs(2));
        assert!(
            hundred_second
                <= ten_second
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_artifact_second
                <= hundred_artifact_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_artifact_second
                <= ten_artifact_second
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(hundred_artifact_second <= std::time::Duration::from_secs(2));
        assert!(hundred_artifact_query <= std::time::Duration::from_secs(2));
        assert!(hundred_query <= std::time::Duration::from_millis(250));
        assert!(
            hundred_query
                <= ten_query
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(50))
        );

        drop(hundred_store);
        let reopened =
            TaskRuntimeStore::new_in_memory_with_shadow_root(hundred_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        time_history_target_queries(&reopened, &hundred_run)?;
        assert_eq!(
            reopened
                .list_reviews(&hundred_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        drop(hundred_artifact_store);
        let reopened_artifacts = TaskRuntimeStore::new_in_memory_with_shadow_root(
            hundred_artifact_temp.path().join("tasks"),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened_artifacts
                .list_artifacts(&hundred_artifact_run)
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        Ok(())
    }

    #[test]
    #[ignore = "final 2k/10k scale characterization; run explicitly after the refactor"]
    fn artifact_and_per_task_review_history_characterization_at_2k_and_10k() -> Result<(), String> {
        let (small_temp, small_store, small_run, _, _) = seed_history_scale_fixture(2_000)?;
        let (large_temp, large_store, large_run, _, _) = seed_history_scale_fixture(10_000)?;

        for (store, run_id, expected_reviews) in [
            (&small_store, &small_run, 2_000_usize),
            (&large_store, &large_run, 10_000_usize),
        ] {
            let target_query = time_history_target_queries(store, run_id)?;
            assert!(
                target_query <= std::time::Duration::from_millis(250),
                "targeted history query exceeded daily characterization budget: {target_query:?}"
            );
            assert_eq!(
                store
                    .list_reviews(run_id, "other-task")
                    .map_err(|error| error.to_string())?
                    .len(),
                expected_reviews
            );
            assert_eq!(
                store
                    .list_artifacts(run_id)
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
            let (artifact_path, target_review_path, _) = store
                .shadow
                .history_paths_for_test(run_id, "target-task")
                .map_err(|error| error.to_string())?;
            assert!(
                std::fs::metadata(&artifact_path)
                    .map_err(|error| error.to_string())?
                    .len()
                    <= 64 * 1024
            );
            assert!(
                std::fs::metadata(&target_review_path)
                    .map_err(|error| error.to_string())?
                    .len()
                    <= 64 * 1024
            );
        }

        drop(large_store);
        let restarted =
            TaskRuntimeStore::new_in_memory_with_shadow_root(large_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .list_reviews(&large_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            10_000
        );
        assert_eq!(
            restarted
                .list_reviews(&large_run, "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            restarted
                .list_artifacts(&large_run)
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        time_history_target_queries(&restarted, &large_run)?;
        drop(small_store);
        drop(small_temp);
        Ok(())
    }

    #[test]
    fn query_checkpoint_corruption_self_heals_and_restart_preserves_results() -> Result<(), String>
    {
        let (temp, store, run_id) = seed_public_query_fixture(10_000)?;
        time_public_queries(&store, &run_id)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        drop(store);
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        let repaired: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&checkpoint_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            repaired
                .get("state")
                .and_then(|state| state.get("query_projection_schema"))
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        drop(reopened);
        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&restarted, &run_id)?;
        Ok(())
    }

    #[test]
    fn behind_checkpoint_is_repaired_once_on_query_only_restart() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        let receipt = store
            .shadow
            .append_event_batch(
                &run_id,
                vec![RuntimeJournalEvent::for_append(
                    &run_id,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "checkpoint_suffix"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        let head = receipt.apply.last_sequence;
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let before = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "checkpoint before restart is missing".to_string())?;
        assert!(before.sequence < head);
        drop(store);

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        drop(reopened);
        let repaired = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired checkpoint is missing".to_string())?;
        assert_eq!(repaired.sequence, head);

        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let stats = restarted
            .shadow
            .rewrite_plan_with_stats(&run_id)
            .map_err(|error| error.to_string())?;
        assert!(stats.used_checkpoint);
        assert_eq!(stats.folded_events, 0);
        Ok(())
    }

    #[test]
    fn full_history_rebuild_removes_stale_review_segments() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "stale-history")?;
        store
            .add_review(&history_review("stale-history", "target-task", "review-1"))
            .map_err(|error| error.to_string())?;
        let (_, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("stale-history", "target-task")
            .map_err(|error| error.to_string())?;
        let review_directory = review_path
            .parent()
            .ok_or_else(|| "review history has no directory".to_string())?
            .to_path_buf();
        drop(store);
        let stale_path = review_directory.join("stale-valid-segment.jsonl");
        std::fs::write(&stale_path, b"{}\n").map_err(|error| error.to_string())?;
        std::fs::remove_file(cursor_path).map_err(|error| error.to_string())?;

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_reviews("stale-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert!(!stale_path.exists());
        Ok(())
    }

    #[test]
    fn legacy_query_checkpoint_schema_rebuilds_before_production_read() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        drop(store);
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let mut legacy = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "query checkpoint is missing".to_string())?;
        legacy.state.clear_query_projection_schema_for_test();
        checkpoints
            .save(&legacy.state, legacy.sequence)
            .map_err(|error| error.to_string())?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        drop(reopened);
        let repaired = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired query checkpoint is missing".to_string())?;
        assert!(repaired.state.has_current_query_projection_schema());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn readonly_legacy_checkpoint_does_not_block_journal_derived_queries() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
        use std::os::unix::fs::PermissionsExt;

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let run_directory = temp.path().join("tasks").join(&run_id);
        let checkpoint_path = run_directory.join("checkpoint.json");
        let (artifact_history, review_history, _) = store
            .shadow
            .history_paths_for_test(&run_id, "task-a")
            .map_err(|error| error.to_string())?;
        drop(store);
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let mut legacy = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "query checkpoint is missing".to_string())?;
        legacy.state.clear_query_projection_schema_for_test();
        checkpoints
            .save(&legacy.state, legacy.sequence)
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(artifact_history).map_err(|error| error.to_string())?;
        std::fs::remove_file(review_history).map_err(|error| error.to_string())?;
        let original_permissions = std::fs::metadata(&run_directory)
            .map_err(|error| error.to_string())?
            .permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(&run_directory, readonly).map_err(|error| error.to_string())?;
        let query = (|| {
            let reopened =
                TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
                    .map_err(|error| error.to_string())?;
            time_public_queries(&reopened, &run_id)?;
            let first_replays = reopened
                .shadow
                .history_fallback_replay_count_for_test(&run_id)
                .map_err(|error| error.to_string())?;
            time_public_queries(&reopened, &run_id)?;
            let second_replays = reopened
                .shadow
                .history_fallback_replay_count_for_test(&run_id)
                .map_err(|error| error.to_string())?;
            if first_replays != second_replays {
                return Err("readonly history fallback was replayed on every query".to_string());
            }
            Ok(())
        })();
        std::fs::set_permissions(&run_directory, original_permissions)
            .map_err(|error| error.to_string())?;
        query?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn readonly_review_fallback_lru_avoids_aba_full_replay() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "readonly-lru")?;
        store
            .add_review(&history_review(
                "readonly-lru",
                "target-task",
                "target-review",
            ))
            .map_err(|error| error.to_string())?;
        store
            .add_review(&history_review(
                "readonly-lru",
                "other-task",
                "other-review",
            ))
            .map_err(|error| error.to_string())?;
        let (_, target_path, _) = store
            .shadow
            .history_paths_for_test("readonly-lru", "target-task")
            .map_err(|error| error.to_string())?;
        let (_, other_path, _) = store
            .shadow
            .history_paths_for_test("readonly-lru", "other-task")
            .map_err(|error| error.to_string())?;
        let review_directory = target_path
            .parent()
            .ok_or_else(|| "review segment has no directory".to_string())?
            .to_path_buf();
        drop(store);
        std::fs::remove_file(target_path).map_err(|error| error.to_string())?;
        std::fs::remove_file(other_path).map_err(|error| error.to_string())?;
        let original_permissions = std::fs::metadata(&review_directory)
            .map_err(|error| error.to_string())?
            .permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(&review_directory, readonly).map_err(|error| error.to_string())?;
        let query = (|| {
            let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
                .map_err(|error| error.to_string())?;
            for task_id in ["target-task", "other-task", "target-task"] {
                if reopened
                    .list_reviews("readonly-lru", task_id)
                    .map_err(|error| error.to_string())?
                    .len()
                    != 1
                {
                    return Err(format!("readonly review fallback lost task {task_id}"));
                }
            }
            let replays = reopened
                .shadow
                .history_fallback_replay_count_for_test("readonly-lru")
                .map_err(|error| error.to_string())?;
            if replays != 2 {
                return Err(format!(
                    "readonly A/B/A review fallback replayed {replays} times"
                ));
            }
            Ok(())
        })();
        std::fs::set_permissions(&review_directory, original_permissions)
            .map_err(|error| error.to_string())?;
        query
    }

    #[test]
    fn todo_metadata_clears_across_revision_reset_running_and_restart() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        store
            .create_run(
                "reset-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "reset metadata",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let mut plan = TaskPlan {
            plan_id: "reset-plan".to_string(),
            run_id: "reset-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("reset metadata"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "reset-task".to_string(),
                title: "Reset task".to_string(),
                description: "Verify metadata reset".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "old-owner".to_string(),
                domain_profile: DomainProfile::General,
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "reset-run",
                "reset-task",
                echo_agent::tasks::TaskStatus::Completed,
                Some("old-owner"),
                Some("old-summary"),
            )
            .map_err(|error| error.to_string())?;
        plan.revision = 2;
        store
            .commit_runtime_event(RuntimeJournalEvent::for_append(
                "reset-run",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": 1,
                    "reason": "reset completed task",
                    "reset_task_ids": ["reset-task"],
                    "plan": plan.specification(),
                }),
            ))
            .map_err(|error| error.to_string())?;
        let pending = store
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "reset Todo is missing".to_string())?;
        assert_eq!(pending.status, TodoStatus::Pending);
        assert!(pending.owner_agent.is_none());
        assert!(pending.started_at.is_none());
        assert!(pending.completed_at.is_none());
        assert!(pending.summary.is_none());
        store
            .set_task_status(
                "reset-run",
                "reset-task",
                echo_agent::tasks::TaskStatus::Running,
                Some("new-owner"),
                None,
            )
            .map_err(|error| error.to_string())?;
        let running = store
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "running Todo is missing".to_string())?;
        assert_eq!(running.status, TodoStatus::Running);
        assert_eq!(running.owner_agent.as_deref(), Some("new-owner"));
        assert!(running.started_at.is_some());
        assert!(running.completed_at.is_none());
        assert!(running.summary.is_none());
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let restarted = reopened
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "restarted Todo is missing".to_string())?;
        assert_eq!(restarted.status, TodoStatus::Running);
        assert_eq!(restarted.owner_agent.as_deref(), Some("new-owner"));
        assert!(restarted.completed_at.is_none());
        assert!(restarted.summary.is_none());
        Ok(())
    }

    #[test]
    fn concurrent_public_queries_remain_coherent_with_incremental_appends() -> Result<(), String> {
        let (_temp, store, run_id) = seed_public_query_fixture(10_000)?;
        let store = std::sync::Arc::new(store);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let reader_store = std::sync::Arc::clone(&store);
            let reader_run = run_id.clone();
            let reader_barrier = std::sync::Arc::clone(&barrier);
            readers.push(std::thread::spawn(move || -> Result<(), String> {
                reader_barrier.wait();
                for _ in 0..25 {
                    time_public_queries(&reader_store, &reader_run)?;
                }
                Ok(())
            }));
        }
        barrier.wait();
        for ordinal in 0..25 {
            store
                .note(&run_id, None, &format!("concurrent append {ordinal}"))
                .map_err(|error| error.to_string())?;
        }
        for reader in readers {
            reader
                .join()
                .map_err(|_| "public query reader panicked".to_string())??;
        }
        time_public_queries(&store, &run_id)?;
        Ok(())
    }

    #[test]
    fn projection_receipt_distinguishes_uncommitted_and_committed_degradation() -> Result<(), String>
    {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "receipt-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "receipt",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let before = store
            .list_events("receipt-run", 0)
            .map_err(|error| error.to_string())?
            .len();
        store.fail_next_cell_started_for_test();
        let uncommitted = store.record_background_cell_started(
            "receipt-run",
            "cell-uncommitted",
            "false",
            "hash-a",
            None,
            None,
            None,
        );
        assert!(uncommitted.is_err());
        assert_eq!(
            store
                .list_events("receipt-run", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before
        );
        store.fail_next_cell_started_projection_for_test();
        let committed = store
            .record_background_cell_started(
                "receipt-run",
                "cell-committed",
                "true",
                "hash-b",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, .. } = committed else {
            return Err("committed projection degradation lost its typed receipt".to_string());
        };
        assert!(seq > 0);
        assert_eq!(
            store
                .list_events("receipt-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellStarted)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn durability_receipt_preserves_batch_cell_and_reconciled_degradation() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "durability-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "durability",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let degraded = store
            .commit_runtime_events_with_receipt(
                "durability-run",
                vec![RuntimeJournalEvent::for_append(
                    "durability-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "durability-batch"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail } = degraded else {
            return Err("degraded journal batch was reported durable".to_string());
        };
        assert!(seq > 0);
        assert!(detail.contains("journal durability degraded"));

        store
            .shadow
            .reconcile_next_append_unconfirmed_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let reconciled = store
            .commit_runtime_events_with_receipt(
                "durability-run",
                vec![RuntimeJournalEvent::for_append(
                    "durability-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "reconciled-batch"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            reconciled,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability unconfirmed")
        ));

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let first = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            first,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability degraded")
        ));
        store
            .shadow
            .fail_next_durability_probe_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let still_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            still_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability degraded")
        ));
        let settled = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(settled, ProjectionCommitReceipt::Durable { .. }));

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        store
            .note("durability-run", None, "committed diagnostic boundary")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_events("durability-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::Note
                        && event
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            == Some("committed diagnostic boundary")
                })
                .count(),
            1
        );

        store
            .shadow
            .fail_history_cursor_writes_for_test("durability-run", 3)
            .map_err(|error| error.to_string())?;
        let history_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            history_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        let duplicate_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            duplicate_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        let duplicate_repaired = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            duplicate_repaired,
            ProjectionCommitReceipt::Durable { .. }
        ));

        let checkpoint = TaskRuntimeStore::classify_committed_projection(
            42,
            JournalDurabilityStatus::Confirmed,
            CheckpointApplyStatus::Degraded {
                error: "checkpoint-write".to_string(),
            },
            HistoryProjectionApplyStatus::Current,
            ProjectionCommitReceipt::Durable { seq: 42 },
        );
        assert!(matches!(
            checkpoint,
            ProjectionCommitReceipt::CommittedProjectionDegraded { seq: 42, detail }
                if detail.contains("checkpoint durability degraded")
        ));
        Ok(())
    }

    #[test]
    fn partial_history_failure_replays_without_duplicates_before_advancing_cursor()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "partial-history")?;
        let (artifact_path, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("partial-history", "target-task")
            .map_err(|error| error.to_string())?;
        let cursor_before = std::fs::read(&cursor_path).map_err(|error| error.to_string())?;
        store
            .shadow
            .fail_next_review_history_append_for_test("partial-history")
            .map_err(|error| error.to_string())?;
        let receipt = store
            .commit_runtime_events_with_receipt(
                "partial-history",
                vec![
                    artifact_history_event("partial-history", "artifact-1"),
                    review_history_event("partial-history", "target-task", "review-1"),
                ],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            receipt,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        assert_eq!(
            std::fs::read(&cursor_path).map_err(|error| error.to_string())?,
            cursor_before
        );
        assert_eq!(line_count(&artifact_path)?, 1);
        assert!(!review_path.exists());
        drop(store);

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_artifacts("partial-history")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            reopened
                .list_reviews("partial-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(line_count(&artifact_path)?, 1);
        assert_eq!(line_count(&review_path)?, 1);
        Ok(())
    }

    #[test]
    fn missing_history_segments_recover_old_and_new_facts_from_journal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "missing-history")?;
        store
            .add_artifact(&history_artifact("missing-history", "artifact-old"))
            .map_err(|error| error.to_string())?;
        store
            .add_review(&history_review(
                "missing-history",
                "target-task",
                "review-old",
            ))
            .map_err(|error| error.to_string())?;
        let (artifact_path, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("missing-history", "target-task")
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(&artifact_path).map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_artifacts("missing-history")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let repaired_artifact_cursor = history_cursor_sequence(&cursor_path)?;
        store
            .add_artifact(&history_artifact("missing-history", "artifact-new"))
            .map_err(|error| error.to_string())?;
        let artifacts = store
            .list_artifacts("missing-history")
            .map_err(|error| error.to_string())?;
        assert_eq!(artifacts.len(), 2);
        assert_eq!(line_count(&artifact_path)?, 2);
        assert!(history_cursor_sequence(&cursor_path)? > repaired_artifact_cursor);
        std::fs::remove_file(&artifact_path).map_err(|error| error.to_string())?;
        let append_repair = store
            .commit_runtime_events_with_receipt(
                "missing-history",
                vec![artifact_history_event(
                    "missing-history",
                    "artifact-after-unlink",
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            append_repair,
            ProjectionCommitReceipt::Durable { .. }
        ));
        assert_eq!(line_count(&artifact_path)?, 3);

        std::fs::remove_file(&review_path).map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_reviews("missing-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let repaired_review_cursor = history_cursor_sequence(&cursor_path)?;
        store
            .add_review(&history_review(
                "missing-history",
                "target-task",
                "review-new",
            ))
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews("missing-history", "target-task")
            .map_err(|error| error.to_string())?;
        assert_eq!(reviews.len(), 2);
        assert_eq!(line_count(&review_path)?, 2);
        assert!(history_cursor_sequence(&cursor_path)? > repaired_review_cursor);
        std::fs::remove_file(&review_path).map_err(|error| error.to_string())?;
        let append_repair = store
            .commit_runtime_events_with_receipt(
                "missing-history",
                vec![review_history_event(
                    "missing-history",
                    "target-task",
                    "review-after-unlink",
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            append_repair,
            ProjectionCommitReceipt::Durable { .. }
        ));
        assert_eq!(line_count(&review_path)?, 3);

        std::fs::write(&artifact_path, b"{corrupt artifact segment\n")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_artifacts("missing-history")
                .map_err(|error| error.to_string())?
                .len(),
            3
        );
        assert_eq!(line_count(&artifact_path)?, 3);
        std::fs::write(&review_path, b"{corrupt review segment\n")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_reviews("missing-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            3
        );
        assert_eq!(line_count(&review_path)?, 3);
        Ok(())
    }

    #[test]
    fn valid_jsonl_prefix_and_empty_segment_self_heal_after_restart() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "truncated-history")?;
        for id in ["artifact-1", "artifact-2"] {
            store
                .add_artifact(&history_artifact("truncated-history", id))
                .map_err(|error| error.to_string())?;
        }
        for id in ["review-1", "review-2"] {
            store
                .add_review(&history_review("truncated-history", "target-task", id))
                .map_err(|error| error.to_string())?;
        }
        let (artifact_path, review_path, _) = store
            .shadow
            .history_paths_for_test("truncated-history", "target-task")
            .map_err(|error| error.to_string())?;
        drop(store);

        retain_first_jsonl_record(&artifact_path)?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_artifacts("truncated-history")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert_eq!(line_count(&artifact_path)?, 2);
        drop(reopened);

        std::fs::write(&review_path, b"").map_err(|error| error.to_string())?;
        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .list_reviews("truncated-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert_eq!(line_count(&review_path)?, 2);
        Ok(())
    }

    #[test]
    fn committed_projection_degradation_preserves_all_mutation_outcomes() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "transition-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "transition",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store
                .transition_run("transition-run", TaskRunStatus::Running)
                .map_err(|error| error.to_string())?
                .status,
            TaskRunStatus::Running
        );
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store
                .finalize_run("transition-run", TaskRunStatus::Failed, Some("terminal"))
                .map_err(|error| error.to_string())?
                .status,
            TaskRunStatus::Failed
        );

        store
            .create_run(
                "pause-goal-run",
                "test",
                "conversation",
                "root-2",
                DomainProfile::General,
                "old goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("pause-goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert!(
            store
                .request_pause("pause-goal-run")
                .map_err(|error| error.to_string())?
        );
        store.fail_next_runtime_mutation_projection_for_test();
        let updated = store
            .update_run_goal(
                "pause-goal-run",
                1,
                "new goal",
                "user revised goal",
                RunGoalActorSource::Gui,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.goal, "new goal");
        assert_eq!(updated.goal_revision, 2);

        store
            .create_run(
                "plan-run",
                "test",
                "conversation",
                "root-3",
                DomainProfile::General,
                "plan",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "plan-id".to_string(),
                run_id: "plan-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("plan"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "task-a".to_string(),
                    title: "Task A".to_string(),
                    description: "Do A".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "subagent".to_string(),
                    domain_profile: DomainProfile::General,
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .get_plan("plan-run")
                .map_err(|error| error.to_string())?
                .is_some()
        );

        store
            .create_run(
                "turn-run",
                "test",
                "conversation",
                "root-4",
                DomainProfile::General,
                "turn",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("turn-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("turn-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert!(matches!(
            store
                .claim_run_turn(
                    "turn-run",
                    "turn-1",
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )
                .map_err(|error| error.to_string())?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert_eq!(
            store
                .list_events("turn-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn direct_completion_survives_committed_projection_degradation() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        let run_id = "direct-degraded";
        store
            .create_run(
                run_id,
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "direct answer",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            store
                .claim_run_turn(
                    run_id,
                    "turn-direct",
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )
                .map_err(|error| error.to_string())?,
            RunTurnClaimOutcome::Started(_)
        ));
        let task_id = "direct-answer";
        let plan = TaskPlan {
            plan_id: format!("plan:{run_id}"),
            run_id: run_id.to_string(),
            revision: 0,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("direct answer"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: task_id.to_string(),
                title: "Direct answer".to_string(),
                description: "direct answer".to_string(),
                kind: PlanTaskKind::Summary,
                agent_role: "primary-agent".to_string(),
                domain_profile: DomainProfile::General,
                ..Default::default()
            }],
        };
        let summary = TaskExecutionSummary {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            subagent_name: "primary-agent".to_string(),
            outcome: SubagentOutcome::terminal(
                SubagentStatus::Completed,
                "complete answer",
                Vec::new(),
            ),
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        };
        store.fail_next_runtime_mutation_projection_for_test();
        super::super::revisioned_runtime::commit_direct_completion(
            store.clone(),
            plan,
            summary,
            "complete answer".to_string(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let todos = store
            .list_todos(run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(todos.len(), 1);
        assert_eq!(
            todos.first().map(|todo| todo.status),
            Some(TodoStatus::Completed)
        );
        assert_eq!(
            store
                .list_events(run_id, 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::PlanRevisionCommitted)
                .count(),
            1
        );
        Ok(())
    }

    /// Helper: a minimal `PlanTask` body with the given id and sane defaults,
    /// for the cycle test above (avoids repeating the full struct literal).
    fn sample_task_body(id: &str) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
            claim: None,
            sort_order: 0,
        }
    }
}
