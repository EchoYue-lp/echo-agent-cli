#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::subagent::{SubagentPromptCompiler, SubagentTaskContext};
    use futures::future::BoxFuture;
    use futures::stream::{self, BoxStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedTurnAgent {
        script: fn(&str) -> Vec<AgentEvent>,
        execution_id: String,
    }

    struct PermissionCountingTool {
        name: String,
        permission: echo_agent::prelude::ToolPermission,
        readonly_capability: bool,
        calls: Arc<AtomicUsize>,
        policies:
            Option<Arc<std::sync::Mutex<Vec<Option<echo_agent::tasks::NestedDelegationPolicy>>>>>,
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
                readonly_capability: false,
                calls,
                policies: None,
            }
        }

        fn with_policy_observer(
            mut self,
            policies: Arc<std::sync::Mutex<Vec<Option<echo_agent::tasks::NestedDelegationPolicy>>>>,
        ) -> Self {
            self.policies = Some(policies);
            self
        }

        fn with_readonly_capability(mut self) -> Self {
            self.readonly_capability = true;
            self
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

        fn risk_level(&self) -> echo_agent::tools::ToolRiskLevel {
            if self.readonly_capability {
                echo_agent::tools::ToolRiskLevel::ReadOnly
            } else {
                echo_agent::tools::ToolRiskLevel::Standard
            }
        }

        fn execute_with_context<'a>(
            &'a self,
            parameters: echo_agent::tools::ToolParameters,
            context: &'a echo_agent::tools::ToolContext,
        ) -> BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>> {
            Box::pin(async move {
                if let Some(policies) = self.policies.as_ref() {
                    policies
                        .lock()
                        .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                        .push(context.delegation_policy);
                }
                self.execute(parameters).await
            })
        }
    }

    impl ScriptedTurnAgent {
        fn new(script: fn(&str) -> Vec<AgentEvent>, execution_id: &str) -> Self {
            Self {
                script,
                execution_id: execution_id.to_string(),
            }
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

        fn execute_stream_message_with_invocation_context<'a>(
            &'a self,
            _message: echo_agent::llm::types::Message,
            _cancel: echo_agent::agent::CancellationToken,
            _invocation: echo_agent::agent::AgentInvocationContext,
        ) -> BoxFuture<
            'a,
            echo_agent::error::Result<BoxStream<'a, echo_agent::error::Result<AgentEvent>>>,
        > {
            let events = (self.script)(&self.execution_id);
            Box::pin(async move {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as BoxStream<'a, _>)
            })
        }
    }

    // 测试source显式声明自身结算；通用正文确认不替代原生执行终态。
    fn scripted_execution_status(
        execution_id: &str,
        kind: echo_agent::agent::ExecutionStatusKind,
        sequence: u64,
    ) -> AgentEvent {
        AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
            adapter_generated: false,
            execution_id: execution_id.into(),
            request_id: None,
            sequence,
            phase: if kind == echo_agent::agent::ExecutionStatusKind::Started {
                echo_agent::agent::AgentPhase::Thinking
            } else {
                echo_agent::agent::AgentPhase::Terminal
            },
            status: kind,
            reason: None,
        })
    }

    fn turn_identity(
        run: &TaskRun,
        turn_id: &str,
    ) -> Result<echo_agent::agent::EventIdentity, String> {
        echo_agent::agent::EventIdentity::for_chat(
            Some(run.conversation_id.clone()),
            turn_id,
            run.root_message_id.clone(),
            Some(run.run_id.clone()),
        )
        .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn owned_run_usage_is_durable_without_trace_and_trace_projection_never_rebills()
    -> anyhow::Result<()> {
        use echo_agent::agent::{
            EventEnvelope, EventIdentity, ModelCallObservation, ModelCallOutcome,
        };
        for with_trace in [false, true] {
            let directory = tempfile::tempdir()?;
            let (workspace, conversation, original_owner, journal_identity) = {
                let (store, _agent, log, _runtime) =
                    publication_run_fixture(directory.path()).await?;
                let _resources = store.test_resource_scope();
                let run_id = seed_run(&store, vec![solo_readonly_task("usage-source")])
                    .map_err(anyhow::Error::msg)?;
                let run = store
                    .get_run(&run_id)?
                    .ok_or_else(|| anyhow::anyhow!("run missing"))?;
                let turn = "actual-owned-usage-turn";
                // 原公开配置/claim必须真实进入Started；Ok也可能是NotSubmitted。
                store.configure_run_continuation(&run_id, true, false, None, None)?;
                let claim = store.claim_run_turn(
                    &run_id,
                    turn,
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )?;
                anyhow::ensure!(
                    matches!(&claim, super::super::store::RunTurnClaimOutcome::Started(started) if started.turn_id == turn),
                    "usage fixture original claim was not Started: {claim:?}"
                );
                let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
                let trace: Option<ExecSink> = if with_trace {
                    let log = log.clone();
                    let errors = errors.clone();
                    Some(Arc::new(move |event| {
                        if event.event == RuntimeEventKind::Usage {
                            // 原Exec trace可投到Run根；费用必须按显式native source去重，不能猜Run ID。
                            for _ in 0..2 {
                                if let Err(error) = log.append(
                                    &event.workspace_id,
                                    Some(&event.conversation_id),
                                    "trace-original-run-root",
                                    crate::chat_driver::ChatDriverEvent::Execution(Box::new(
                                        event.clone(),
                                    )),
                                ) {
                                    errors
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner())
                                        .push(error.to_string());
                                }
                            }
                        }
                    }))
                } else {
                    None
                };
                let sink = EkoAgentTurnSink::for_run(
                    &run,
                    turn,
                    TaskRuntimeOperation::new(store.clone()),
                    HashSet::new(),
                    trace,
                );
                let identity = EventIdentity::for_chat(
                    Some(run.conversation_id.clone()),
                    turn,
                    turn,
                    Some(run_id.clone()),
                )?
                .with_execution_id("original-native-execution-distinct-from-run")?;
                let started = AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
                    adapter_generated: false,
                    execution_id: "original-native-execution-distinct-from-run".into(),
                    request_id: None,
                    sequence: 1,
                    phase: echo_agent::agent::AgentPhase::Thinking,
                    status: echo_agent::agent::ExecutionStatusKind::Started,
                    reason: None,
                });
                let waiting = AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
                    adapter_generated: false,
                    execution_id: "original-native-execution-distinct-from-run".into(),
                    request_id: Some("owned-logical-call".into()),
                    sequence: 2,
                    phase: echo_agent::agent::AgentPhase::Thinking,
                    status: echo_agent::agent::ExecutionStatusKind::Waiting,
                    reason: None,
                });
                let usage = AgentEvent::LlmUsage {
                    model: "source-model".into(),
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    total_tokens: 120,
                    cached_prompt_tokens: 50,
                    cache_creation_prompt_tokens: 5,
                    usage_reported: true,
                    observation: Some(ModelCallObservation {
                        call_id: "owned-logical-call".into(),
                        attempt_id: "actual-attempt".into(),
                        outcome: ModelCallOutcome::Completed,
                        request_duration_ms: 1200,
                        first_output_ms: Some(100),
                        last_output_ms: Some(1100),
                        output_samples: 2,
                        timing_comparable: true,
                    }),
                };
                for (index, payload) in [started, waiting, usage].into_iter().enumerate() {
                    let event = EventEnvelope::new(
                        &identity,
                        u64::try_from(index)?.saturating_add(1),
                        None,
                        payload,
                    )?;
                    EventSink::on_event(&sink, event.clone()).await?;
                    if matches!(event.payload, AgentEvent::LlmUsage { .. }) {
                        EventSink::on_event(&sink, event).await?;
                    }
                }
                assert!(
                    errors
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .is_empty()
                );
                let snapshot = log.conversation_usage(&run.workspace_id, &run.conversation_id)?;
                assert_eq!(
                    (
                        snapshot.main.steps,
                        snapshot.main.attempts,
                        snapshot.main.pending_calls
                    ),
                    (1, 1, 0)
                );
                assert_eq!(
                    (
                        snapshot.main.input_tokens,
                        snapshot.main.output_tokens,
                        snapshot.main.cached_tokens
                    ),
                    (100, 20, 50)
                );
                assert_eq!(snapshot.turns, 1);
                assert!(snapshot.turns_known);
                let events = log.replay(&run.workspace_id, Some(&run.conversation_id), turn, 0)?;
                assert_eq!(events.events.iter().filter(|event|matches!(&event.payload,
                    crate::chat_driver::ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::LlmUsage {..}))).count(),1);
                (
                    run.workspace_id,
                    run.conversation_id,
                    Arc::downgrade(&log),
                    snapshot.journal_identity,
                )
            };
            assert!(
                original_owner.upgrade().is_none(),
                "释放原log owner后才叫cold reopen"
            );
            let cold = crate::chat_event_log::ChatEventLog::open(
                directory.path().join("chat-events"),
                crate::chat_event_log::ChatEventRetention::default(),
            )?;
            let restored = cold.conversation_usage(&workspace, &conversation)?;
            assert_eq!(restored.journal_identity, journal_identity);
            assert_eq!(
                (
                    restored.main.steps,
                    restored.main.attempts,
                    restored.main.pending_calls
                ),
                (1, 1, 0)
            );
            assert_eq!(
                (
                    restored.main.input_tokens,
                    restored.main.output_tokens,
                    restored.main.cached_tokens
                ),
                (100, 20, 50)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn owned_run_usage_never_unlocks_native_publication_without_a_source_witness()
    -> anyhow::Result<()> {
        use echo_agent::agent::{EventEnvelope, EventIdentity};
        let directory = tempfile::tempdir()?;
        let (store, _agent, log, _runtime) = publication_run_fixture(directory.path()).await?;
        let _resources = store.test_resource_scope();
        let run_id = seed_run(&store, vec![solo_readonly_task("usage-not-header")])
            .map_err(anyhow::Error::msg)?;
        let run = store
            .get_run(&run_id)?
            .ok_or_else(|| anyhow::anyhow!("run missing"))?;
        let turn = "usage-observation-is-not-started";
        store.configure_run_continuation(&run_id, true, false, None, None)?;
        let claim =
            store.claim_run_turn(&run_id, turn, RunTurnOrigin::User, TurnVisibility::Visible)?;
        anyhow::ensure!(
            matches!(&claim, super::super::store::RunTurnClaimOutcome::Started(started) if started.turn_id == turn),
            "usage fixture original claim was not Started: {claim:?}"
        );
        let sink = EkoAgentTurnSink::for_run(
            &run,
            turn,
            TaskRuntimeOperation::new(store),
            HashSet::new(),
            None,
        );
        let identity =
            EventIdentity::for_chat(Some(run.conversation_id.clone()), turn, turn, Some(run_id))?
                .with_execution_id("usage-only-execution")?;
        for (index, payload) in [
            AgentEvent::LlmUsage {
                model: "observed".into(),
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
                cached_prompt_tokens: 0,
                cache_creation_prompt_tokens: 0,
                usage_reported: true,
                observation: None,
            },
            AgentEvent::FinalAnswer("没有原发布见证，不能给正文补资格".into()),
        ]
        .into_iter()
        .enumerate()
        {
            EventSink::on_event(
                &sink,
                EventEnvelope::new(
                    &identity,
                    u64::try_from(index)?.saturating_add(1),
                    None,
                    payload,
                )?,
            )
            .await?;
        }
        let replay = log.replay(&run.workspace_id, Some(&run.conversation_id), turn, 0)?;
        assert_eq!(replay.events.iter().filter(|event|matches!(&event.payload,
            crate::chat_driver::ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::LlmUsage {..}))).count(),1);
        assert!(!replay.events.iter().any(|event|matches!(&event.payload,
            crate::chat_driver::ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::FinalAnswer(_)|AgentEvent::ExecutionStatus(_)))));
        Ok(())
    }

    #[tokio::test]
    async fn owned_run_usage_retries_original_accounting_after_journal_success()
    -> anyhow::Result<()> {
        use echo_agent::agent::{EventEnvelope, EventIdentity};
        let directory = tempfile::tempdir()?;
        let (store, _agent, log, _runtime) = publication_run_fixture(directory.path()).await?;
        let _resources = store.test_resource_scope();
        let run_id = seed_run(
            &store,
            vec![solo_readonly_task("retry-original-accounting")],
        )
        .map_err(anyhow::Error::msg)?;
        let run = store
            .get_run(&run_id)?
            .ok_or_else(|| anyhow::anyhow!("run missing"))?;
        let turn = "restore-original-accounting-after-error";
        let sink = EkoAgentTurnSink::for_run(
            &run,
            turn,
            TaskRuntimeOperation::new(store.clone()),
            HashSet::new(),
            None,
        );
        let identity = EventIdentity::for_chat(
            Some(run.conversation_id.clone()),
            turn,
            turn,
            Some(run_id.clone()),
        )?
        .with_execution_id("actual-usage-execution")?;
        let usage = EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::LlmUsage {
                model: "source-model".into(),
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
                cached_prompt_tokens: 0,
                cache_creation_prompt_tokens: 0,
                usage_reported: true,
                observation: None,
            },
        )?;
        // 有意缺原active RunTurn，只制造原Store一次真实拒绝，不模拟合法准入或执行。
        let first = EventSink::on_event(&sink, usage.clone()).await;
        assert!(first.is_err(), "原accounting失败必须保留");
        assert!(log.replay(&run.workspace_id,Some(&run.conversation_id),turn,0)?.events.iter().any(|event|
            matches!(&event.payload,crate::chat_driver::ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::LlmUsage {..}))),
            "此次故障发生在原LU已journal之后");
        store.configure_run_continuation(&run_id, true, false, None, None)?;
        let claim =
            store.claim_run_turn(&run_id, turn, RunTurnOrigin::User, TurnVisibility::Visible)?;
        anyhow::ensure!(
            matches!(&claim, super::super::store::RunTurnClaimOutcome::Started(started) if started.turn_id == turn),
            "usage fixture original claim was not Started: {claim:?}"
        );
        EventSink::on_event(&sink, usage.clone()).await?;
        EventSink::on_event(&sink, usage).await?;
        let continuation = store
            .get_run_state(&run_id)?
            .and_then(|state| state.continuation)
            .ok_or_else(|| anyhow::anyhow!("continuation missing"))?;
        assert_eq!(
            continuation.tokens_used, 12,
            "同envelope重试须完成原费用且不得重复加账"
        );
        let stats = log.conversation_usage(&run.workspace_id, &run.conversation_id)?;
        assert_eq!(
            (
                stats.main.input_tokens,
                stats.main.output_tokens,
                stats.main.attempts
            ),
            (10, 2, 1)
        );
        Ok(())
    }

    async fn drive_scripted_run_turn(
        store: Arc<TaskRuntimeStore>,
        run: &TaskRun,
        turn_id: &str,
        script: fn(&str) -> Vec<AgentEvent>,
    ) -> Result<TurnReceipt, String> {
        let _resources = store.test_resource_scope();
        let sink = EkoAgentTurnSink::for_run(
            run,
            turn_id,
            TaskRuntimeOperation::new(store),
            HashSet::new(),
            None,
        );
        let request =
            TurnRequest::new(turn_identity(run, turn_id)?, "test").mode(TurnMode::Execute);
        AgentTurnDriver
            .drive(&ScriptedTurnAgent::new(script, turn_id), request, &sink)
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn interrupted_task_progress_keeps_original_save_settlement_in_owned_journal()
    -> anyhow::Result<()> {
        fn saving_failure(execution_id: &str) -> Vec<AgentEvent> {
            vec![
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Started,
                    1,
                ),
                AgentEvent::TranscriptProjectionSettlement(
                    echo_agent::memory::TranscriptProjectionSettlement {
                        status: echo_agent::memory::TranscriptProjectionSettlementStatus::Blocked,
                        operation_id: Some("original-save-operation".into()),
                        conversation_id: Some("conv_test".into()),
                        generation_id: Some("original-generation".into()),
                        attempt: 1,
                        error_class: Some(
                            echo_agent::memory::TranscriptProjectionErrorClass::OutcomeUnknown,
                        ),
                        detail: Some("fixture save not confirmed".into()),
                    },
                ),
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Failed,
                    2,
                ),
                AgentEvent::Error {
                    source: "fixture_interrupted".into(),
                    message: "save interrupted".into(),
                    failure: echo_agent::error::AgentFailure::message(
                        "fixture_interrupted",
                        "save interrupted",
                    ),
                },
            ]
        }
        let directory = tempfile::tempdir()?;
        let (store, _agent, log, runtime) = publication_run_fixture(directory.path()).await?;
        let _resources = store.test_resource_scope();
        let run_id = seed_run(
            &store,
            vec![solo_readonly_task("original-save-observation")],
        )
        .map_err(anyhow::Error::msg)?;
        let run = store
            .get_run(&run_id)?
            .ok_or_else(|| anyhow::anyhow!("run missing"))?;
        let turn = "owned-save-failure-turn";
        store.claim_run_turn(&run_id, turn, RunTurnOrigin::User, TurnVisibility::Visible)?;
        let sink = EkoAgentTurnSink::for_run(
            &run,
            turn,
            TaskRuntimeOperation::new(store.clone()),
            HashSet::new(),
            None,
        );
        // RunTurn的原发布root是本轮turn；PlanTask helper的run.root_message_id
        // 属于另一种上下文，不能拿它作为这个原ChatEventLog的publication root。
        let identity = echo_agent::agent::EventIdentity::for_chat(
            Some(run.conversation_id.clone()),
            turn,
            turn,
            Some(run_id.clone()),
        )?
        .with_execution_id(turn)?;
        let request =
            TurnRequest::new(identity, "report original evidence").mode(TurnMode::Execute);
        let receipt = AgentTurnDriver
            .drive(
                &ScriptedTurnAgent::new(saving_failure, turn),
                request,
                &sink,
            )
            .await?;
        assert!(matches!(receipt.outcome, TurnOutcome::Failed(_)));
        assert_eq!(
            receipt.delivery,
            echo_agent::runtime::TurnDeliveryOutcome::Delivered,
            "the original source must reach its owned journal and publication: {receipt:?}"
        );
        let native = store
            .list_events(&run_id, 0)?
            .into_iter()
            .filter(|event| {
                event
                    .payload
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("native_agent_event")
            })
            .map(|event| {
                let raw = event
                    .payload
                    .get("agent_envelope")
                    .ok_or_else(|| anyhow::anyhow!("native envelope missing"))?;
                Ok(serde_json::from_value::<echo_agent::agent::EventEnvelope>(
                    raw.clone(),
                )?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let saves = native
            .iter()
            .filter_map(|event| match &event.payload {
                AgentEvent::TranscriptProjectionSettlement(settlement) => Some((event, settlement)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            saves.len(),
            1,
            "owned sink must persist the original save fact before publication"
        );
        let (event, settlement) = saves
            .first()
            .ok_or_else(|| anyhow::anyhow!("save fact disappeared"))?;
        assert_eq!(
            settlement.operation_id.as_deref(),
            Some("original-save-operation")
        );
        assert_eq!(
            settlement.status,
            echo_agent::memory::TranscriptProjectionSettlementStatus::Blocked
        );
        assert_eq!(
            event.run_id.as_ref().map(|id| id.as_str()),
            Some(run_id.as_str())
        );
        assert_eq!(
            event.execution_id.as_ref().map(|id| id.as_str()),
            Some(turn)
        );
        assert_eq!(
            store.get_run(&run_id)?.map(|run| run.status),
            Some(TaskRunStatus::Running)
        );
        let replay = log.replay(
            &run.workspace_id,
            Some(&run.conversation_id),
            &run.root_message_id,
            0,
        )?;
        let expected = serde_json::to_value(*event)?;
        let published = replay
            .events
            .iter()
            .find_map(|entry| match &entry.payload {
                crate::chat_driver::ChatDriverEvent::Agent(original)
                    if original.event_id == event.event_id =>
                {
                    Some(original)
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("original save publication missing"))?;
        assert_eq!(serde_json::to_value(published.as_ref())?, expected);
        runtime.shutdown().await.map_err(anyhow::Error::msg)?;
        drop(runtime);
        drop(log);
        drop(sink);
        drop(store);
        let cold = crate::chat_event_log::ChatEventLog::open(
            directory.path().join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?;
        let replay = cold.replay(
            &run.workspace_id,
            Some(&run.conversation_id),
            &run.root_message_id,
            0,
        )?;
        let published = replay
            .events
            .iter()
            .find_map(|entry| match &entry.payload {
                crate::chat_driver::ChatDriverEvent::Agent(original)
                    if original.event_id == event.event_id =>
                {
                    Some(original)
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("cold original save publication missing"))?;
        assert_eq!(
            serde_json::to_value(published.as_ref())?,
            expected,
            "cold replay must retain the original save envelope, not reconstruct a notice"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_task_progress_save_fact_cannot_create_execution_head() -> anyhow::Result<()>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let _resources = store.test_resource_scope();
        let run_id = seed_run(&store, vec![solo_readonly_task("no-save-bootstrap")])
            .map_err(anyhow::Error::msg)?;
        let run = store
            .get_run(&run_id)?
            .ok_or_else(|| anyhow::anyhow!("run missing"))?;
        let turn = "no-native-head-turn";
        store.claim_run_turn(&run_id, turn, RunTurnOrigin::User, TurnVisibility::Visible)?;
        let sink = EkoAgentTurnSink::for_run(
            &run,
            turn,
            TaskRuntimeOperation::new(store.clone()),
            HashSet::new(),
            None,
        );
        let identity = turn_identity(&run, turn)
            .map_err(anyhow::Error::msg)?
            .with_execution_id(turn)?;
        let save = echo_agent::memory::TranscriptProjectionSettlement {
            status: echo_agent::memory::TranscriptProjectionSettlementStatus::Blocked,
            operation_id: Some("not-an-execution-head".into()),
            conversation_id: Some(run.conversation_id.clone()),
            generation_id: None,
            attempt: 1,
            error_class: Some(echo_agent::memory::TranscriptProjectionErrorClass::OutcomeUnknown),
            detail: None,
        };
        sink.on_event(echo_agent::agent::EventEnvelope::new(
            &identity,
            1,
            None,
            AgentEvent::TranscriptProjectionSettlement(save),
        )?)
        .await?;
        sink.on_event(echo_agent::agent::EventEnvelope::new(
            &identity,
            2,
            None,
            AgentEvent::FinalAnswer("must not bootstrap publication".into()),
        )?)
        .await?;
        assert!(!store.list_events(&run_id, 0)?.iter().any(|event| {
            event
                .payload
                .get("kind")
                .and_then(serde_json::Value::as_str)
                == Some("native_agent_event")
        }));
        Ok(())
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
        )
        .to_value()?;
        let context = SubagentTaskContext {
            task_title: Some(task.title.clone()),
            user_goal: user_goal.map(str::to_string),
            files: task.files.clone(),
            execution_checks: task.execution_checks.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            required_artifacts: task.required_artifacts.clone(),
            ..SubagentTaskContext::default()
        };
        let compiler = crate::subagent_prompt::EkoSubagentPromptCompiler;
        Ok(compiler
            .compile_invocation(&SubagentInvocation {
                agent_name: &task.agent_role,
                task: &task.description,
                mode: echo_agent::subagent::ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::Fresh,
                history: &[],
                history_limit: None,
                current_message: None,
                context: &context,
                capability_override: None,
                payload: Some(&payload),
            })
            .task_input())
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
        fn missing_terminal(execution_id: &str) -> Vec<AgentEvent> {
            vec![
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Started,
                    0,
                ),
                AgentEvent::AssistantMessage {
                    block: echo_agent::agent::AssistantMessageBlock {
                        provider_source: None,
                        block_id: "missing-terminal-block".to_string(),
                        request_id: "missing-terminal-request".to_string(),
                        sequence: 1,
                        phase: echo_agent::agent::AssistantPhase::Commentary,
                        delta: "partial output".to_string(),
                        complete: true,
                        provider: Some("eko-native-test-fixture".to_string()),
                        reasoning: None,
                    },
                },
            ]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("missing-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing-terminal run was not created".to_string())?;
        let receipt = drive_scripted_run_turn(
            store.clone(),
            &run,
            "missing-terminal-turn",
            missing_terminal,
        )
        .await?;

        // 原envelope在EOF先报告agent_stream；后续projection诊断不能覆盖这一首因。
        assert!(
            matches!(receipt.outcome, TurnOutcome::Failed(ref failure)
            if failure.code == "agent_stream"
                && failure.message == "agent stream ended without a terminal event"),
            "original missing-terminal receipt: {receipt:?}"
        );
        assert!(receipt.final_answer.is_none());
        assert_eq!(receipt.usage().tokens_used, None);
        Ok(())
    }

    #[tokio::test]
    async fn task_turn_usage_counts_only_provider_reported_events() -> Result<(), String> {
        fn usage_script(execution_id: &str) -> Vec<AgentEvent> {
            vec![
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Started,
                    0,
                ),
                AgentEvent::LlmUsage {
                    model: "unknown-usage".to_string(),
                    prompt_tokens: 100,
                    completion_tokens: 200,
                    total_tokens: 300,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: false,
                    observation: None,
                },
                AgentEvent::LlmUsage {
                    model: "reported-usage".to_string(),
                    prompt_tokens: 3,
                    completion_tokens: 4,
                    total_tokens: 7,
                    cached_prompt_tokens: 1,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: true,
                    observation: None,
                },
                AgentEvent::FinalAnswer("done".to_string()),
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Settled,
                    1,
                ),
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
            TaskRuntimeOperation::new(store.clone()),
            HashSet::new(),
            None,
        );
        let request = TurnRequest::new(
            turn_identity(&run, &execution_id)?
                .with_execution_id(&execution_id)
                .map_err(|error| error.to_string())?,
            "test",
        )
        .mode(TurnMode::Execute);
        let receipt = AgentTurnDriver
            .drive(
                &ScriptedTurnAgent::new(usage_script, &execution_id),
                request,
                &sink,
            )
            .await
            .map_err(|error| error.to_string())?;

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
        fn provider_failure(execution_id: &str) -> Vec<AgentEvent> {
            let failure = echo_agent::error::AgentFailure {
                category: echo_agent::error::AgentFailureCategory::Llm,
                terminal_kind: echo_agent::error::AgentTerminalKind::TimedOut,
                retryable: true,
                code: "llm_timeout".to_string(),
                http_status: Some(504),
                message: "provider timed out".to_string(),
            };
            vec![
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Started,
                    0,
                ),
                AgentEvent::Error {
                    source: "llm".to_string(),
                    message: failure.message.clone(),
                    failure,
                },
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Failed,
                    1,
                ),
            ]
        }

        fn cancelled(execution_id: &str) -> Vec<AgentEvent> {
            vec![
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Started,
                    0,
                ),
                AgentEvent::Cancelled,
                scripted_execution_status(
                    execution_id,
                    echo_agent::agent::ExecutionStatusKind::Cancelled,
                    1,
                ),
            ]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("typed-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "typed-terminal run was not created".to_string())?;
        let failed =
            drive_scripted_run_turn(store.clone(), &run, "provider-timeout", provider_failure)
                .await?;
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
        let cancelled =
            drive_scripted_run_turn(store.clone(), &run, "cancelled-turn", cancelled).await?;
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
            let mut result = SubagentOutcome::terminal(
                SubagentStatus::TimedOut,
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
                .map(|receipt| receipt.resolution)
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
        assert!(summary.outcome.evidence.iter().any(|evidence| {
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
            domain_binding: None,
            domain_role: None,

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
            result: None,
            interruption_requested: None,
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
        let tasks = parse_suggested_tasks(output);
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
        assert!(summary.outcome.evidence.iter().any(|evidence| {
            evidence.kind == "tool_result"
                && evidence.subject == "read_file"
                && evidence
                    .attributes
                    .get("args")
                    .and_then(|args| args.get("path"))
                    .and_then(serde_json::Value::as_str)
                    == Some("README.md")
        }));
        assert_eq!(
            summary.outcome.touched_files.read,
            vec!["README.md".to_string()]
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
    async fn task_runtime_operation_keeps_async_heartbeat_responsive() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let operation = TaskRuntimeOperation::new(store);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let operation = tokio::spawn(async move {
            operation
                .run("TaskRuntime operation heartbeat test", move |_store| {
                    let _ignored = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            StoreError::InvalidPlan(format!(
                                "TaskRuntime operation release failed: {error}"
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
            .map_err(|error| format!("TaskRuntime operation failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_blocking_operation_finishes_after_caller_drop() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let operation = TaskRuntimeOperation::new(store.clone());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_in_operation = completed.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            operation
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
        let operation = TaskRuntimeOperation::new(store.clone());
        let parked_operation = operation.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let parked = tokio::spawn(async move {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            parked_operation.reserve_settlement("parked settlement after seal")
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
        // machine the way the runtime plan integration does.
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
            domain_binding: None,

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

    type ScriptedDispatchResult = Result<(SubagentOutcome, String), String>;

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
        fn respond(self: &Arc<Self>, id: &str, outcome: SubagentOutcome) {
            let full_output = outcome.summary.clone();
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((outcome, full_output)));
        }
        /// Script a bounded parent summary plus a distinct complete review output.
        fn respond_with_output(
            self: &Arc<Self>,
            id: &str,
            outcome: SubagentOutcome,
            full_output: &str,
        ) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((outcome, full_output.to_string())));
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

    async fn wait_for_fixture_task_commits(
        store: &TaskRuntimeStore,
        run_id: &str,
        task_ids: &[String],
        progress: &tokio::sync::Notify,
    ) -> Result<(), String> {
        loop {
            let changed = progress.notified();
            let plan = store
                .get_plan(run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "fixture plan missing before interruption".to_string())?;
            if task_ids.iter().all(|id| {
                plan.tasks.iter().any(|task| {
                    task.id == *id
                        && task.status == echo_agent::tasks::TaskStatus::Completed
                        && task.claim.is_none()
                })
            }) {
                return Ok(());
            }
            changed.await;
        }
    }

    impl TaskDispatcher for Arc<ScriptedDispatcher> {
        fn dispatch(
            &self,
            _store: Arc<TaskRuntimeStore>,
            _blocking: TaskRuntimeOperation,
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
                        _ = context.cancellation_token().cancelled() => {
                            return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                        }
                        _ = gate.release.notified() => {}
                    }
                }
                // Honor cancellation even in the mock.
                if context.cancellation_token().is_cancelled() {
                    return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                }
                let result = match results {
                    Some(Ok((result, full_output))) => Ok(TaskDispatchSuccess {
                        task_id,
                        outcome: result,
                        full_output,
                        suggested_tasks: Vec::new(),
                    }),
                    Some(Err(error)) => Err(TaskDispatchFailure::failed(task_id, error)),
                    // Default: generic success for unscripted tasks.
                    None => Ok(TaskDispatchSuccess {
                        task_id,
                        outcome: successful_task_result("ok"),
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
            _blocking: TaskRuntimeOperation,
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

    fn successful_task_result(summary: &str) -> SubagentOutcome {
        SubagentOutcome {
            contract_version: 1,
            status: SubagentStatus::Completed,
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
        result.verification.push(SubagentVerification {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "claimed by subagent".to_string(),
            source: SubagentEvidenceSource::Reported,
        });
        result.artifacts.push(SubagentArtifact {
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
            verification.source = SubagentEvidenceSource::Observed;
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
        result.verification.push(SubagentVerification {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "subagent report only".to_string(),
            source: SubagentEvidenceSource::Reported,
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
                outcome: successful_task_result("durable result"),
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
            domain_binding: None,

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
        let _resource_scope = store.test_resource_scope();
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

        let error = launch_task_run_resume(
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
        let running_error = launch_task_run_resume(
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
    async fn task_run_resume_preserves_allow_direct_completion() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        store
            .create_run_with_profile(
                "direct-resume",
                "test",
                "direct-resume-conversation",
                "direct-resume-root",
                DomainProfile::General,
                "inspect and report",
                "agent_autonomous",
                AttendedMode::Attended,
                TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("direct-resume", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("direct-resume", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("direct-resume", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let snapshot = store
            .get_run_state("direct-resume")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct resume snapshot missing".to_string())?;
        let expected = TaskRunResumeIdentity::capture(&snapshot);
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("direct-resume")
                .llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_model_name("direct-resume")
                        .with_response("direct evidence-backed result"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );

        let launch = launch_task_run_resume(
            store.clone(),
            expected,
            agent,
            None,
            None,
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(
            launch.wait().await.map_err(|error| error.to_string())?,
            RunOutcome::Completed
        ));
        assert!(
            store
                .get_plan("direct-resume")
                .map_err(|error| error.to_string())?
                .is_some(),
            "direct resume must materialize completion evidence"
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
            project_binding: None,
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
                invocation_resources: Vec::new(),
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
        assert_eq!(subagent_run.status, SubagentStatus::Completed);
        let outcome = subagent_run
            .outcome
            .as_ref()
            .ok_or_else(|| "SubagentRun outcome is missing".to_string())?;
        assert!(outcome.summary.contains(REMOTE_MARKER));
        assert!(!outcome.summary.contains("LOCAL_AGENT_MUST_NOT_RUN"));
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
        let committed_progress = Arc::new(tokio::sync::Notify::new());
        let progress_for_sink = committed_progress.clone();
        let completion_sink: ExecSink = Arc::new(move |event| {
            if event.event == RuntimeEventKind::TaskCompleted {
                progress_for_sink.notify_one();
            }
        });
        let completed_ids = tasks
            .iter()
            .take(4)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_store = store.clone();
        let run_dispatcher = dispatcher.clone();
        let run_id_for_task = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan_with_admission(
                run_store,
                run_dispatcher,
                None,
                &run_id_for_task,
                EkoExecutionLimits {
                    max_concurrent_subagents: 8,
                    ..EkoExecutionLimits::default()
                },
                run_cancel,
                Some(completion_sink),
                Arc::new(ExecutionAdmission::with_capacity(
                    EkoExecutionLimits::default().max_concurrent_subagents,
                )),
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &cancelled_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
            wait_for_fixture_task_commits(&store, &run_id, &completed_ids, &committed_progress)
                .await
        })
        .await
        .map_err(|_| "dispatch/cancellation boundary was not reached".to_string())??;
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
        let committed_progress = Arc::new(tokio::sync::Notify::new());
        let progress_for_sink = committed_progress.clone();
        let completion_sink: ExecSink = Arc::new(move |event| {
            if event.event == RuntimeEventKind::TaskCompleted {
                progress_for_sink.notify_one();
            }
        });
        let completed_ids = tasks
            .iter()
            .take(4)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let cancel = CancellationToken::new();
        let execution = {
            let run_store = store.clone();
            let run_dispatcher = dispatcher.clone();
            let run_id = run_id.clone();
            let run_cancel = cancel.clone();
            tokio::spawn(async move {
                execute_runtime_plan_with_admission(
                    run_store,
                    run_dispatcher,
                    None,
                    &run_id,
                    EkoExecutionLimits {
                        max_concurrent_subagents: 8,
                        ..EkoExecutionLimits::default()
                    },
                    run_cancel,
                    Some(completion_sink),
                    Arc::new(ExecutionAdmission::with_capacity(
                        EkoExecutionLimits::default().max_concurrent_subagents,
                    )),
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &paused_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
            wait_for_fixture_task_commits(&store, &run_id, &completed_ids, &committed_progress)
                .await
        })
        .await
        .map_err(|_| "dispatch/pause boundary was not reached".to_string())??;
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
    async fn runtime_plan_reuses_durable_subagent_outcome_after_restart() -> Result<(), String> {
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
                outcome: Some(&recovered_result),
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
            execute_runtime_plan_with_admission(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
                Arc::new(ExecutionAdmission::with_capacity(
                    EkoExecutionLimits::default().max_concurrent_subagents,
                )),
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
                        task: Box::new(second.spec()),
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
            domain_binding: None,

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
            execute_runtime_plan_with_admission(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
                Arc::new(ExecutionAdmission::with_capacity(
                    EkoExecutionLimits::default().max_concurrent_subagents,
                )),
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

    #[derive(Default, Debug)]
    struct VerificationBackgroundCapacityObservation {
        requests: usize,
        launches: usize,
        searches: usize,
        waits: usize,
        last_wait_id: Option<String>,
        receipt: Option<echo_agent::tools::cell::CommandCellLaunchReceipt>,
        terminal: Option<echo_agent::tools::cell::CommandCellDelta>,
        wait_deltas: Vec<echo_agent::tools::cell::CommandCellDelta>,
        output: String,
        shell_available_by_request: Vec<usize>,
        final_answer_issued: bool,
    }

    #[derive(Default)]
    struct VerificationBackgroundCapacityClient {
        observation: std::sync::Mutex<VerificationBackgroundCapacityObservation>,
    }

    impl VerificationBackgroundCapacityClient {
        fn next_script(
            &self,
            request: &echo_agent::llm::ChatRequest,
        ) -> echo_agent::error::Result<echo_agent::testing::MockLlmClient> {
            use echo_agent::error::ReactError;
            use echo_agent::testing::MockLlmClient;
            use echo_agent::tools::cell::{CommandCellDelta, CommandCellLaunchReceipt};

            let mut observed = self.observation.lock().map_err(|error| {
                ReactError::Other(format!("verification fixture observation lock: {error}"))
            })?;
            observed.requests = observed.requests.saturating_add(1);
            observed
                .shell_available_by_request
                .push(PROCESS_EXECUTION_GOVERNOR.shell.available_permits());
            let visible = request.tools.as_deref().unwrap_or_default();
            let exposed = |name: &str| {
                visible
                    .iter()
                    .any(|definition| definition.function.name == name)
            };
            let shell_result = request.messages.iter().rev().find(|message| {
                matches!(message.role, echo_agent::llm::types::Role::Tool)
                    && message.name.as_deref() == Some("shell")
                    && message.tool_call_id.as_deref() == Some("verification-background-launch")
            });
            if let Some(message) = shell_result {
                let content = message.text_content().ok_or_else(|| {
                    ReactError::Other(
                        "verification fixture: shell receipt text missing".to_string(),
                    )
                })?;
                let receipt: CommandCellLaunchReceipt = serde_json::from_str(content.as_ref())
                    .map_err(|error| {
                        ReactError::Other(format!(
                            "verification fixture: original shell receipt decode failed: {error}"
                        ))
                    })?;
                if observed
                    .receipt
                    .as_ref()
                    .is_some_and(|prior| prior != &receipt)
                {
                    return Err(ReactError::Other(
                        "verification fixture: original cell receipt changed".to_string(),
                    ));
                }
                observed.receipt = Some(receipt);
            }
            let wait_result = request.messages.iter().rev().find(|message| {
                matches!(message.role, echo_agent::llm::types::Role::Tool)
                    && message.name.as_deref() == Some("wait")
                    && message
                        .tool_call_id
                        .as_deref()
                        .is_some_and(|call_id| call_id.starts_with("verification-background-wait-"))
            });
            let mut cursor = 0;
            if let Some(message) = wait_result {
                let content = message.text_content().ok_or_else(|| {
                    ReactError::Other("verification fixture: wait receipt text missing".to_string())
                })?;
                let delta: CommandCellDelta =
                    serde_json::from_str(content.as_ref()).map_err(|error| {
                        ReactError::Other(format!(
                            "verification fixture: original wait receipt decode failed: {error}"
                        ))
                    })?;
                let receipt = observed.receipt.as_ref().ok_or_else(|| {
                    ReactError::Other(
                        "verification fixture: wait preceded shell receipt".to_string(),
                    )
                })?;
                if delta.snapshot.cell_id != receipt.cell_id {
                    return Err(ReactError::Other(
                        "verification fixture: wait returned a foreign cell".to_string(),
                    ));
                }
                if observed.last_wait_id != message.tool_call_id {
                    observed.wait_deltas.push(delta.clone());
                    observed.output.push_str(&delta.new_output);
                    observed.last_wait_id = message.tool_call_id.clone();
                }
                cursor = delta.next_cursor;
                if delta.snapshot.phase.is_terminal() {
                    observed.terminal = Some(delta);
                    observed.final_answer_issued = true;
                    return Ok(MockLlmClient::new().with_response("背景验证完成。"));
                }
            }

            let desired = if observed.receipt.is_none() {
                "shell"
            } else {
                "wait"
            };
            if !exposed(desired) {
                if !exposed("tool_search") || observed.searches >= 3 {
                    return Err(ReactError::Other(format!(
                        "verification fixture: original tool exposure did not activate {desired}"
                    )));
                }
                observed.searches = observed.searches.saturating_add(1);
                return Ok(MockLlmClient::new().then_tool_call(
                    format!("verification-background-search-{}", observed.searches),
                    "tool_search",
                    serde_json::json!({"query": desired, "limit": 1}).to_string(),
                ));
            }
            if desired == "shell" {
                if observed.launches != 0 {
                    return Err(ReactError::Other(
                        "verification fixture: launched shell has no original receipt".to_string(),
                    ));
                }
                observed.launches = observed.launches.saturating_add(1);
                return Ok(MockLlmClient::new().then_tool_call(
                    "verification-background-launch",
                    "shell",
                    serde_json::json!({
                        "command": "echo verification-background-cell-effect",
                        "background": true,
                        "timeout": 5
                    })
                    .to_string(),
                ));
            }
            if observed.waits >= 8 {
                return Err(ReactError::Other(
                    "verification fixture: original cell never reached a terminal observation"
                        .to_string(),
                ));
            }
            let cell_id = observed
                .receipt
                .as_ref()
                .map(|receipt| receipt.cell_id.clone())
                .ok_or_else(|| {
                    ReactError::Other("verification fixture: cell receipt missing".to_string())
                })?;
            observed.waits = observed.waits.saturating_add(1);
            Ok(MockLlmClient::new().then_tool_call(
                format!("verification-background-wait-{}", observed.waits),
                "wait",
                serde_json::json!({
                    "cell_id": cell_id,
                    "cursor": cursor,
                    "yield_time_ms": if observed.waits == 1 { 0 } else { 6000 }
                })
                .to_string(),
            ))
        }
    }

    impl echo_agent::llm::LlmClient for VerificationBackgroundCapacityClient {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn chat(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<'_, echo_agent::error::Result<echo_agent::llm::ChatResponse>> {
            let selected = self.next_script(&request);
            Box::pin(async move {
                let script = selected?;
                echo_agent::llm::LlmClient::chat(&script, request).await
            })
        }

        fn chat_stream(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<
            '_,
            echo_agent::error::Result<
                BoxStream<'static, echo_agent::error::Result<echo_agent::llm::ChatChunk>>,
            >,
        > {
            let selected = self.next_script(&request);
            Box::pin(async move {
                let script = selected?;
                echo_agent::llm::LlmClient::chat_stream(&script, request).await
            })
        }
    }

    #[tokio::test]
    async fn primary_verification_background_cell_runs_before_parent_releases_shell_slot()
    -> Result<(), String> {
        let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::open_for_workspace(fixture.path().join("tasks"), "ws_test")
                .map_err(|error| error.to_string())?,
        );
        let _resource_scope = store.test_resource_scope();
        let run_id = seed_run(
            &store,
            vec![PlanTask {
                id: "verification-background-capacity".to_string(),
                title: "验证实际父任务内的后台命令容量".to_string(),
                description: "通过真实 shell(background=true) 启动一个 echo，消费原回执并 wait 到终态后结束。".to_string(),
                kind: PlanTaskKind::Verification,
                agent_role: "primary".to_string(),
                allowed_tools: vec!["tool_search".to_string(), "shell".to_string(), "wait".to_string()],
                ..Default::default()
            }],
        )?;
        let execution_scope = crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_raw("ws_test".to_string()),
            fixture.path(),
        );
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "verification fixture run missing".to_string())?;
        assert_eq!(store.active_workspace_id(), run.workspace_id);
        assert_eq!(run.workspace_id, execution_scope.workspace_id());
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let command_cells = super::super::command_cells::CommandCellRuntimeService::new(
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            Arc::new(
                crate::chat_event_log::ChatEventLog::open(
                    fixture.path().join("chat-events"),
                    Default::default(),
                )
                .map_err(|error| error.to_string())?,
            ),
            product_data_io.clone(),
        )?;
        // 真实 factory 与 execute_run 负责父 Verification 的许可；测试不手工占槽、
        // 不提前释放父许可。只有模型传输受控，shell/wait、sandbox 与所有权交接走原路径。
        let created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                system_prompt: Some(
                    "只执行本次受控背景 shell/wait 验证，不委派或修改任务。".to_string(),
                ),
                working_dir: Some(fixture.path().to_path_buf()),
                workspace_data_root: Some(fixture.path().join("product-data")),
                execution_scope: Some(execution_scope),
                task_runtime_store: Some(store.clone()),
                command_cell_runtime: Some(command_cells.clone()),
                product_data_io: Some(product_data_io.clone()),
                ..Default::default()
            },
            &crate::config::EkoConfig::default(),
        )
        .await?;
        let client = Arc::new(VerificationBackgroundCapacityClient::default());
        let mut agent = created.agent;
        agent.set_llm_client(client.clone());
        let agent = crate::agent_handle::AgentHandle::new(agent);
        let cancel = CancellationToken::new();
        let (execution, fixture_timed_out) = {
            let pending = execute_run(
                store.clone(),
                Some(agent.clone()),
                None,
                None,
                None,
                None,
                &run_id,
                cancel.clone(),
                super::super::memory_bridge::MemoryPolicy::None,
                None,
            );
            tokio::pin!(pending);
            match tokio::time::timeout(std::time::Duration::from_secs(20), pending.as_mut()).await {
                Ok(result) => (result, false),
                Err(_) => {
                    cancel.cancel();
                    let closing = command_cells.begin_shutdown();
                    let settled = pending.await;
                    closing?;
                    (settled, true)
                }
            }
        };
        // 先结算原 owner，再检查目标断言；失败也不能给后续测试留下全局 shell 许可。
        let command_shutdown = command_cells.shutdown().await;
        drop(agent);
        drop(created.model_consumers);
        drop(created.command_cell_runtime);
        drop(command_cells);
        let io_shutdown = product_data_io.join_shutdown().await;
        let available_after_parent = PROCESS_EXECUTION_GOVERNOR.shell.available_permits();
        let observed = std::mem::take(
            &mut *client
                .observation
                .lock()
                .map_err(|error| error.to_string())?,
        );
        eprintln!(
            "verification background capacity: observation={observed:?}; execution={execution:?}; parent_after={available_after_parent}; fixture_timeout={fixture_timed_out}"
        );
        command_shutdown?;
        io_shutdown?;
        if fixture_timed_out {
            return Err(
                "verification fixture exceeded20s; this is not the target capacity RED".to_string(),
            );
        }
        if observed.receipt.is_none() || observed.waits == 0 || !observed.final_answer_issued {
            return Err(
                "verification fixture did not consume the original shell/wait receipts".to_string(),
            );
        }
        let terminal = observed
            .terminal
            .ok_or_else(|| "verification fixture lacks a terminal cell receipt".to_string())?;
        assert_eq!(observed.launches, 1);
        assert_eq!(available_after_parent, 1);
        assert_eq!(
            terminal.snapshot.exit_code,
            Some(0),
            "original Verification parent prevented its background cell from executing before parent settlement"
        );
        assert!(
            observed
                .output
                .contains("verification-background-cell-effect"),
            "real sandbox effect was not observed before the parent verification returned"
        );
        assert!(matches!(
            execution.map_err(|error| error.to_string())?,
            RunOutcome::Completed
        ));
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn final_factory_managers_share_one_physical_slot_across_workspace_shell_cell_and_code()
    -> Result<(), String> {
        use echo_agent::tools::cell::{
            CommandCellDelta, CommandCellLaunchReceipt, CommandCellPhase,
        };
        use echo_agent::tools::{ToolContext, ToolParameters};
        let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
        let cwd_a = fixture.path().join("workspace-a");
        let cwd_b = fixture.path().join("workspace-b");
        for path in [&cwd_a, &cwd_b] {
            std::fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        let store_a = Arc::new(
            TaskRuntimeStore::open_for_workspace(
                fixture.path().join("tasks-a"),
                "mixed-workspace-a",
            )
            .map_err(|error| error.to_string())?,
        );
        let store_b = Arc::new(
            TaskRuntimeStore::open_for_workspace(
                fixture.path().join("tasks-b"),
                "mixed-workspace-b",
            )
            .map_err(|error| error.to_string())?,
        );
        let _lifetime_a = store_a.test_resource_scope();
        let _lifetime_b = store_b.test_resource_scope();
        let scope_a = crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_raw("mixed-workspace-a".to_string()),
            &cwd_a,
        );
        let scope_b = crate::workspace::WorkspaceExecutionScope::workspace(
            &crate::workspace::WorkspaceId::from_raw("mixed-workspace-b".to_string()),
            &cwd_b,
        );
        let io = crate::product_data_io::ProductDataIoService::new();
        let occupancy_root = fixture.path().join("atomic-occupancy");
        std::fs::create_dir(&occupancy_root).map_err(|error| error.to_string())?;
        let mut manager = echo_agent::sandbox::SandboxManager::with_configs(
            echo_agent::sandbox::LocalConfig {
                allowed_write_paths: vec![occupancy_root.clone()],
                ..Default::default()
            },
            None,
            None,
            echo_agent::sandbox::SandboxPolicy::local_os(),
        );
        manager.set_allow_fallback(false);
        let sandbox: Arc<dyn echo_agent::sandbox::SandboxExecutor> = Arc::new(manager);
        // 必须实际具备原 run_code 隔离；缺失时失败，不用假 capability 或降级执行代签。
        if !sandbox
            .is_available_at(echo_agent::sandbox::IsolationLevel::OsSandbox)
            .await
        {
            return Err(
                "mixed workspace fixture requires the actual OS-isolated local backend".to_string(),
            );
        }
        let cells = super::super::command_cells::CommandCellRuntimeService::new(
            sandbox.clone(),
            Arc::new(
                crate::chat_event_log::ChatEventLog::open(
                    fixture.path().join("chat-events"),
                    Default::default(),
                )
                .map_err(|error| error.to_string())?,
            ),
            io.clone(),
        )?;
        let created_a = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                working_dir: Some(cwd_a.clone()),
                workspace_data_root: Some(fixture.path().join("product-a")),
                execution_scope: Some(scope_a),
                task_runtime_store: Some(store_a),
                command_cell_runtime: Some(cells.clone()),
                product_data_io: Some(io.clone()),
                sandbox_executor: Some(sandbox.clone()),
                ..Default::default()
            },
            &crate::config::EkoConfig::default(),
        )
        .await?;
        let created_b = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                working_dir: Some(cwd_b.clone()),
                workspace_data_root: Some(fixture.path().join("product-b")),
                execution_scope: Some(scope_b.clone()),
                task_runtime_store: Some(store_b.clone()),
                command_cell_runtime: Some(cells.clone()),
                product_data_io: Some(io.clone()),
                sandbox_executor: Some(sandbox.clone()),
                ..Default::default()
            },
            &crate::config::EkoConfig::default(),
        )
        .await?;
        let registry_b = cells.scoped_with_executor(scope_b, Some(store_b), sandbox);
        let manager_a = created_a.agent.tool_manager().clone();
        let manager_b = created_b.agent.tool_manager().clone();
        let cancel = Arc::new(CancellationToken::new());
        let make_context = |cwd: &std::path::Path, id: &str| {
            // 与普通 Chat 的原调用合同一致：本次根用户消息同时绑定 turn 与 message，
            // 由 ShellTool 传入原 cell owner，不能在产品层免除根消息身份校验。
            let root_message_id = format!("mixed-{id}-root-message");
            ToolContext {
                working_dir: Some(cwd.to_path_buf()),
                conversation_id: Some(format!("mixed-{id}")),
                turn_id: Some(root_message_id.clone()),
                message_id: Some(root_message_id),
                execution_id: Some(format!("mixed-{id}-execution")),
                call_id: Some(format!("mixed-{id}-call")),
                cancel: Some(cancel.clone()),
                ..Default::default()
            }
        };
        let quote_path =
            |path: &std::path::Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let occupancy_path = quote_path(&occupancy_root.join("occupied"));
        let overlap_path = quote_path(&occupancy_root.join("overlap"));
        let payload = |id: &str| {
            format!(
                "occupancy={occupancy_path}; overlap={overlap_path}; if ! mkdir \"$occupancy\"; then printf overlap > \"$overlap\"; exit 91; fi; trap 'rmdir live-{id}; rmdir \"$occupancy\"' EXIT; mkdir live-{id} || exit 92; echo $$ > pid-{id}; printf started > started-{id}; while [ ! -e release-{id} ]; do sleep 0.01; done; printf mixed-{id}-effect"
            )
        };
        let foreground_context = make_context(&cwd_a, "foreground");
        let foreground_parameters: ToolParameters = serde_json::from_value(serde_json::json!({
            "command": payload("foreground"), "timeout": 5,
        }))
        .map_err(|error| error.to_string())?;
        // 本用例不请求模型；直接经过 factory 最终 ToolManager 与原工具/物理后端。
        // 不注入 provider、不手动占槽或释放许可，不替换命令 executor。
        let foreground = tokio::spawn(async move {
            manager_a
                .execute_tool_with_context("shell", foreground_parameters, &foreground_context)
                .await
                .map_err(|error| error.to_string())
        });
        let first_started = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !cwd_a.join("started-foreground").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        let background_context = make_context(&cwd_b, "background");
        let background_parameters: ToolParameters = serde_json::from_value(serde_json::json!({
            "command": payload("background"), "background": true, "timeout": 5,
        }))
        .map_err(|error| error.to_string())?;
        // 同一原 5 秒观察绝对期限只捕获一次；后续输出增量不会重启预算。
        let cell_observation_deadline = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .ok_or("mixed cell observation deadline overflow")?;
        let background_manager = manager_b.clone();
        let mut background = tokio::spawn(async move {
            background_manager
                .execute_tool_with_context("shell", background_parameters, &background_context)
                .await
                .map_err(|error| error.to_string())
        });
        let code_context = make_context(&cwd_b, "code");
        let code_parameters: ToolParameters = serde_json::from_value(serde_json::json!({
            "language": "bash", "code": payload("code"), "timeout": 5,
        }))
        .map_err(|error| error.to_string())?;
        let code = tokio::spawn(async move {
            manager_b
                .execute_tool_with_context("run_code", code_parameters, &code_context)
                .await
                .map_err(|error| error.to_string())
        });
        let background_early =
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut background).await;
        let launch_returned_before_foreground_release = background_early.is_ok();
        let background_result = background_early.ok();
        let parse_early_receipt = || -> Result<CommandCellLaunchReceipt, String> {
            let result = background_result
                .as_ref()
                .ok_or("background launch did not return while foreground held capacity")?
                .as_ref()
                .map_err(|error| error.to_string())?
                .as_ref()
                .map_err(Clone::clone)?;
            if !result.success {
                return Err(format!(
                    "background launch returned failure, not a receipt: {result:?}"
                ));
            }
            serde_json::from_value(
                result
                    .data
                    .clone()
                    .ok_or("background launch has no original receipt")?,
            )
            .map_err(|error| error.to_string())
        };
        let early_receipt = parse_early_receipt();
        let receipt_before_foreground_release = early_receipt.is_ok();
        let queued_before_release: Result<CommandCellDelta, String> = async {
            let receipt = early_receipt.as_ref().map_err(Clone::clone)?;
            registry_b
                .wait(&receipt.cell_id, 0, 0)
                .await
                .map_err(|error| error.to_string())
        }
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let initial_second_started =
            cwd_b.join("started-background").exists() || cwd_b.join("started-code").exists();
        let available_while_foreground_live = PROCESS_EXECUTION_GOVERNOR.shell.available_permits();
        let operations = [
            (&cwd_a, "foreground"),
            (&cwd_b, "background"),
            (&cwd_b, "code"),
        ];
        let observation = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                // live/started 只用于释放夹具门闩；峰值判据由负载自己的原子占用与永久冲突记录提供。
                // 冲突直接记录并失败，绝不另排队以掩盖产品没有限制真实并发的问题。
                for (cwd, id) in operations {
                    if cwd.join(format!("started-{id}")).exists() {
                        std::fs::write(cwd.join(format!("release-{id}")), "release")
                            .map_err(|error| error.to_string())?;
                    }
                }
                if operations
                    .iter()
                    .all(|(cwd, id)| cwd.join(format!("started-{id}")).exists())
                {
                    return Ok::<(), String>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        // 无论目标观察是否失败，先打开全部自有门闩并取消失败样本，再收集原任务和 cell owner。
        let mut release_errors = Vec::new();
        for (cwd, id) in operations {
            if let Err(error) = std::fs::write(cwd.join(format!("release-{id}")), "release") {
                release_errors.push(error.to_string());
            }
        }
        if observation.is_err()
            || observation.as_ref().is_ok_and(|result| result.is_err())
            || !release_errors.is_empty()
        {
            cancel.cancel();
        }
        let foreground_result = foreground.await.map_err(|error| error.to_string());
        let code_result = code.await.map_err(|error| error.to_string());
        let background_result = match background_result {
            Some(result) => result.map_err(|error| error.to_string()),
            None => background.await.map_err(|error| error.to_string()),
        };
        let terminal: Result<(CommandCellDelta, String), String> = async {
            let result = background_result
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .map_err(Clone::clone)?;
            let receipt: CommandCellLaunchReceipt = serde_json::from_value(
                result
                    .data
                    .clone()
                    .ok_or_else(|| format!("no original receipt: {result:?}"))?,
            )
            .map_err(|error| error.to_string())?;
            tokio::time::timeout_at(cell_observation_deadline, async {
                let mut cursor = 0;
                let mut output = String::new();
                loop {
                    let remaining = cell_observation_deadline
                        .saturating_duration_since(tokio::time::Instant::now());
                    let yield_ms = u64::try_from(remaining.as_millis())
                        .unwrap_or(u64::MAX)
                        .min(5000);
                    let delta = registry_b
                        .wait(&receipt.cell_id, cursor, yield_ms)
                        .await
                        .map_err(|error| error.to_string())?;
                    output.push_str(&delta.new_output);
                    cursor = delta.next_cursor;
                    if delta.snapshot.phase.is_terminal() {
                        return Ok((delta, output));
                    }
                }
            })
            .await
            .map_err(|_| "original background cell has no terminal receipt".to_string())?
        }
        .await;
        let shutdown = cells.shutdown().await;
        drop(created_a);
        drop(created_b);
        drop(registry_b);
        drop(cells);
        let io_shutdown = io.join_shutdown().await;
        let available_after = PROCESS_EXECUTION_GOVERNOR.shell.available_permits();
        eprintln!(
            "mixed factory workspace: first_started={first_started}; launch_returned_before_release={launch_returned_before_foreground_release}; receipt_before_release={receipt_before_foreground_release}; early_receipt={early_receipt:?}; initial_second_started={initial_second_started}; queued={queued_before_release:?}; overlap={}; available_held={available_while_foreground_live}; available_after={available_after}; foreground={foreground_result:?}; code={code_result:?}; terminal={terminal:?}",
            occupancy_root.join("overlap").exists()
        );
        shutdown?;
        io_shutdown?;
        observation.map_err(|_| {
            "mixed payloads did not all start before the original command deadlines".to_string()
        })??;
        if !release_errors.is_empty() {
            return Err(release_errors.join("; "));
        }
        let foreground_result = foreground_result??;
        let code_result = code_result??;
        let (terminal, background_output) = terminal?;
        let queued_before_release = queued_before_release?;
        assert!(first_started && receipt_before_foreground_release);
        assert!(!initial_second_started);
        assert_eq!(available_while_foreground_live, 0);
        assert_eq!(
            queued_before_release.snapshot.phase,
            CommandCellPhase::Queued
        );
        assert!(
            !occupancy_root.join("overlap").exists(),
            "real gated payloads overlapped"
        );
        assert!(
            !occupancy_root.join("occupied").exists(),
            "original payload did not release its atomic occupancy"
        );
        assert_eq!(available_after, 1);
        assert!(
            foreground_result.success
                && foreground_result.output.contains("mixed-foreground-effect")
        );
        assert!(code_result.success && code_result.output.contains("mixed-code-effect"));
        assert_eq!(terminal.snapshot.exit_code, Some(0));
        assert!(background_output.contains("mixed-background-effect"));
        Ok(())
    }

    #[derive(Clone, Debug)]
    struct PrimaryVerificationToolRequest {
        names: Vec<String>,
        has_tools: bool,
        schema_bytes: usize,
        estimated_tokens: usize,
        tool_result_names: Vec<String>,
        search_receipt_count: usize,
        search_activated_list_dir: bool,
        search_no_eligible_match: bool,
    }

    #[derive(Default)]
    struct PrimaryVerificationMetadataClient {
        requests: std::sync::Mutex<Vec<PrimaryVerificationToolRequest>>,
        script: Option<Arc<echo_agent::testing::MockLlmClient>>,
    }

    impl PrimaryVerificationMetadataClient {
        fn observe(&self, request: &echo_agent::llm::ChatRequest) -> echo_agent::error::Result<()> {
            let definitions = request.tools.as_deref().unwrap_or_default();
            let stats =
                echo_agent::tools::ToolManager::schema_stats_for(definitions).map_err(|_| {
                    echo_agent::error::ReactError::Other(
                        "无法统计验证请求的工具 schema".to_string(),
                    )
                })?;
            let mut names = definitions
                .iter()
                .map(|definition| definition.function.name.clone())
                .collect::<Vec<_>>();
            names.sort_unstable();
            // 只投影原运行时实际交给下一请求的工具回执，不注入消息或保存正文。
            let tool_results = request
                .messages
                .iter()
                .filter(|message| matches!(message.role, echo_agent::llm::types::Role::Tool))
                .collect::<Vec<_>>();
            let tool_result_names = tool_results
                .iter()
                .filter_map(|message| message.name.clone())
                .collect::<Vec<_>>();
            let search_receipts = tool_results
                .iter()
                .filter(|message| {
                    message.name.as_deref() == Some("tool_search")
                        && message.tool_call_id.as_deref() == Some("tool-search-request-consumer")
                })
                .collect::<Vec<_>>();
            let search_activated_list_dir = search_receipts.iter().any(|message| {
                message.text_content().is_some_and(|content| {
                    content.starts_with("Activated tool schemas for the next turn:")
                        && content.lines().any(|line| line.starts_with("- list_dir:"))
                })
            });
            let search_no_eligible_match = search_receipts.iter().any(|message| {
                message.text_content().is_some_and(|content| {
                    content.starts_with("No eligible tools matched 'list_dir'.")
                })
            });
            // 只在实际模型边界保留无内容元数据；不保存 prompt、参数 schema 或整个请求。
            self.requests
                .lock()
                .map_err(|_| {
                    echo_agent::error::ReactError::Other("验证请求元数据锁不可用".to_string())
                })?
                .push(PrimaryVerificationToolRequest {
                    names,
                    has_tools: request.tools.is_some(),
                    schema_bytes: stats.schema_bytes,
                    estimated_tokens: stats.estimated_tokens,
                    tool_result_names,
                    search_receipt_count: search_receipts.len(),
                    search_activated_list_dir,
                    search_no_eligible_match,
                });
            Ok(())
        }
    }

    impl echo_agent::llm::LlmClient for PrimaryVerificationMetadataClient {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn chat(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<'_, echo_agent::error::Result<echo_agent::llm::ChatResponse>> {
            let observed = self.observe(&request);
            if let Some(script) = self.script.as_ref() {
                return Box::pin(async move {
                    observed?;
                    echo_agent::llm::LlmClient::chat(script.as_ref(), request).await
                });
            }
            drop(request);
            Box::pin(async move {
                observed?;
                Ok(echo_agent::llm::ChatResponse {
                    message: echo_agent::llm::types::Message::assistant("验证完成。".to_owned()),
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                    raw: Default::default(),
                })
            })
        }

        fn chat_stream(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<
            '_,
            echo_agent::error::Result<
                BoxStream<'static, echo_agent::error::Result<echo_agent::llm::ChatChunk>>,
            >,
        > {
            let observed = self.observe(&request);
            if let Some(script) = self.script.as_ref() {
                return Box::pin(async move {
                    observed?;
                    echo_agent::llm::LlmClient::chat_stream(script.as_ref(), request).await
                });
            }
            drop(request);
            Box::pin(async move {
                observed?;
                let chunks: BoxStream<
                    'static,
                    echo_agent::error::Result<echo_agent::llm::ChatChunk>,
                > = Box::pin(stream::iter(vec![
                    Ok(echo_agent::llm::ChatChunk {
                        delta: echo_agent::llm::types::DeltaMessage {
                            content: Some("验证完成。".to_string()),
                            ..Default::default()
                        },
                        finish_reason: None,
                        usage: None,
                    }),
                    Ok(echo_agent::llm::ChatChunk {
                        delta: Default::default(),
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    }),
                ]));
                Ok(chunks)
            })
        }
    }

    async fn primary_verification_actual_requests(
        allowed_tools: &[&str],
        client: Arc<PrimaryVerificationMetadataClient>,
    ) -> Result<(HashSet<String>, Vec<PrimaryVerificationToolRequest>), String> {
        let searching = client.script.is_some();
        let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let run_id = seed_run(
            &store,
            vec![PlanTask {
                id: "verify-model-tools".to_string(),
                title: "核对 primary verification 的实际工具曝光".to_string(),
                description: if searching {
                    "仅调用 tool_search 查询 list_dir，再正常回答验证完成；不执行目标工具。"
                        .to_string()
                } else {
                    "无需执行工具，正常回答验证完成。".to_string()
                },
                kind: PlanTaskKind::Verification,
                agent_role: "primary".to_string(),
                allowed_tools: allowed_tools
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect(),
                ..Default::default()
            }],
        )?;
        let product_data_io = crate::product_data_io::ProductDataIoService::new();
        let command_cells = super::super::command_cells::CommandCellRuntimeService::new(
            Arc::new(echo_agent::sandbox::SandboxManager::local_sandbox()),
            Arc::new(
                crate::chat_event_log::ChatEventLog::open(
                    fixture.path().join("chat-events"),
                    Default::default(),
                )
                .map_err(|error| error.to_string())?,
            ),
            product_data_io.clone(),
        )?;
        // 使用原 EKO factory、真实内置注册和 TaskRuntime 工具；只替换受控模型传输。
        let created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams {
                system_prompt: Some(if searching {
                    "仅搜索延迟工具；收到真实搜索回执后正常回答，不执行目标工具。".to_string()
                } else {
                    "按要求正常回答，不调用工具。".to_string()
                }),
                working_dir: Some(fixture.path().to_path_buf()),
                workspace_data_root: Some(fixture.path().join("product-data")),
                task_runtime_store: Some(store.clone()),
                command_cell_runtime: Some(command_cells.clone()),
                product_data_io: Some(product_data_io.clone()),
                ..Default::default()
            },
            &crate::config::EkoConfig::default(),
        )
        .await?;
        let mut agent = created.agent;
        let registered = agent.tool_names().into_iter().collect::<HashSet<_>>();
        agent.set_llm_client(client.clone());
        let agent = crate::agent_handle::AgentHandle::new(agent);
        let execution = execute_run(
            store.clone(),
            Some(agent.clone()),
            None,
            None,
            None,
            None,
            &run_id,
            CancellationToken::new(),
            super::super::memory_bridge::MemoryPolicy::None,
            None,
        )
        .await;
        // 原 owner 先结算，再检查断言，避免失败时遗留 command-cell 观察任务。
        let command_shutdown = command_cells.shutdown().await;
        drop(agent);
        drop(created.model_consumers);
        drop(created.command_cell_runtime);
        drop(command_cells);
        let io_shutdown = product_data_io.join_shutdown().await;
        command_shutdown?;
        io_shutdown?;
        assert!(matches!(
            execution.map_err(|error| error.to_string())?,
            RunOutcome::Completed
        ));
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "验证 TaskRun 缺失".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let subagent_runs = store
            .list_subagent_runs(&run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(subagent_runs.len(), 1);
        let subagent_run = subagent_runs
            .first()
            .ok_or_else(|| "原 primary verification 执行记录缺失".to_string())?;
        assert_eq!(subagent_run.task_id, "verify-model-tools");
        assert_eq!(subagent_run.subagent_name, "primary");
        assert_eq!(subagent_run.status, SubagentStatus::Completed);
        let outcome = subagent_run
            .outcome
            .as_ref()
            .ok_or_else(|| "原 primary verification 未保存正常回答".to_string())?;
        assert!(outcome.summary.contains("验证完成。"));
        let requests = std::mem::take(
            &mut *client
                .requests
                .lock()
                .map_err(|_| "验证元数据锁不可用".to_string())?,
        );
        for (index, request) in requests.iter().enumerate() {
            assert!(request.names.iter().all(|name| registered.contains(name)));
            assert!(
                request.schema_bytes <= 16_000,
                "实际请求 schema 超出 16,000 bytes：{}",
                request.schema_bytes
            );
            assert!(
                request.estimated_tokens <= 4_000,
                "实际请求 schema 超出 4,000 估算 token：{}",
                request.estimated_tokens
            );
            eprintln!(
                "primary verification request={} tools={:?} schema_bytes={} estimated_tokens={}",
                index.saturating_add(1),
                request.names,
                request.schema_bytes,
                request.estimated_tokens
            );
        }
        Ok((registered, requests))
    }

    async fn primary_verification_actual_request(
        allowed_tools: &[&str],
    ) -> Result<(HashSet<String>, PrimaryVerificationToolRequest), String> {
        let (registered, mut requests) = primary_verification_actual_requests(
            allowed_tools,
            Arc::new(PrimaryVerificationMetadataClient::default()),
        )
        .await?;
        assert_eq!(
            requests.len(),
            1,
            "必须真正命中一次模型 API，不能以零请求通过"
        );
        let request = requests
            .pop()
            .ok_or_else(|| "最终模型请求缺失".to_string())?;
        Ok((registered, request))
    }

    #[tokio::test]
    async fn primary_verification_model_tools_default_scope() -> Result<(), String> {
        let (registered, request) = primary_verification_actual_request(&[]).await?;
        for required in [
            "read_file",
            "tool_search",
            "task_create",
            "task_update",
            "task_list",
        ] {
            assert!(
                registered.contains(required),
                "原 factory 必须注册 {required}"
            );
            assert!(
                request.names.iter().any(|name| name == required),
                "缺省请求必须曝光 {required}"
            );
        }
        assert!(request.has_tools);
        // 复用原产品策略作为预期；不在夹具中另建 registry 或实现可见性算法。
        let registered = registered.into_iter().collect::<Vec<_>>();
        let expected = crate::tool_exposure::initial_visible_tools_for_task_run(&registered);
        assert_eq!(request.names.into_iter().collect::<HashSet<_>>(), expected);
        Ok(())
    }

    #[tokio::test]
    async fn primary_verification_model_tools_read_file_bound() -> Result<(), String> {
        let (registered, request) = primary_verification_actual_request(&["read_file"]).await?;
        assert!(registered.contains("read_file"));
        assert!(request.has_tools);
        assert_eq!(request.names, vec!["read_file".to_string()]);
        Ok(())
    }

    #[tokio::test]
    async fn primary_verification_model_tools_disjoint_bound() -> Result<(), String> {
        let missing = "unregistered_primary_verification_probe";
        let (registered, request) = primary_verification_actual_request(&[missing]).await?;
        assert!(
            !registered.contains(missing),
            "反控必须与真实注册目录无交集"
        );
        assert!(request.names.is_empty());
        assert!(
            !request.has_tools,
            "空交集应成为实际 ChatRequest.tools=None，不能回退全量"
        );
        Ok(())
    }

    #[tokio::test]
    async fn primary_verification_tool_search_activation_reaches_next_model_request()
    -> Result<(), String> {
        for target_allowed in [true, false] {
            let allowed_tools = if target_allowed {
                vec!["tool_search", "read_file", "list_dir"]
            } else {
                vec!["tool_search", "read_file"]
            };
            let client = Arc::new(PrimaryVerificationMetadataClient {
                script: Some(Arc::new(
                    echo_agent::testing::MockLlmClient::new()
                        .then_tool_call(
                            "tool-search-request-consumer",
                            "tool_search",
                            r#"{"query":"list_dir","limit":1}"#,
                        )
                        .with_response("验证完成。"),
                )),
                ..Default::default()
            });
            let (registered, requests) =
                primary_verification_actual_requests(&allowed_tools, client).await?;
            for required in ["tool_search", "read_file", "list_dir"] {
                assert!(
                    registered.contains(required),
                    "原 factory 必须注册 {required}"
                );
            }
            assert_eq!(requests.len(), 2, "真实搜索后必须进入下一次模型请求");
            let first = requests
                .first()
                .ok_or_else(|| "首个真实模型请求缺失".to_string())?;
            let second = requests
                .get(1)
                .ok_or_else(|| "搜索后的真实模型请求缺失".to_string())?;
            let first_names = first
                .names
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let second_names = second
                .names
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let hard_bound = allowed_tools.iter().copied().collect::<HashSet<_>>();
            assert!(first.has_tools && second.has_tools);
            assert_eq!(first_names, HashSet::from(["read_file", "tool_search"]));
            assert!(
                !first.names.iter().any(|name| name == "list_dir"),
                "目标必须真实延迟，不能用默认已曝光工具冒充激活"
            );
            for request in &requests {
                assert!(
                    request
                        .names
                        .iter()
                        .all(|name| hard_bound.contains(name.as_str())),
                    "每次实际模型请求都必须保持当前非空 hard bound"
                );
            }
            assert!(first.tool_result_names.is_empty());
            assert_eq!(first.search_receipt_count, 0);
            assert_eq!(second.tool_result_names, vec!["tool_search".to_string()]);
            assert_eq!(
                second.search_receipt_count, 1,
                "原搜索回执必须被下一请求消费"
            );
            if target_allowed {
                assert_eq!(second_names, hard_bound);
                assert!(second.search_activated_list_dir);
                assert!(!second.search_no_eligible_match);
            } else {
                assert_eq!(second_names, first_names);
                assert!(!second.search_activated_list_dir);
                assert!(second.search_no_eligible_match);
            }
        }
        Ok(())
    }

    struct DelegationMessageProbe {
        inner: Arc<echo_agent::testing::MockLlmClient>,
        saw_attachment: Arc<std::sync::atomic::AtomicBool>,
    }

    impl echo_agent::llm::LlmClient for DelegationMessageProbe {
        fn chat(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<'_, echo_agent::error::Result<echo_agent::llm::ChatResponse>> {
            if request.messages.iter().any(|message| {
                matches!(&message.content, echo_agent::llm::types::MessageContent::Parts(parts)
                    if parts.iter().any(|part| matches!(part, echo_agent::llm::types::ContentPart::File { .. })))
            }) {
                self.saw_attachment.store(true, Ordering::SeqCst);
            }
            echo_agent::llm::LlmClient::chat(self.inner.as_ref(), request)
        }
        fn chat_stream(
            &self,
            request: echo_agent::llm::ChatRequest,
        ) -> BoxFuture<
            '_,
            echo_agent::error::Result<
                BoxStream<'static, echo_agent::error::Result<echo_agent::llm::ChatChunk>>,
            >,
        > {
            if request.messages.iter().any(|message| {
                matches!(&message.content, echo_agent::llm::types::MessageContent::Parts(parts)
                    if parts.iter().any(|part| matches!(part, echo_agent::llm::types::ContentPart::File { .. })))
            }) {
                self.saw_attachment.store(true, Ordering::SeqCst);
            }
            echo_agent::llm::LlmClient::chat_stream(self.inner.as_ref(), request)
        }
        fn model_name(&self) -> &str {
            "mock-model"
        }
    }

    async fn actual_subagent_consumer_keeps_run_depth(
        writer: bool,
        attachment: bool,
    ) -> Result<(), String> {
        use crate::subagent_policy::{SubagentDepthDefaults, SubagentDepthLimit};
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::subagent::{SubagentBuilder, SubagentEvent, SubagentRegistry};
        use echo_agent::testing::MockLlmClient;

        for maximum in [1, 2, 3] {
            let defaults = Arc::new(SubagentDepthDefaults::new(SubagentDepthLimit::new(
                maximum,
            )?));
            let store = Arc::new(
                TaskRuntimeStore::new_in_memory()
                    .map_err(|error| error.to_string())?
                    .with_subagent_depth_defaults(defaults.clone()),
            );
            let _resource_scope = store.test_resource_scope();
            let task = PlanTask {
                id: "bound-child".into(),
                title: "验证实际消费者的层数绑定".into(),
                description: "沿真实 Fork 与 agent_tool 执行".into(),
                kind: if writer {
                    PlanTaskKind::Implementation
                } else {
                    PlanTaskKind::ReadOnlyReview
                },
                agent_role: "bound-child-1".into(),
                ..Default::default()
            };
            let run_id = seed_run(&store, vec![task.clone()])?;
            let bound = store
                .get_run_state(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "原 TaskRun 不存在".to_string())?
                .execution_profile
                .require_subagent_depth_limit()?;
            defaults.publish(SubagentDepthLimit::new(if maximum == 1 { 3 } else { 1 })?);
            let policy = bound.first_child_policy()?;
            assert_eq!(policy.delegate_depth, 1);
            assert_eq!(policy.max_delegate_depth, maximum);
            let uploads = tempfile::tempdir().map_err(|error| error.to_string())?;
            if attachment {
                let path = uploads.path().join("中文验证附件.txt");
                std::fs::write(&path, "只读附件，不执行代码").map_err(|error| error.to_string())?;
                store
                    .set_run_attachments(
                        &run_id,
                        &[crate::attachments::AttachmentRef {
                            path,
                            name: "中文验证附件.txt".into(),
                            mime_type: "text/plain".into(),
                            source: crate::types::AttachmentSource::default(),
                        }],
                    )
                    .map_err(|error| error.to_string())?;
            }
            let registry = Arc::new(SubagentRegistry::new());
            let mut events = registry.event_bus().subscribe();
            let saw_attachment = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut actors = Vec::new();
            for depth in 1_u8..=4 {
                let name = format!("bound-child-{depth}");
                let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
                let calls = Arc::new(AtomicUsize::new(0));
                let llm = Arc::new(
                    MockLlmClient::new()
                        .then_tool_call("probe", "depth_probe", r#"{"path":"Cargo.toml"}"#)
                        .then_tool_call("search", "tool_search", r#"{"query":"agent_tool"}"#)
                        .then_tool_call(
                            "delegate",
                            "agent_tool",
                            serde_json::json!({
                                "agent_name":format!("bound-child-{}", depth.saturating_add(1)),
                                "task":"继续真实下一层", "mode":"fork"
                            })
                            .to_string(),
                        )
                        .with_response("## Summary\n层数探针完成"),
                );
                let mut child_builder = ReactAgentBuilder::new()
                    .name(&name)
                    .model("mock-model")
                    .enable_tools()
                    .enable_subagent()
                    .register_agent_dispatch_tool()
                    .subagent_registry(registry.clone())
                    .llm_client(Arc::new(DelegationMessageProbe {
                        inner: llm.clone(),
                        saw_attachment: saw_attachment.clone(),
                    }))
                    .tool(Box::new(
                        PermissionCountingTool::new(
                            "depth_probe",
                            echo_agent::prelude::ToolPermission::Read,
                            calls.clone(),
                        )
                        // 只观察原调用的内存计数/policy，不读写文件；显式声明
                        // ReadOnly 能力，不能把权限 Read 当成 capability 声明。
                        .with_readonly_capability()
                        .with_policy_observer(observations.clone()),
                    ));
                if !writer {
                    child_builder = child_builder.readonly_tools();
                }
                let child = child_builder.build().map_err(|error| error.to_string())?;
                let definition = SubagentBuilder::new(&name).fork_mode().can_delegate();
                let definition = if writer {
                    definition.write_access()
                } else {
                    definition.read_only()
                };
                registry.register(definition.build(), Box::new(child)).await;
                actors.push((depth, calls, observations, llm));
            }
            let primary = crate::agent_handle::AgentHandle::new(
                ReactAgentBuilder::new()
                    .name("primary")
                    .model("mock-model")
                    .enable_tools()
                    .enable_subagent()
                    .subagent_registry(registry)
                    .build()
                    .map_err(|error| error.to_string())?,
            );
            let execution_id = format!("{run_id}:bound-child:1:1");
            // 与真实 task dispatch 共用应用 helper，避免手写 for_run 让消费者
            // 回归绕开跨 Run 身份丢失的实际接缝。
            let (_control_identity, identity) = super::super::subagent_control::attempt_identity(
                &run_id,
                &task.id,
                &execution_id,
                1,
                1,
            )
            .map_err(|error| error.to_string())?;
            let uplink: echo_agent::tools::SubagentUplinkFn = Arc::new(|_| {
                Box::pin(async {
                    echo_agent::tools::SubagentUplinkReceipt {
                        accepted: true,
                        status: "test_no_message".into(),
                        detail: String::new(),
                    }
                })
            });
            let result = if writer {
                run_writer_subagent(
                    &primary,
                    TaskRuntimeOperation::new(store.clone()),
                    &run_id,
                    &execution_id,
                    "bound-isolation",
                    &task.agent_role,
                    &task.description,
                    serde_json::json!({}),
                    SubagentTaskContext::default(),
                    Vec::new(),
                    CancellationToken::new(),
                    policy,
                    None,
                    identity,
                    None,
                    uplink,
                    1,
                    Vec::new(),
                )
                .await
            } else {
                let allowed_tools = vec![
                    "agent_tool".to_string(),
                    "depth_probe".to_string(),
                    "tool_search".to_string(),
                ];
                run_readonly_subagent(
                    &primary,
                    &run_id,
                    &execution_id,
                    None,
                    &task.agent_role,
                    &task.description,
                    serde_json::json!({}),
                    SubagentTaskContext::default(),
                    allowed_tools,
                    CancellationToken::new(),
                    policy,
                    None,
                    identity,
                    None,
                    uplink,
                    1,
                    Vec::new(),
                )
                .await
            };
            result.map_err(|error| error.to_string())?;
            // 必须先经过真实消费者、Fork、模型与工具执行，再核完整 policy 与越界无派发。
            for (depth, calls, observations, llm) in actors {
                assert_eq!(
                    llm.call_count() > 0,
                    depth <= maximum,
                    "writer={writer}, max={maximum}, depth={depth}"
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    usize::from(depth <= maximum),
                    "writer={writer}, max={maximum}, depth={depth}"
                );
                let observed = observations.lock().map_err(|error| error.to_string())?;
                if depth <= maximum {
                    assert_eq!(
                        *observed,
                        vec![Some(echo_agent::tasks::NestedDelegationPolicy {
                            can_spawn_subagents: true,
                            delegate_depth: depth,
                            max_delegate_depth: maximum,
                        })]
                    );
                } else {
                    assert!(observed.is_empty());
                }
            }
            let mut started = Vec::new();
            while let Ok(event) = events.try_recv() {
                if let SubagentEvent::DispatchStarted { agent, mode, .. } = event.as_ref() {
                    assert_eq!(*mode, echo_agent::subagent::ExecutionMode::Fork);
                    started.push(agent.clone());
                }
            }
            assert_eq!(started.len(), usize::from(maximum));
            assert_eq!(saw_attachment.load(Ordering::SeqCst), attachment);
        }
        Ok(())
    }

    #[tokio::test]
    async fn readonly_fork_consumer_preserves_bound_subagent_depth() -> Result<(), String> {
        actual_subagent_consumer_keeps_run_depth(false, false).await
    }

    #[tokio::test]
    async fn writer_text_fork_consumer_preserves_bound_subagent_depth() -> Result<(), String> {
        actual_subagent_consumer_keeps_run_depth(true, false).await
    }

    #[tokio::test]
    async fn writer_message_fork_consumer_preserves_bound_subagent_depth() -> Result<(), String> {
        actual_subagent_consumer_keeps_run_depth(true, true).await
    }

    #[tokio::test]
    async fn primary_verification_keeps_original_depth_and_hard_tool_bound() -> Result<(), String> {
        use crate::subagent_policy::{SubagentDepthDefaults, SubagentDepthLimit};
        use echo_agent::testing::MockLlmClient;
        for original in [1, 3] {
            for allow_probe in [false, true] {
                let defaults = Arc::new(SubagentDepthDefaults::new(SubagentDepthLimit::new(
                    original,
                )?));
                let store = Arc::new(
                    TaskRuntimeStore::new_in_memory()
                        .map_err(|error| error.to_string())?
                        .with_subagent_depth_defaults(defaults.clone()),
                );
                let task = PlanTask {
                    id: "verify-policy".to_string(),
                    title: "Verify bounded invocation".to_string(),
                    description: "Run the permitted probe".to_string(),
                    kind: PlanTaskKind::Verification,
                    agent_role: "primary".to_string(),
                    allowed_tools: vec![
                        if allow_probe { "run_code" } else { "read_file" }.to_string(),
                    ],
                    ..Default::default()
                };
                let run_id = seed_run(&store, vec![task.clone()])?;
                defaults.publish(SubagentDepthLimit::new(if original == 1 { 3 } else { 1 })?);
                let calls = Arc::new(AtomicUsize::new(0));
                let policies = Arc::new(std::sync::Mutex::new(Vec::new()));
                let llm = Arc::new(
                    MockLlmClient::new()
                        .then_tool_call("probe", "run_code", r#"{"path":"Cargo.toml"}"#)
                        .with_response("Probe finished."),
                );
                let agent = echo_agent::agent::ReactAgentBuilder::new()
                    .llm_client(llm.clone())
                    .tool(Box::new(
                        PermissionCountingTool::new(
                            "run_code",
                            echo_agent::prelude::ToolPermission::Read,
                            calls.clone(),
                        )
                        .with_policy_observer(policies.clone()),
                    ))
                    .build()
                    .map_err(|error| error.to_string())?;
                let execution_id = format!("{run_id}:verify-policy:1:1");
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
                let outcome = run_main_agent_task(
                    &crate::agent_handle::AgentHandle::new(agent),
                    TaskRuntimeOperation::new(store),
                    &run_id,
                    &task,
                    &execution_id,
                    "Run the probe only when allowed",
                    CancellationToken::new(),
                    None,
                    None,
                )
                .await;
                assert!(llm.call_count() > 0, "must reach the real Agent/LLM path");
                if allow_probe {
                    outcome.map_err(|error| error.to_string())?;
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    let observed = policies.lock().map_err(|error| error.to_string())?;
                    let policy = observed
                        .first()
                        .and_then(|policy| *policy)
                        .ok_or_else(|| "primary ToolContext lost its bound policy".to_string())?;
                    assert_eq!(policy.delegate_depth, 0);
                    assert_eq!(policy.max_delegate_depth, original);
                } else {
                    assert_eq!(
                        calls.load(Ordering::SeqCst),
                        0,
                        "a hallucinated registered tool must not bypass nonempty W"
                    );
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn main_agent_task_streams_tool_events_to_subagent_trace() -> Result<(), String> {
        use crate::agent_handle::AgentHandle;
        use echo_agent::agent::ReactAgentBuilder;
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
            TaskRuntimeOperation::new(store),
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
        let result = SubagentOutcome {
            contract_version: 0, // plain-text fallback, NOT a failure
            status: SubagentStatus::Completed,
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
        let result = SubagentOutcome {
            contract_version: 0,
            status: SubagentStatus::Completed,
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
            SubagentOutcome {
                contract_version: 0,
                status: SubagentStatus::Completed,
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
            SubagentOutcome {
                contract_version: 1,
                status: SubagentStatus::Completed,
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
            SubagentOutcome {
                contract_version: 1,
                status: SubagentStatus::Completed,
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
            SubagentOutcome {
                contract_version: 1,
                status: SubagentStatus::Completed,
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
    #[test]
    fn live_task_payload_projects_only_committed_settlement() {
        let paused = echo_agent::tasks::TaskStatus::Paused("paused by user".to_string());
        let late = RuntimeTaskResolutionReceipt {
            resolution: echo_agent::tasks::RuntimeTaskResolution::Interrupted {
                disposition: echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
                    reason: "paused by user".to_string(),
                },
            },
            settlement: crate::tasks::task_runtime::SuccessEvidenceSettlement::Late(
                crate::tasks::task_runtime::LateSuccessEvidence::RunInterrupted {
                    run_status: TaskRunStatus::Paused,
                },
            ),
            acceptance: crate::tasks::task_runtime::SuccessEvidenceAcceptance::Rejected,
            committed_summary: Some("settlement superseded by interruption".to_string()),
            committed_status: Some(paused.clone()),
        };
        let staged = serde_json::json!({
            "terminal_status": "completed",
            "summary": "late success",
            "artifacts": [{"path": "late.txt"}],
        });
        let payload = committed_task_payload(&late, staged);
        assert_eq!(payload["terminal_status"], "paused");
        assert_eq!(payload["settlement"], "late");
        assert_eq!(payload["acceptance"], "rejected");
        assert_ne!(payload["summary"], "late success");
        assert!(
            payload.get("artifacts").is_none(),
            "rejected evidence must not expose its artifacts"
        );
        assert_eq!(
            runtime_event_for_committed_status(&paused),
            Some(RuntimeEventKind::TaskBlocked)
        );

        // Defect A shape: the pre-settlement classifier reported `Committed`
        // because the durable claim interruption left the run `Running`, yet
        // the canonical settlement refused the success. The live payload must
        // still degrade: `is_late()` alone would leak the success fields.
        let durable_cancel = echo_agent::tasks::TaskStatus::Cancelled;
        let refused = RuntimeTaskResolutionReceipt {
            resolution: echo_agent::tasks::RuntimeTaskResolution::Interrupted {
                disposition: echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled,
            },
            settlement: crate::tasks::task_runtime::SuccessEvidenceSettlement::Committed,
            acceptance: crate::tasks::task_runtime::SuccessEvidenceAcceptance::Rejected,
            committed_summary: Some("settlement superseded by interruption".to_string()),
            committed_status: Some(durable_cancel.clone()),
        };
        let refused_payload = committed_task_payload(
            &refused,
            serde_json::json!({
                "terminal_status": "completed",
                "summary": "interrupted success",
                "artifacts": [{"path": "refused.txt"}],
                "verification": [{"check": "cargo test", "status": "passed"}],
            }),
        );
        assert_eq!(refused_payload["terminal_status"], "cancelled");
        assert_eq!(refused_payload["acceptance"], "rejected");
        assert_ne!(refused_payload["summary"], "interrupted success");
        assert!(
            refused_payload.get("artifacts").is_none()
                && refused_payload.get("verification").is_none(),
            "settlement-rejected evidence must not expose success payload fields"
        );

        let completed = echo_agent::tasks::TaskStatus::Completed;
        let accepted = RuntimeTaskResolutionReceipt {
            resolution: echo_agent::tasks::RuntimeTaskResolution::Completed,
            settlement: crate::tasks::task_runtime::SuccessEvidenceSettlement::Committed,
            acceptance: crate::tasks::task_runtime::SuccessEvidenceAcceptance::Accepted,
            committed_summary: Some("committed result".to_string()),
            committed_status: Some(completed.clone()),
        };
        let accepted_payload = committed_task_payload(
            &accepted,
            serde_json::json!({
                "terminal_status": "completed",
                "summary": "staged result",
                "artifacts": [{"path": "kept.txt"}],
            }),
        );
        assert_eq!(accepted_payload["terminal_status"], "completed");
        assert_eq!(accepted_payload["summary"], "committed result");
        assert_eq!(accepted_payload["settlement"], "committed");
        assert_eq!(accepted_payload["acceptance"], "accepted");
        assert!(
            accepted_payload.get("artifacts").is_some(),
            "committed evidence keeps its rich payload"
        );
        assert_eq!(
            runtime_event_for_committed_status(&completed),
            Some(RuntimeEventKind::TaskCompleted)
        );
        assert_eq!(
            runtime_event_for_committed_status(&echo_agent::tasks::TaskStatus::Pending),
            None
        );
    }

    /// Attempt-owned execution evidence that looks exactly like success:
    /// artifacts, verification, and touched files all set.
    fn success_execution_summary(run_id: &str, task_id: &str, text: &str) -> TaskExecutionSummary {
        let mut outcome = SubagentOutcome::terminal(SubagentStatus::Completed, text, Vec::new());
        outcome.artifacts = vec![echo_agent::subagent::SubagentArtifact {
            path: "src/settled.rs".to_string(),
            kind: "patch".to_string(),
            bytes: Some(42),
            sha256: None,
            producer_execution_id: None,
            available: true,
        }];
        outcome.verification = vec![echo_agent::subagent::SubagentVerification {
            check: "cargo test -p echo-agent-app-core".to_string(),
            status: echo_agent::subagent::SubagentVerificationStatus::Passed,
            details: "ok".to_string(),
            source: echo_agent::subagent::SubagentEvidenceSource::Reported,
        }];
        outcome.touched_files.written = vec!["src/settled.rs".to_string()];
        TaskExecutionSummary {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            subagent_name: "reviewer".to_string(),
            outcome,
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    fn review_result(run_id: &str, task_id: &str, outcome: ReviewOutcome) -> ReviewResult {
        ReviewResult {
            id: format!("review-{run_id}-{task_id}"),
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn has_note_kind(store: &TaskRuntimeStore, run_id: &str, kind: &str) -> Result<bool, String> {
        Ok(store
            .list_events(run_id, 0)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|event| {
                event
                    .payload
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some(kind)
            }))
    }

    fn claim_seeded_task(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<echo_agent::tasks::TaskClaim, String> {
        let snapshot = store
            .load_runtime_plan_snapshot(run_id)
            .map_err(|error| error.to_string())?;
        let task = snapshot
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| "seeded task missing".to_string())?;
        match store
            .claim_runtime_task(run_id, &task, snapshot.revision)
            .map_err(|error| error.to_string())?
        {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => Ok(claim),
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                Err("claim was not admitted".to_string())
            }
        }
    }

    /// A stale or foreign physical claim must not re-emit the current claim's
    /// terminal status with the refused attempt's product evidence. Exercise
    /// the real controller settlement and ExecSink, not a fabricated receipt.
    #[tokio::test]
    async fn superseded_controller_settlement_emits_no_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        use echo_agent::tasks::{
            RuntimeDagController, RuntimeTaskResolution, RuntimeTaskResolutionRequest as Request,
        };

        for identity in ["foreign", "stale"] {
            let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
            let _resource_scope = store.test_resource_scope();
            let run_id = seed_run(&store, vec![solo_readonly_task("reviewed")])?;
            let first_claim = claim_seeded_task(&store, &run_id)?;
            let current_claim = if identity == "stale" {
                store.settle_runtime_task_resolution(
                    &run_id,
                    "reviewed",
                    &first_claim,
                    Request::Requeue {
                        failure_fingerprint: None,
                        error: "retry admitted attempt".to_string(),
                        exhaustion: echo_agent::tasks::RuntimeRetryExhaustion::Failed,
                    },
                    RuntimeTaskProductSettlement::default(),
                )?;
                claim_seeded_task(&store, &run_id)?
            } else {
                first_claim.clone()
            };
            let mut refused_claim = first_claim;
            if identity == "foreign" {
                refused_claim.claim_id = "foreign-physical-claim".to_string();
            }
            let runtime_task = store
                .load_runtime_plan_snapshot(&run_id)?
                .tasks
                .into_iter()
                .find(|task| task.spec.id == "reviewed")
                .ok_or("claimed task missing")?;
            let events = Arc::new(std::sync::Mutex::new(Vec::<ExecEvent>::new()));
            let captured = events.clone();
            let trace_sink: ExecSink = Arc::new(move |event| {
                captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(event);
            });
            let controller = EkoRuntimeDagController {
                store: store.clone(),
                blocking: TaskRuntimeOperation::new(store.clone()),
                dispatcher: Arc::new(ScriptedDispatcher::new()),
                reviewer_llm: None,
                write_sem: Arc::new(Semaphore::new(1)),
                shell_sem: Arc::new(Semaphore::new(1)),
                llm_sem: Arc::new(Semaphore::new(1)),
                file_write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
                trace_sink: Some(trace_sink),
                cancel: CancellationToken::new(),
                resolution_metadata: std::sync::Mutex::new(HashMap::new()),
                dispatch_failures: std::sync::Mutex::new(HashMap::new()),
            };
            let product = |text: &str, outcome| RuntimeTaskProductSettlement {
                summary: Some(text.to_string()),
                execution_summary: Some(success_execution_summary(&run_id, "reviewed", text)),
                review: Some(review_result(&run_id, "reviewed", outcome)),
                diagnostic_note: Some(text.to_string()),
                typed_terminal: None,
            };
            controller.stage_resolution_metadata(
                &current_claim.claim_id,
                product("accepted current attempt", ReviewOutcome::Pass),
            )?;
            assert_eq!(
                controller
                    .settle_resolution(&run_id, &current_claim, &runtime_task, Request::Completed)
                    .await?,
                RuntimeTaskResolution::Completed
            );
            {
                let observed = events.lock().map_err(|_| "event mutex poisoned")?;
                assert_eq!(observed.len(), 1);
                let event = observed.first().ok_or("accepted event missing")?;
                assert_eq!(event.event, RuntimeEventKind::TaskCompleted);
                assert_eq!(
                    event.payload.get("summary"),
                    Some(&serde_json::json!("accepted current attempt"))
                );
            }
            let sequence = store
                .get_run_state(&run_id)?
                .map(|state| state.journal_sequence)
                .ok_or("run state missing")?;

            controller.stage_resolution_metadata(
                &refused_claim.claim_id,
                product("refused old attempt", ReviewOutcome::NeedsFix),
            )?;
            assert_eq!(
                controller
                    .settle_resolution(
                        &run_id,
                        &refused_claim,
                        &runtime_task,
                        Request::Blocked {
                            error: "refused old attempt".to_string(),
                            disposition: echo_agent::tasks::RuntimeStopDisposition::Pause,
                        },
                    )
                    .await?,
                RuntimeTaskResolution::Superseded
            );
            assert_eq!(
                events.lock().map_err(|_| "event mutex poisoned")?.len(),
                1,
                "{identity}: a refused attempt must not emit the current task's terminal status"
            );
            assert_eq!(
                store
                    .get_run_state(&run_id)?
                    .map(|state| state.journal_sequence),
                Some(sequence),
                "{identity}: the refused attempt must append no product facts"
            );
            assert_eq!(
                store
                    .get_summary(&run_id, "reviewed")?
                    .map(|summary| summary.outcome.summary),
                Some("accepted current attempt".to_string())
            );
            let reviews = store.list_reviews(&run_id, "reviewed")?;
            assert_eq!(reviews.len(), 1);
            assert!(matches!(
                reviews.first().map(|review| &review.outcome),
                Some(ReviewOutcome::Pass)
            ));
        }
        Ok(())
    }

    /// A failure fallback can lose the exact-claim CAS to a durable stop. The
    /// journal and live stream must describe that winner while keeping the
    /// original failure separately available for diagnosis.
    #[tokio::test]
    async fn abandoned_claim_projects_the_committed_interruption_and_keeps_failure_diagnostic()
    -> Result<(), Box<dyn std::error::Error>> {
        use echo_agent::tasks::{
            RuntimeClaimAbandonment, RuntimeDagController, RuntimeInterruptionDisposition,
            RuntimeTaskSettlementOutcome, TaskStatus,
        };

        for interruption in ["pause", "cancel", "none"] {
            let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
            let _resource_scope = store.test_resource_scope();
            let run_id = seed_run(&store, vec![solo_readonly_task("abandoned")])?;
            let claim = claim_seeded_task(&store, &run_id)?;
            let runtime_task = store
                .load_runtime_plan_snapshot(&run_id)?
                .tasks
                .into_iter()
                .find(|task| task.spec.id == "abandoned")
                .ok_or("claimed task missing")?;
            let events = Arc::new(std::sync::Mutex::new(Vec::<ExecEvent>::new()));
            let captured = events.clone();
            let trace_sink: ExecSink = Arc::new(move |event| {
                captured
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(event);
            });
            let controller = EkoRuntimeDagController {
                store: store.clone(),
                blocking: TaskRuntimeOperation::new(store.clone()),
                dispatcher: Arc::new(ScriptedDispatcher::new()),
                reviewer_llm: None,
                write_sem: Arc::new(Semaphore::new(1)),
                shell_sem: Arc::new(Semaphore::new(1)),
                llm_sem: Arc::new(Semaphore::new(1)),
                file_write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
                trace_sink: Some(trace_sink),
                cancel: CancellationToken::new(),
                resolution_metadata: std::sync::Mutex::new(HashMap::new()),
                dispatch_failures: std::sync::Mutex::new(HashMap::new()),
            };
            let disposition = match interruption {
                "pause" => Some(RuntimeInterruptionDisposition::Paused {
                    reason: "durable pause wins".to_string(),
                }),
                "cancel" => Some(RuntimeInterruptionDisposition::Cancelled),
                _ => None,
            };
            if let Some(disposition) = disposition {
                assert!(
                    controller
                        .request_task_interruption(
                            &run_id,
                            &runtime_task,
                            &claim,
                            disposition.clone(),
                        )
                        .await?
                );
                let persisted = store.load_runtime_plan_snapshot(&run_id)?;
                assert_eq!(
                    persisted
                        .tasks
                        .first()
                        .and_then(|task| task.execution.interruption_requested.as_ref()),
                    Some(&disposition),
                    "the exact-claim stop must be durable before abandonment"
                );
            }

            let failure = "original abandonment failure";
            let (
                expected_status,
                expected_summary,
                expected_label,
                expected_detail,
                expected_event,
            ) = match interruption {
                "pause" => (
                    TaskStatus::Paused("durable pause wins".to_string()),
                    "durable pause wins",
                    "paused",
                    Some("durable pause wins"),
                    RuntimeEventKind::TaskBlocked,
                ),
                "cancel" => (
                    TaskStatus::Cancelled,
                    "dispatch cancelled before resolution",
                    "cancelled",
                    None,
                    RuntimeEventKind::TaskCancelled,
                ),
                _ => (
                    TaskStatus::Failed(failure.to_string()),
                    failure,
                    "failed",
                    Some(failure),
                    RuntimeEventKind::TaskFailed,
                ),
            };
            let outcome = controller
                .abandon_claim(
                    &run_id,
                    &claim,
                    &runtime_task,
                    RuntimeClaimAbandonment::Failed {
                        error: failure.to_string(),
                    },
                )
                .await?;
            assert_eq!(
                outcome,
                RuntimeTaskSettlementOutcome::Settled {
                    status: expected_status.clone(),
                }
            );
            assert_eq!(
                store
                    .runtime_task_execution(&run_id, "abandoned")?
                    .map(|execution| execution.status),
                Some(expected_status)
            );
            let journal = store.list_events(&run_id, 0)?;
            let terminal = journal
                .iter()
                .rev()
                .find(|event| {
                    event.task_id.as_deref() == Some("abandoned")
                        && event
                            .payload
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            == Some(expected_label)
                })
                .ok_or("committed task event missing")?;
            assert_eq!(
                terminal
                    .payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str),
                Some(expected_summary),
                "{interruption}: the task journal must describe the committed winner"
            );
            assert_eq!(
                terminal
                    .payload
                    .get("status_detail")
                    .and_then(serde_json::Value::as_str),
                expected_detail
            );
            if interruption != "none" {
                assert!(
                    journal.iter().any(|event| {
                        event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            == Some("abandoned_attempt_diagnostic")
                            && event
                                .payload
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                == Some(failure)
                    }),
                    "{interruption}: retain the failure separately from the final summary"
                );
            }
            {
                let observed = events.lock().map_err(|_| "event mutex poisoned")?;
                assert_eq!(observed.len(), 1);
                let event = observed.first().ok_or("terminal live event missing")?;
                assert_eq!(event.event, expected_event);
                assert_eq!(
                    event
                        .payload
                        .get("summary")
                        .and_then(serde_json::Value::as_str),
                    Some(expected_summary)
                );
                assert_eq!(
                    event
                        .payload
                        .get("terminal_status")
                        .and_then(serde_json::Value::as_str),
                    Some(expected_label)
                );
                assert_eq!(
                    event
                        .payload
                        .get("status_detail")
                        .and_then(serde_json::Value::as_str),
                    expected_detail
                );
            }
            let sequence = store
                .get_run_state(&run_id)?
                .map(|state| state.journal_sequence);
            assert_eq!(
                controller
                    .abandon_claim(
                        &run_id,
                        &claim,
                        &runtime_task,
                        RuntimeClaimAbandonment::Failed {
                            error: "superseded repeated abandonment".to_string(),
                        },
                    )
                    .await?,
                RuntimeTaskSettlementOutcome::Superseded
            );
            assert_eq!(events.lock().map_err(|_| "event mutex poisoned")?.len(), 1);
            assert_eq!(
                store
                    .get_run_state(&run_id)?
                    .map(|state| state.journal_sequence),
                sequence,
                "{interruption}: superseded abandonment must append no journal facts"
            );
        }
        Ok(())
    }

    /// Defect B regression: a review gate that returns after a durable
    /// interruption must not persist the completed execution's success summary.
    ///
    /// Reproduces the real path with the canonical driver registration: a live
    /// driver is admitted and registered, the user cancels, the cancellation
    /// becomes a durable claim interruption while the run is still `Running`,
    /// and only then does the review gate return `NeedsFix`/`Blocked`. The
    /// canonical settlement commits `Interrupted`, so the execution summary and
    /// the review must both be refused instead of persisted for a task the user
    /// already stopped.
    // Tokio 的固定多线程属性名；不是本项目的 Subagent 概念。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn review_after_durable_interruption_publishes_no_success_evidence() -> Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let run_id = seed_run(&store, vec![solo_readonly_task("reviewed")])?;
        let claim = claim_seeded_task(&store, &run_id)?;

        // Register through the canonical driver admission path: this is what
        // makes a cancellation durable at the claim while the run stays live.
        let admission = store
            .reserve_run_driver_admission(run_id.clone(), CancellationToken::new())
            .map_err(|error| error.to_string())?;
        let driver = store
            .register_observed_run_driver::<()>(
                admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_cancel(&run_id)
                .map_err(|error| error.to_string())?
        );
        assert_eq!(
            store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .map(|run| run.status),
            Some(TaskRunStatus::Running),
            "a live driver keeps the run Running while the claim interruption is durable"
        );
        let interrupted = store
            .load_runtime_plan_snapshot(&run_id)
            .map_err(|error| error.to_string())?
            .tasks
            .into_iter()
            .find(|task| task.spec.id == "reviewed")
            .ok_or_else(|| "claimed task missing".to_string())?;
        assert_eq!(
            interrupted.execution.interruption_requested,
            Some(echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled),
            "request_cancel must durably interrupt the exact claim, not just the process token"
        );

        let product = RuntimeTaskProductSettlement {
            summary: Some("review needs fix".to_string()),
            execution_summary: Some(success_execution_summary(
                &run_id,
                "reviewed",
                "execution finished before the cancellation was observed",
            )),
            review: Some(review_result(&run_id, "reviewed", ReviewOutcome::NeedsFix)),
            diagnostic_note: Some("review needs fix".to_string()),
            typed_terminal: None,
        };
        let staged = staged_task_payload(&product);
        let receipt = store
            .settle_runtime_task_resolution(
                &run_id,
                "reviewed",
                &claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                    error: "review needs fix".to_string(),
                    disposition: echo_agent::tasks::RuntimeStopDisposition::Pause,
                },
                product,
            )
            .map_err(|error| error.to_string())?;

        // Durable truth first: no execution success evidence may land for an
        // attempt the user already interrupted.
        assert!(
            !has_note_kind(&store, &run_id, "summary_persisted")?,
            "no execution success summary may be persisted for an interrupted attempt"
        );
        assert!(
            store
                .get_summary(&run_id, "reviewed")
                .map_err(|error| error.to_string())?
                .is_none(),
            "the summary projection must stay empty"
        );
        assert!(
            store
                .list_reviews(&run_id, "reviewed")
                .map_err(|error| error.to_string())?
                .is_empty(),
            "the interrupted attempt's review must not land"
        );
        assert!(
            has_note_kind(&store, &run_id, "settlement_evidence_rejected")?,
            "the journal must keep one rejection fact"
        );
        assert!(
            receipt.acceptance.is_rejected(),
            "interrupted attempt must not accept its success evidence, got {:?}",
            receipt.acceptance
        );
        assert!(
            matches!(
                receipt.resolution,
                echo_agent::tasks::RuntimeTaskResolution::Interrupted { .. }
            ),
            "settlement must commit the interruption, got {:?}",
            receipt.resolution
        );
        assert_eq!(
            receipt.committed_status,
            Some(echo_agent::tasks::TaskStatus::Cancelled)
        );

        // Live payload: the same staged attempt data the dispatch would emit
        // must be degraded to the interruption truth.
        let payload = committed_task_payload(&receipt, staged);
        assert_eq!(payload["terminal_status"], "cancelled");
        assert_eq!(payload["acceptance"], "rejected");
        assert!(
            payload.get("artifacts").is_none()
                && payload.get("verification").is_none()
                && payload.get("touched_files").is_none(),
            "live payload must not expose success evidence, got {payload}"
        );
        assert_ne!(
            payload["summary"],
            "execution finished before the cancellation was observed"
        );

        drop(driver);
        Ok(())
    }

    /// Defect A regression matrix: cancel/pause × completion/abandon.
    ///
    /// The cell the old implementation got wrong is "cancel × completion": the
    /// durable claim interruption leaves the run `Running`, so the
    /// interruption-first classifier still reports `Committed` while the
    /// canonical settlement commits `Interrupted`. The receipt's acceptance
    /// fact and the degraded live payload must agree in every cell.
    // Tokio 的固定多线程属性名；不是本项目的 Subagent 概念。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interruption_matrix_acceptance_matches_live_payload() -> Result<(), String> {
        for interrupt in ["cancel", "pause"] {
            for outcome in ["completion", "abandon"] {
                let store =
                    Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
                let _resource_scope = store.test_resource_scope();
                let run_id = seed_run(&store, vec![solo_readonly_task("reviewed")])?;
                let claim = claim_seeded_task(&store, &run_id)?;
                let token = CancellationToken::new();
                let registration = store
                    .register_run_cancellation(&run_id, token.clone())
                    .map_err(|error| error.to_string())?;

                if interrupt == "cancel" {
                    assert!(
                        store
                            .request_cancel(&run_id)
                            .map_err(|error| error.to_string())?
                    );
                } else {
                    assert!(
                        store
                            .request_pause(&run_id)
                            .map_err(|error| error.to_string())?
                    );
                }
                assert!(
                    token.is_cancelled(),
                    "{interrupt}: driver token must be cancelled"
                );

                if outcome == "abandon" {
                    let fallback = if interrupt == "cancel" {
                        echo_agent::tasks::TaskStatus::Cancelled
                    } else {
                        echo_agent::tasks::TaskStatus::Paused("paused by user".to_string())
                    };
                    store
                        .settle_runtime_task_claim(
                            &run_id,
                            "reviewed",
                            &claim,
                            fallback,
                            Some("abandoned attempt".to_string()),
                        )
                        .map_err(|error| error.to_string())?;
                    let committed = store
                        .runtime_task_execution(&run_id, "reviewed")
                        .map_err(|error| error.to_string())?
                        .map(|execution| execution.status)
                        .ok_or_else(|| format!("{interrupt}/{outcome}: task missing"))?;
                    assert!(
                        matches!(
                            committed,
                            echo_agent::tasks::TaskStatus::Cancelled
                                | echo_agent::tasks::TaskStatus::Paused(_)
                        ),
                        "{interrupt}/{outcome}: abandoned task status was {committed:?}"
                    );
                    assert!(
                        !has_note_kind(&store, &run_id, "summary_persisted")?,
                        "{interrupt}/{outcome}: abandoned attempt kept a success summary"
                    );
                    drop(registration);
                    continue;
                }

                let product = RuntimeTaskProductSettlement {
                    summary: Some("late success".to_string()),
                    execution_summary: Some(success_execution_summary(
                        &run_id,
                        "reviewed",
                        "late success",
                    )),
                    review: Some(review_result(&run_id, "reviewed", ReviewOutcome::Pass)),
                    diagnostic_note: None,
                    typed_terminal: None,
                };
                let staged = staged_task_payload(&product);
                let receipt = store
                    .settle_runtime_task_resolution(
                        &run_id,
                        "reviewed",
                        &claim,
                        echo_agent::tasks::RuntimeTaskResolutionRequest::Completed,
                        product,
                    )
                    .map_err(|error| error.to_string())?;

                assert!(
                    receipt.acceptance.is_rejected(),
                    "{interrupt}/{outcome}: refused success evidence must be marked rejected, settlement={:?}",
                    receipt.settlement
                );
                let committed = receipt
                    .committed_status
                    .clone()
                    .ok_or_else(|| format!("{interrupt}/{outcome}: committed status missing"))?;
                assert!(
                    matches!(
                        committed,
                        echo_agent::tasks::TaskStatus::Cancelled
                            | echo_agent::tasks::TaskStatus::Paused(_)
                    ),
                    "{interrupt}/{outcome}: task did not settle as interruption: {committed:?}"
                );
                assert!(
                    !has_note_kind(&store, &run_id, "summary_persisted")?,
                    "{interrupt}/{outcome}: refused success evidence persisted a summary"
                );
                assert!(
                    store
                        .get_summary(&run_id, "reviewed")
                        .map_err(|error| error.to_string())?
                        .is_none(),
                    "{interrupt}/{outcome}: summary projection must stay empty"
                );

                let payload = committed_task_payload(&receipt, staged);
                assert_eq!(
                    payload["acceptance"], "rejected",
                    "{interrupt}/{outcome}: payload and receipt acceptance disagree"
                );
                assert!(
                    payload.get("artifacts").is_none()
                        && payload.get("verification").is_none()
                        && payload.get("touched_files").is_none(),
                    "{interrupt}/{outcome}: live payload leaked success fields, got {payload}"
                );
                assert_ne!(payload["summary"], "late success");
                drop(registration);
            }
        }
        Ok(())
    }

    /// The fix must not over-reach: a non-success review gate on a healthy run
    /// keeps its execution evidence and review history.
    // Tokio 的固定多线程属性名；不是本项目的 Subagent 概念。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn needs_fix_review_without_interruption_keeps_execution_evidence() -> Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let run_id = seed_run(&store, vec![solo_readonly_task("reviewed")])?;
        let claim = claim_seeded_task(&store, &run_id)?;

        let product = RuntimeTaskProductSettlement {
            summary: Some("review needs fix".to_string()),
            execution_summary: Some(success_execution_summary(
                &run_id,
                "reviewed",
                "execution finished",
            )),
            review: Some(review_result(&run_id, "reviewed", ReviewOutcome::NeedsFix)),
            diagnostic_note: Some("review needs fix".to_string()),
            typed_terminal: None,
        };
        let staged = staged_task_payload(&product);
        let receipt = store
            .settle_runtime_task_resolution(
                &run_id,
                "reviewed",
                &claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                    error: "review needs fix".to_string(),
                    disposition: echo_agent::tasks::RuntimeStopDisposition::Pause,
                },
                product,
            )
            .map_err(|error| error.to_string())?;

        assert!(
            receipt.acceptance.is_accepted(),
            "a healthy blocked review keeps its evidence, got {:?}",
            receipt.acceptance
        );
        assert_eq!(
            receipt.committed_status,
            Some(echo_agent::tasks::TaskStatus::Blocked(
                "review needs fix".to_string()
            ))
        );
        assert!(
            has_note_kind(&store, &run_id, "summary_persisted")?,
            "healthy non-success attempts keep their execution summary"
        );
        assert_eq!(
            store
                .list_reviews(&run_id, "reviewed")
                .map_err(|error| error.to_string())?
                .len(),
            1,
            "healthy non-success attempts keep their review history"
        );
        let payload = committed_task_payload(&receipt, staged);
        assert_eq!(payload["terminal_status"], "blocked");
        assert_eq!(payload["acceptance"], "accepted");
        assert!(
            payload.get("artifacts").is_some() && payload.get("verification").is_some(),
            "accepted evidence keeps its rich payload, got {payload}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bound_light_domain_uses_original_summary_and_formal_domain_requires_real_graph()
    -> Result<(), Box<dyn std::error::Error>> {
        for formal in [false, true] {
            let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
            let binding = crate::domain_plugin::test_binding(formal)?;
            let run_id = if formal {
                "formal-domain"
            } else {
                "light-domain"
            };
            store.create_run_for_active_workspace_with_domain(
                run_id,
                "conversation",
                "root",
                DomainProfile::General,
                "answer from evidence",
                "agent_task_plan",
                AttendedMode::Attended,
                TaskRunExecutionProfile::orchestrated(if formal {
                    RunPlanPolicy::RequirePlan
                } else {
                    RunPlanPolicy::AllowDirect
                }),
                Some(&binding),
            )?;
            store.transition_run(run_id, TaskRunStatus::Running)?;
            store.configure_run_continuation(run_id, true, false, None, None)?;
            store.claim_run_turn(run_id, "turn", RunTurnOrigin::User, TurnVisibility::Visible)?;
            let run = store.get_run(run_id)?.ok_or("run missing")?;
            let observation = EkoAgentTurnSink::observe_chat(&run, HashSet::new())
                .finish(Some("Observed answer."));
            let mut receipt = TurnReceipt::failed(
                "turn",
                echo_agent::error::AgentFailure::message("fixture", "initial"),
            )?;
            receipt.outcome = TurnOutcome::Completed;
            receipt.final_answer = Some("Observed answer.".to_string());
            finalize_direct_agent_observation(
                &store,
                run_id,
                &mut receipt,
                observation,
                &CancellationToken::new(),
            )
            .await?;
            assert!(matches!(receipt.outcome, TurnOutcome::Completed));
            let plan = store.get_plan(run_id)?;
            if formal {
                assert!(
                    plan.is_none(),
                    "formal domain must not materialize a fake direct Summary"
                );
                assert!(!store.completion_gate_report(run_id)?.ready);
            } else {
                let plan = plan.ok_or("light domain Summary missing")?;
                assert_eq!(plan.domain_binding, Some(binding.reference()));
                assert_eq!(plan.tasks.len(), 1);
                let task = plan.tasks.first().ok_or("Summary missing")?;
                assert_eq!(task.kind, PlanTaskKind::Summary);
                assert!(task.required_artifacts.is_empty());
                assert!(task.execution_checks.is_empty());
                assert!(task.acceptance_criteria.is_empty());
                assert!(task.allowed_tools.is_empty());
                assert_eq!(task.domain_binding, Some(binding.reference()));
                assert!(store.completion_gate_report(run_id)?.ready);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn bound_direct_observation_rejects_real_mutation_without_relaxing_summary()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let binding = crate::domain_plugin::test_binding(false)?;
        store.create_run_for_active_workspace_with_domain(
            "mutating-domain",
            "conversation",
            "root",
            DomainProfile::General,
            "answer",
            "agent_task_plan",
            AttendedMode::Attended,
            TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
            Some(&binding),
        )?;
        store.transition_run("mutating-domain", TaskRunStatus::Running)?;
        store.configure_run_continuation("mutating-domain", true, false, None, None)?;
        store.claim_run_turn(
            "mutating-domain",
            "turn",
            RunTurnOrigin::User,
            TurnVisibility::Visible,
        )?;
        let observation = EkoAgentTurnObservation {
            output: "claimed answer".to_string(),
            observed_evidence: Vec::new(),
            observed_artifacts: Vec::new(),
            mutating_tool_observed: true,
        };
        let mut receipt = TurnReceipt::failed(
            "turn",
            echo_agent::error::AgentFailure::message("fixture", "initial"),
        )?;
        receipt.outcome = TurnOutcome::Completed;
        finalize_direct_agent_observation(
            &store,
            "mutating-domain",
            &mut receipt,
            observation,
            &CancellationToken::new(),
        )
        .await?;
        assert!(matches!(receipt.outcome, TurnOutcome::Failed(_)));
        assert!(store.get_plan("mutating-domain")?.is_none());
        Ok(())
    }

    // 新受控合成 Model，经原 unattended producer/RunTurn/Driver/sink；不是旧 Cron payload 重放。
    async fn publication_run_fixture(
        root: &std::path::Path,
    ) -> anyhow::Result<(
        Arc<TaskRuntimeStore>,
        crate::agent_handle::AgentHandle,
        Arc<crate::chat_event_log::ChatEventLog>,
        Arc<super::super::command_cells::CommandCellRuntimeService>,
    )> {
        use echo_agent::testing::{MockLlmClient, MockTool};
        let store = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            root.join("store"),
        )?);
        let log = Arc::new(crate::chat_event_log::ChatEventLog::open(
            root.join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?);
        let runtime = super::super::command_cells::CommandCellRuntimeService::new(
            Arc::new(echo_agent::sandbox::LocalSandbox::new(Default::default())),
            log.clone(),
            crate::product_data_io::ProductDataIoService::new(),
        )
        .map_err(anyhow::Error::msg)?;
        runtime.bind_store(&store);
        let llm = Arc::new(
            MockLlmClient::new()
                .with_model_name("publication-fixture")
                .then_tool_call("publication-read", "read_file", r#"{"path":"README.md"}"#)
                .with_response("受控 publication 原公开回答🧭"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("publication-fixture")
                .llm_client(llm)
                .tool(Box::new(MockTool::new("read_file").with_parameters(serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})).with_response("原只读证据")))
                .build()?,
        );
        Ok((store, agent, log, runtime))
    }

    #[tokio::test]
    async fn unattended_owned_turn_preserves_real_native_execution_header() -> anyhow::Result<()> {
        use echo_agent::agent::{AgentEvent, EventEnvelope, ExecutionStatusKind};
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".echo-semantic/local/cron-publication-tests");
        std::fs::create_dir_all(&directory)?;
        let root = tempfile::tempdir_in(directory)?;
        let (store, agent, _log, runtime) = publication_run_fixture(root.path()).await?;
        let _scope = store.test_resource_scope();
        let result = launch_unattended_run(
            store.clone(),
            agent,
            "cron",
            "definition-fixture",
            "occurrence-fixture",
            "原 producer 身份核对",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await;
        runtime.shutdown().await.map_err(anyhow::Error::msg)?;
        drop(runtime);
        let run_id = result?;
        let native = store
            .list_events(&run_id, 0)?
            .into_iter()
            .filter(|event| {
                event
                    .payload
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("native_agent_event")
            })
            .map(|event| {
                let raw = event
                    .payload
                    .get("agent_envelope")
                    .ok_or_else(|| anyhow::anyhow!("original native envelope missing"))?;
                Ok(serde_json::from_value::<EventEnvelope>(raw.clone())?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let source = native
            .iter()
            .find_map(|event| match &event.payload {
                AgentEvent::ExecutionStatus(status)
                    if status.status == ExecutionStatusKind::Started =>
                {
                    Some(status.execution_id.clone())
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("original typed Started missing"))?;
        anyhow::ensure!(!source.is_empty(), "real source execution empty");
        anyhow::ensure!(
            !native.is_empty(),
            "original producer did not persist native events"
        );
        for event in &native {
            anyhow::ensure!(
                event.execution_id.as_ref().map(|id| id.as_str()) == Some(source.as_str()),
                "original canonical header lost typed source execution"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn unattended_owned_turn_publishes_strict_cold_history_without_raw_fallback()
    -> anyhow::Result<()> {
        use echo_agent::llm::types::Role;
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".echo-semantic/local/cron-publication-tests");
        std::fs::create_dir_all(&directory)?;
        let root = tempfile::tempdir_in(directory)?;
        let (store, agent, log, runtime) = publication_run_fixture(root.path()).await?;
        let _scope = store.test_resource_scope();
        let result = launch_unattended_run(
            store.clone(),
            agent.clone(),
            "cron",
            "definition-fixture",
            "occurrence-fixture",
            "原 producer 冷历史核对",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await;
        runtime.shutdown().await.map_err(anyhow::Error::msg)?;
        drop(runtime);
        let run_id = result?;
        let run = store
            .get_run(&run_id)?
            .ok_or_else(|| anyhow::anyhow!("original run missing"))?;
        let inner = agent.inner().clone();
        let messages = inner.read().await.get_messages().await;
        let message = messages
            .iter()
            .rev()
            .find(|message| {
                message.role == Role::Assistant
                    && message.content.as_text().as_deref() == Some("受控 publication 原公开回答🧭")
            })
            .ok_or_else(|| anyhow::anyhow!("original canonical assistant message missing"))?;
        anyhow::ensure!(
            message.execution_status.is_some(),
            "synthetic Model did not produce native canonical status"
        );
        let events =
            log.native_history_publications(&run.workspace_id, &run.conversation_id, message)?;
        let view = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &events,
        )
        .map_err(anyhow::Error::msg)?
        .ok_or_else(|| anyhow::anyhow!("native view missing"))?;
        anyhow::ensure!(
            view.final_answer_confirmed
                && view.content.as_deref() == Some("受控 publication 原公开回答🧭"),
            "original unattended producer did not publish cold history"
        );
        let empty = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &[],
        )
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            empty.is_some_and(|view| !view.final_answer_confirmed && view.content.is_none()),
            "missing publication gained raw fallback"
        );
        let missing_settled = events
            .iter()
            .filter(|event| {
                !matches!(&event.payload,
            crate::chat_driver::ChatDriverEvent::Agent(native)
                if matches!(&native.payload, AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Settled))
            })
            .cloned()
            .collect::<Vec<_>>();
        let incomplete = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &missing_settled,
        )
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            incomplete.is_some_and(|view| !view.final_answer_confirmed),
            "missing real Settled was manufactured"
        );
        // 纯 consumer 故障夹具：不得把另一原 owner 或后继产品失败当本次成功。
        let mut foreign = events.clone();
        for event in &mut foreign {
            if let crate::chat_driver::ChatDriverEvent::Agent(native) = &mut event.payload {
                let identity = echo_agent::agent::EventIdentity::for_chat(
                    Some(run.conversation_id.clone()),
                    native.turn_id.as_str(),
                    native
                        .message_id
                        .as_ref()
                        .map(|id| id.as_str())
                        .unwrap_or(native.turn_id.as_str()),
                    Some(run.run_id.clone()),
                )?
                .with_execution_id("foreign-synthetic-owner")?;
                let mut payload = native.payload.clone();
                if let AgentEvent::ExecutionStatus(status) = &mut payload {
                    status.execution_id = "foreign-synthetic-owner".to_string();
                }
                **native = echo_agent::agent::EventEnvelope::new(
                    &identity,
                    native.sequence,
                    native.parent_event_id.clone(),
                    payload,
                )?;
            }
        }
        let wrong_owner = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &foreign,
        )
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            wrong_owner.is_some_and(|view| !view.final_answer_confirmed),
            "foreign publication owner gained success"
        );
        let mut product_failed = events.clone();
        for event in &mut product_failed {
            if let crate::chat_driver::ChatDriverEvent::TurnStatus { status } = &mut event.payload {
                *status = "failed".to_string();
            }
        }
        let failed = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &product_failed,
        )
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            failed.is_some_and(|view| !view.final_answer_confirmed),
            "product failure was hidden by native Settled"
        );
        drop(log);
        let cold = crate::chat_event_log::ChatEventLog::open(
            root.path().join("chat-events"),
            crate::chat_event_log::ChatEventRetention::default(),
        )?;
        let cold_events =
            cold.native_history_publications(&run.workspace_id, &run.conversation_id, message)?;
        let cold_view = crate::conversation_projection::project_native_history_publication(
            message,
            &run.workspace_id,
            &run.conversation_id,
            &cold_events,
        )
        .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            cold_view.is_some_and(|view| view.final_answer_confirmed),
            "cold original publication not recoverable"
        );
        Ok(())
    }
}
