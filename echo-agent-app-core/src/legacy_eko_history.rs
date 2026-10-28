//! EKO旧控制历史的来源适配；不复制Framework恢复/CAS/执行逻辑。
//!
//! 已核旧factory默认Final、非默认Think，但原checkpoint/trace未保存逐call注册来源。
//! 不能由今天的工厂/registry或名字/参数补造过去的Builtin或Custom身份。

use echo_agent::compression::{ProjectionContext, RecoveredToolOrigin};
use echo_agent::error::Result;
use echo_agent::llm::types::{Message, Role, ToolCall};
use futures::future::BoxFuture;

/// 三个真实projector共用这一薄接缝。空来源明确表示Unknown，不是恢复/执行许可。
/// 原数据不携可靠逐call来源时，已配对历史由Framework原样保留，pending交原restore明确拒绝。
/// 后继只有原恢复owner提供可核可靠来源才能交Frame归一化，本接缝不为旧数据签发新proof。
pub(crate) fn recovered_tool_origins<'a>(
    context: &'a ProjectionContext,
    messages: &'a [Message],
) -> BoxFuture<'a, Result<Vec<RecoveredToolOrigin>>> {
    Box::pin(async move {
        let candidates = legacy_candidates(messages);
        if !candidates.is_empty() {
            tracing::debug!(
                historical_call_count = candidates.len(),
                conversation_bound = context.conversation_id.is_some(),
                "旧EKO调用无当时注册来源，保持Unknown并交Framework原恢复校验"
            );
        }
        Ok(Vec::new())
    })
}

fn legacy_candidates(messages: &[Message]) -> Vec<&ToolCall> {
    messages
        .iter()
        .filter(|message| {
            message.role == Role::Assistant
                && message.assistant_blocks.is_none()
                && message.execution_status.is_none()
                && message.approved_assistant_blocks.is_none()
        })
        .flat_map(|message| message.tool_calls.iter().flatten())
        .filter(|call| {
            call.provider_source.is_none()
                && call.call_type == "function"
                && (call.function.name == "final_answer" || call.function.name == "think")
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod production_getter_tests {
    use super::*;
    use echo_agent::agent::{Agent, ReactAgent};
    use echo_agent::agent::{AgentEvent, ReactAgentBuilder};
    use echo_agent::compression::PreModelContextProjector;
    use echo_agent::llm::types::{FunctionCall, ToolCall};
    use echo_agent::state::{
        AgentCheckpoint, FileRuntimeStateStore, RuntimeCheckpointCasRequest,
        RuntimeCheckpointCasStatus, RuntimeStateExpectedVersion, RuntimeStateStore,
    };
    use echo_agent::testing::{MockLlmClient, MockTool};
    use echo_agent::trace::{
        JsonlRunStore, Run, RunEvent, RunStatus, RunStore, RunTimings, TokenUsage,
    };
    use futures::StreamExt;
    use std::path::Path;
    use std::sync::Arc;

    const CONVERSATION: &str = "original-eko-conversation";
    pub(crate) fn temporary() -> anyhow::Result<tempfile::TempDir> {
        let base=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.echo-semantic/local/parallel-executors-20261027/runtime-acceptance/eko-legacy-source/tmp");
        std::fs::create_dir_all(&base)?;
        Ok(tempfile::Builder::new()
            .prefix("eko-source-")
            .tempdir_in(base)?)
    }
    fn legacy_call() -> ToolCall {
        ToolCall {
            provider_source: None,
            id: "original-control".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "final_answer".into(),
                arguments: r#"{"answer":"original old candidate"}"#.into(),
            },
        }
    }
    pub(crate) fn messages() -> Vec<Message> {
        vec![
            Message::user("old request".into()),
            Message::assistant_with_tools(vec![legacy_call()]),
            Message::tool_result(
                "original-control".into(),
                "final_answer".into(),
                "actual old tool result".into(),
            ),
        ]
    }
    pub(crate) fn context() -> ProjectionContext {
        ProjectionContext {
            iteration: 0,
            agent_name: "eko-source-agent".into(),
            session_id: Some(CONVERSATION.into()),
            conversation_id: Some(CONVERSATION.into()),
            run_id: None,
            turn_id: Some("current-turn".into()),
        }
    }
    pub(crate) async fn durable_fixture(
        root: &Path,
        epoch: u64,
        include_trace: bool,
    ) -> anyhow::Result<(Arc<FileRuntimeStateStore>, Arc<JsonlRunStore>)> {
        let state = Arc::new(FileRuntimeStateStore::new(root.join("runtime"))?);
        let checkpoint = AgentCheckpoint {
            conversation_id: CONVERSATION.into(),
            messages_json: AgentCheckpoint::serialize_payload(messages(), None)?,
            current_plan: None,
            active_skills: Vec::new(),
            blocked_reason: None,
            working_dir: None,
            timestamp: chrono::Utc::now(),
        };
        let result = state
            .compare_and_save_checkpoint(RuntimeCheckpointCasRequest {
                scope_id: CONVERSATION.into(),
                runtime_state_id: CONVERSATION.into(),
                conversation_epoch: Some(epoch),
                expected_scope_revision: 0,
                expected_state_version: RuntimeStateExpectedVersion::Absent,
                checkpoint,
                managed_import: None,
            })
            .await?;
        anyhow::ensure!(matches!(
            result.status,
            RuntimeCheckpointCasStatus::Applied | RuntimeCheckpointCasStatus::AlreadyCurrent
        ));
        let traces = Arc::new(JsonlRunStore::new(root.join("runs"))?);
        if include_trace {
            traces
                .save(Run {
                    run_id: "old-eko-root-run".into(),
                    parent_run_id: None,
                    agent_name: "eko-source-agent".into(),
                    model: "old-model".into(),
                    provider: None,
                    turn_id: Some("old-turn".into()),
                    execution_id: Some("old-execution".into()),
                    session_id: CONVERSATION.into(),
                    status: RunStatus::Completed,
                    input: "old request".into(),
                    events: vec![RunEvent::ToolCall {
                        call_id: "original-control".into(),
                        name: "final_answer".into(),
                        args: Some(serde_json::json!({"answer":"original old candidate"})),
                        risk: Some("ReadOnly".into()),
                        duration_ms: 0,
                    }],
                    final_output: Some("actual old final".into()),
                    error: None,
                    token_usage: TokenUsage::default(),
                    timings: RunTimings::default(),
                    started_at: chrono::Utc::now(),
                    finished_at: Some(chrono::Utc::now()),
                })
                .await?;
        }
        Ok((state, traces))
    }
    pub(crate) fn agent(
        state: Arc<FileRuntimeStateStore>,
        traces: Arc<JsonlRunStore>,
    ) -> anyhow::Result<ReactAgent> {
        Ok(ReactAgentBuilder::new()
            .name("eko-source-agent")
            .system_prompt("source test")
            .conversation_id(CONVERSATION)
            .session_id(CONVERSATION)
            .state_store(state)
            .with_run_store(traces)
            .llm_client(Arc::new(
                MockLlmClient::new().with_response("current final"),
            ))
            .build()?)
    }
    fn eko_projector(_agent: &ReactAgent) -> crate::turn_context::EkoContextProjector {
        crate::turn_context::EkoContextProjector::new(
            Arc::new(
                crate::tasks::task_runtime::compact_context::TaskRuntimeProjectionRegistry::new(),
            ),
            Arc::new(crate::turn_context::TurnPromptContextRegistry::new()),
        )
    }

    #[tokio::test]
    async fn original_eko_scope_trace_without_registration_origin_is_not_upgraded_to_builtin()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        drop(state);
        drop(traces);
        let agent = agent(
            Arc::new(FileRuntimeStateStore::new(root.path().join("runtime"))?),
            Arc::new(JsonlRunStore::new(root.path().join("runs"))?),
        )?;
        let original = serde_json::to_vec(&messages())?;
        let origins = eko_projector(&agent)
            .recovered_tool_origins(&context(), &messages())
            .await?;
        assert!(
            origins.is_empty(),
            "scope/trace关联不能排除旧同名custom实现"
        );
        assert_eq!(original, serde_json::to_vec(&messages())?);
        Ok(())
    }
    #[tokio::test]
    async fn imported_epoch_or_missing_actual_call_receipt_never_becomes_builtin()
    -> anyhow::Result<()> {
        for (epoch, trace) in [(2, true), (1, false)] {
            let root = temporary()?;
            let (state, traces) = durable_fixture(root.path(), epoch, trace).await?;
            let agent = agent(state, traces)?;
            assert!(
                eko_projector(&agent)
                    .recovered_tool_origins(&context(), &messages())
                    .await?
                    .is_empty()
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn same_call_id_wrong_original_args_scope_or_native_fields_do_not_claim_builtin()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        let agent = agent(state, traces)?;
        let projector = eko_projector(&agent);
        let mut changed = messages();
        if let Some(call) = changed
            .get_mut(1)
            .and_then(|message| message.tool_calls.as_mut())
            .and_then(|calls| calls.first_mut())
        {
            call.function.arguments = r#"{"answer":"another record"}"#.into();
        }
        assert!(
            projector
                .recovered_tool_origins(&context(), &changed)
                .await?
                .is_empty()
        );
        let mut wrong = context();
        wrong.conversation_id = Some("other-scope".into());
        assert!(
            projector
                .recovered_tool_origins(&wrong, &messages())
                .await?
                .is_empty()
        );
        let mut native = messages();
        if let Some(message) = native.get_mut(1) {
            message.assistant_blocks = Some(Vec::new());
        }
        assert!(
            projector
                .recovered_tool_origins(&context(), &native)
                .await?
                .is_empty()
        );
        Ok(())
    }
    #[tokio::test]
    async fn task_runtime_and_fresh_projector_use_the_same_real_source_getter_without_parent_proof()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        let _original_agent = agent(state, traces)?;
        let task = crate::tasks::task_runtime::compact_context::TaskRuntimeContextProjector::new(
            Arc::new(
                crate::tasks::task_runtime::compact_context::TaskRuntimeProjectionRegistry::new(),
            ),
        );
        assert!(
            task.recovered_tool_origins(&context(), &messages())
                .await?
                .is_empty()
        );
        let fresh = ReactAgentBuilder::new()
            .name("fresh-subagent")
            .llm_client(Arc::new(MockLlmClient::new()))
            .build()?;
        let fresh_context = ProjectionContext {
            agent_name: fresh.name().to_string(),
            session_id: None,
            conversation_id: None,
            run_id: None,
            turn_id: None,
            iteration: 0,
        };
        assert!(
            recovered_tool_origins(&fresh_context, &messages())
                .await?
                .is_empty(),
            "fresh没有原历史来源，不能借父证明"
        );
        Ok(())
    }
    #[tokio::test]
    async fn current_binding_does_not_classify_old_history_but_fresh_custom_executes()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        let mut agent = agent(state, traces)?;
        let projector = eko_projector(&agent);
        // 当前新增绑定不构成历史来源证明；新执行仍须按真实当前工具运行。
        agent.add_tool(Box::new(
            MockTool::new("final_answer").with_response("actual custom result"),
        ));
        let origins = projector
            .recovered_tool_origins(&context(), &messages())
            .await?;
        assert!(origins.is_empty(), "当前Custom绑定不能追认历史当时Custom");
        let llm = Arc::new(
            MockLlmClient::new()
                .then_tool_call("fresh-custom", "final_answer", "{}")
                .with_response("after custom"),
        );
        let fresh = ReactAgentBuilder::new()
            .name("fresh-custom-agent")
            .llm_client(llm.clone())
            .tool(Box::new(
                MockTool::new("final_answer").with_response("actual custom result"),
            ))
            .build()?;
        fresh.set_pre_model_context_projector(Some(Arc::new(eko_projector(&fresh))));
        let events = fresh
            .execute_stream("fresh custom must execute")
            .await?
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        assert!(events.iter().any(|event|matches!(event,AgentEvent::ToolResult{call_id,result,..} if call_id=="fresh-custom"&&result.success&&result.output=="actual custom result")));
        assert_eq!(llm.call_count(), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::FinalAnswer(_)))
                .count(),
            1
        );
        assert!(
            matches!(events.last(),Some(AgentEvent::ExecutionStatus(status)) if status.status==echo_agent::agent::ExecutionStatusKind::Settled)
        );
        Ok(())
    }
    #[tokio::test]
    async fn old_eko_think_nondefault_and_final_default_knowledge_does_not_prove_each_call()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        let agent = agent(state, traces)?;
        let projector = eko_projector(&agent);
        for name in ["think", "final_answer"] {
            let mut record = messages();
            if let Some(call) = record
                .get_mut(1)
                .and_then(|message| message.tool_calls.as_mut())
                .and_then(|calls| calls.first_mut())
            {
                call.function.name = name.into();
            }
            assert!(
                projector
                    .recovered_tool_origins(&context(), &record)
                    .await?
                    .is_empty(),
                "旧定义菜单不等于这条原call来源"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn toolcall_provider_source_alone_is_native_and_never_claims_retired_builtin()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let (state, traces) = durable_fixture(root.path(), 1, true).await?;
        let mut agent = agent(state, traces)?;
        agent.add_tool(Box::new(
            MockTool::new("final_answer").with_response("today custom"),
        ));
        let mut record = messages();
        if let Some(call) = record
            .get_mut(1)
            .and_then(|message| message.tool_calls.as_mut())
            .and_then(|calls| calls.first_mut())
        {
            call.provider_source = Some(echo_agent::llm::types::ProviderOutputSource {
                protocol: echo_agent::llm::LlmApiProtocol::Responses,
                request_id: "native-response".into(),
                item_id: "native-call".into(),
                output_index: 0,
                content_index: None,
                content_kind: Some("function_call".into()),
                phase: None,
            });
        }
        assert!(legacy_candidates(&record).is_empty());
        assert!(
            eko_projector(&agent)
                .recovered_tool_origins(&context(), &record)
                .await?
                .is_empty()
        );
        Ok(())
    }

    struct ReplayProbe {
        name: &'static str,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl echo_agent::tools::Tool for ReplayProbe {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "Count actual current custom calls"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"answer":{"type":"string"}}})
        }
        fn risk_level(&self) -> echo_agent::tools::ToolRiskLevel {
            echo_agent::tools::ToolRiskLevel::ReadOnly
        }
        fn execute(
            &self,
            _parameters: echo_agent::tools::ToolParameters,
        ) -> futures::future::BoxFuture<'_, Result<echo_agent::tools::ToolResult>> {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(echo_agent::tools::ToolResult::success("current effect"))
            })
        }
    }
    #[tokio::test]
    async fn unknown_pending_old_call_is_rejected_without_running_today_same_named_custom()
    -> anyhow::Result<()> {
        let root = temporary()?;
        let state = Arc::new(FileRuntimeStateStore::new(root.path())?);
        let mut checkpoint = AgentCheckpoint::new(CONVERSATION);
        let pending = vec![
            Message::assistant_with_tools(vec![ToolCall {
                provider_source: None,
                id: "completed-effect".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "effect".into(),
                    arguments: "{}".into(),
                },
            }]),
            Message::tool_result(
                "completed-effect".into(),
                "effect".into(),
                "original completed side effect".into(),
            ),
            Message::assistant_with_tools(vec![legacy_call()]),
        ];
        checkpoint.messages_json = serde_json::to_string(&pending)?;
        assert!(
            checkpoint.restore_runtime_payload().is_err(),
            "不篡改旧pending为已完成结果"
        );
        // 旧坏配对只能以磁盘原件夹具读取；今天的save会拒绝该输入，不伪造成新合法checkpoint。
        let owner = serde_json::json!({"version":1,"runtime_state_id":CONVERSATION,"scope_id":CONVERSATION,"phase":"active","checkpoint":checkpoint});
        let raw = serde_json::to_vec(&owner)?;
        let safe = echo_agent::utils::fs::encode_path_segment_identity(CONVERSATION)?;
        let path = root
            .path()
            .join("runtime_state/_runtime_owners")
            .join(format!("{safe}.json"));
        std::fs::write(&path, &raw)?;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let llm = Arc::new(MockLlmClient::new().with_response("must not call model"));
        let agent = ReactAgentBuilder::new()
            .name("eko-source-agent")
            .session_id(CONVERSATION)
            .conversation_id(CONVERSATION)
            .state_store(state)
            .llm_client(llm.clone())
            .tool(Box::new(ReplayProbe {
                name: "effect",
                calls: calls.clone(),
            }))
            .tool(Box::new(ReplayProbe {
                name: "final_answer",
                calls: calls.clone(),
            }))
            .build()?;
        agent.set_pre_model_context_projector(Some(Arc::new(eko_projector(&agent))));
        assert!(
            eko_projector(&agent)
                .recovered_tool_origins(&context(), &pending)
                .await?
                .is_empty(),
            "今天同名绑定也不是当年Custom证据"
        );
        let failure_seen = match agent.execute_stream("resume unknown old pending").await {
            Err(_) => true,
            Ok(source) => {
                let results = source.collect::<Vec<_>>().await;
                let failure_seen = results.iter().any(|result| {
                    result.is_err() || matches!(result, Ok(AgentEvent::Error { .. }))
                });
                assert!(!results.iter().any(|result| matches!(
                    result,
                    Ok(AgentEvent::FinalAnswer(_) | AgentEvent::ToolResult { .. })
                )));
                failure_seen
            }
        };
        assert!(failure_seen, "未知pending必须明确恢复拒绝");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(llm.call_count(), 0);
        assert_eq!(
            std::fs::read(&path)?,
            raw,
            "原旧记录逐字节保留，不能补假ToolResult/成功"
        );
        Ok(())
    }
}
