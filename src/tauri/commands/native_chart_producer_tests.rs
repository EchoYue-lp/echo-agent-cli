//! 受控Model调用真实GenerateChartTool，经原Driver/FileStore/journal及历史wrapper。
//! 不直造Chart事件，不启动App或真实provider。
use super::*;
use echo_agent::agent::AgentEvent;
use echo_agent::tools::{Tool as _, ToolParameters};

struct SavedChartBatch {
    sequence: u64,
    messages: Vec<SavedMessage>,
}

struct ChartProducerSink {
    log: Arc<ChatEventLog>,
    store: Arc<FileConversationStore>,
    events: std::sync::Mutex<Vec<ChatEventEnvelope>>,
    batch: std::sync::Mutex<Option<SavedChartBatch>>,
}

#[async_trait::async_trait]
impl EventSink for ChartProducerSink {
    async fn on_event(
        &self,
        original: echo_agent::agent::EventEnvelope,
    ) -> echo_agent::error::Result<SinkControl> {
        let saved = matches!(&original.payload,
            AgentEvent::TranscriptProjectionSettlement(receipt)
            if receipt.status == echo_agent::memory::TranscriptProjectionSettlementStatus::Settled
                && receipt.operation_id.is_some() && receipt.generation_id.is_some() && receipt.attempt > 0);
        let envelope = self
            .log
            .append_async(
                WORKSPACE.into(),
                Some(CONVERSATION.into()),
                ROOT_TURN.into(),
                ChatDriverEvent::Agent(Box::new(original)),
            )
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        self.events
            .lock()
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .push(envelope.clone());
        let empty = self
            .batch
            .lock()
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .is_none();
        let original_result_seen = self
            .events
            .lock()
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .iter()
            .any(|event| {
                matches!(&event.payload,
                ChatDriverEvent::Agent(agent) if matches!(&agent.payload,
                    AgentEvent::ToolResult { name, .. } if name == "generate_chart"))
            });
        if saved && empty && original_result_seen {
            let stored = self
                .store
                .get_messages(CONVERSATION)
                .await
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            let messages = project_conversation_messages_with_native_history(
                stored,
                None,
                &std::collections::HashSet::new(),
                WORKSPACE,
                CONVERSATION,
                &self.log,
                &echo_agent::agent::CancellationToken::new(),
            )
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            *self
                .batch
                .lock()
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))? =
                Some(SavedChartBatch {
                    sequence: envelope.sequence,
                    messages,
                });
        }
        Ok(SinkControl::Continue)
    }
}

struct ReplaceChartOutput;
impl Guard for ReplaceChartOutput {
    fn name(&self) -> &str {
        "controlled-chart-output-transform"
    }
    fn check<'a>(
        &'a self,
        text: &'a str,
        direction: GuardDirection,
    ) -> BoxFuture<'a, echo_agent::error::Result<GuardResult>> {
        Box::pin(async move {
            if direction == GuardDirection::ToolOutput
                && text.contains("https://vega.github.io/schema/vega-lite/v5.json")
            {
                Ok(GuardResult::Transform {
                    content: "原Guard已替换图表输出".into(),
                    reasons: vec!["controlled original output transform".into()],
                })
            } else {
                Ok(GuardResult::Pass)
            }
        })
    }
}

fn saved_chart_specs(messages: &[SavedMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter_map(|message| message.native_presentation.as_ref())
        .flat_map(|supplement| &supplement.records)
        .filter_map(|record| match &record.payload {
            echo_agent_app_core::api::chat_event_log::NativePresentationPayload::Chart {
                spec,
                ..
            } => Some(spec.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn standard_chart_tool_real_producer_survives_saved_get_prune_and_cold_wrapper()
-> anyhow::Result<()> {
    for mode in [
        "json",
        "html",
        "guard",
        "same-name-text",
        "truncated",
        "spilled",
    ] {
        let root = tempfile::tempdir()?;
        let store_path = root.path().join("transcript");
        let log_path = root.path().join("events");
        let store = Arc::new(FileConversationStore::new(&store_path)?);
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 2,
            max_replay_events: 4,
        };
        let log = Arc::new(ChatEventLog::open(&log_path, retention)?);
        let arguments = serde_json::json!({"chart_type":"bar","title":"原标准工具公开图表","data":[{"类":"甲🙂","值":7}],"x_field":"类","y_field":"值","output_format":if mode=="html" {"html"} else {"json"},"width":240,"height":120});
        let block = AssistantMessageBlock {
            provider_source: None,
            block_id: "chart-progress".into(),
            request_id: "chart-request".into(),
            sequence: 0,
            phase: AssistantPhase::Commentary,
            delta: "我会用原工具生成公开图表".into(),
            complete: true,
            provider: Some("controlled-model".into()),
            reasoning: None,
        };
        let delta: DeltaMessage = serde_json::from_value(
            serde_json::json!({"role":"assistant","content":block.delta,"assistant_blocks":[block],
            "tool_calls":[{"index":0,"id":"actual-chart-call","type":"function","function":{"name":"generate_chart","arguments":serde_json::to_string(&arguments)?}}]}),
        )?;
        let llm = Arc::new(
            MockLlmClient::new()
                .with_model_name("controlled-chart-producer")
                .with_stream_script(vec![
                    StreamChunk::Delta(Box::new(delta)),
                    StreamChunk::Terminal {
                        finish_reason: Some("tool_calls".into()),
                        usage: None,
                    },
                ])
                .with_response("原标准图表工具已结束"),
        );
        let mut builder = ReactAgentBuilder::new()
            .llm_client(llm)
            .auto_project_rules(false)
            .conversation_id(CONVERSATION)
            .conversation_store(store.clone())
            .state_store(Arc::new(echo_agent::state::FileRuntimeStateStore::new(
                root.path().join("runtime"),
            )?))
            .max_iterations(3);
        if mode == "same-name-text" {
            // 真实非标准工具的反控：借同名和输出JSON均不构成公开产物格式声明。
            let parameters: ToolParameters = serde_json::from_value(arguments.clone())?;
            let real = echo_agent::tools::chart::GenerateChartTool
                .execute(parameters)
                .await?;
            builder = builder.tool(Box::new(
                echo_agent::testing::MockTool::new("generate_chart")
                    .with_parameters(echo_agent::tools::chart::GenerateChartTool.parameters())
                    .with_response(real.output),
            ));
        } else {
            builder = builder.tool(Box::new(echo_agent::tools::chart::GenerateChartTool));
        }
        if mode == "truncated" {
            builder = builder.max_tool_output_tokens(1);
        }
        if mode == "spilled" {
            builder = builder.tool_output_artifacts(
                echo_agent::tools::artifact::ToolOutputArtifactConfig::new(
                    root.path().join("artifacts"),
                    "conversation_or_30d",
                )
                .threshold_bytes(1),
            );
        }
        if mode == "guard" {
            builder = builder.guard(Arc::new(ReplaceChartOutput));
        }
        let agent = builder.build()?;
        let sink = ChartProducerSink {
            log: log.clone(),
            store: store.clone(),
            events: Default::default(),
            batch: Default::default(),
        };
        let identity =
            EventIdentity::for_chat(Some(CONVERSATION.into()), ROOT_TURN, ROOT_TURN, None)?
                .with_execution_id(EXECUTION)?;
        let invocation = AgentInvocationContext {
            runtime: Some(echo_agent::tools::ExternalRunContext {
                conversation_id: Some(CONVERSATION.into()),
                turn_id: Some(ROOT_TURN.into()),
                message_id: Some(ROOT_TURN.into()),
                execution_id: Some(EXECUTION.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let receipt = AgentTurnDriver
            .drive(
                &agent,
                TurnRequest::from_message(
                    identity,
                    Message::user("运行标准图表工具受控链".to_string()),
                )
                .mode(TurnMode::Chat)
                .invocation(invocation),
                &sink,
            )
            .await?;
        assert!(
            matches!(receipt.outcome, TurnOutcome::Completed),
            "真实producer必须先成功: {receipt:?}"
        );
        let events = sink
            .events
            .lock()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .clone();
        let result_event = events.iter().find(|event| matches!(&event.payload, ChatDriverEvent::Agent(agent) if matches!(&agent.payload,AgentEvent::ToolResult {name,result,..} if name=="generate_chart" && result.success)))
            .ok_or_else(|| anyhow::anyhow!("缺真正标准工具结果"))?;
        assert!(!events.iter().any(|event|matches!(&event.payload,ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::Chart{..}))),"不能人为造Chart事件掩盖producer缺口");
        if let ChatDriverEvent::Agent(agent) = &result_event.payload
            && let AgentEvent::ToolResult { result, .. } = &agent.payload
        {
            if mode == "json" {
                assert!(
                    result.data.is_some()
                        && result.mime_type.as_deref() == Some("application/vnd.vegalite.v5+json")
                );
            }
            if mode == "truncated" {
                assert!(result.truncated, "必须真正触发原pipeline截断");
            }
            if mode == "spilled" {
                assert!(result.artifact.is_some(), "必须真正触发原artifact spill");
            }
            if mode == "guard" {
                assert_eq!(
                    result.output, "原Guard已替换图表输出",
                    "必须真正触发原ToolOutput Guard替换"
                );
                assert!(
                    result.data.is_none() && result.mime_type.is_none(),
                    "原Guard替换须清除typed投影"
                );
            }
            if mode == "html" || mode == "same-name-text" {
                assert!(result.data.is_none() && result.mime_type.is_none());
            }
        }
        let final_sequence = events.iter().find(|event|matches!(&event.payload,ChatDriverEvent::Agent(agent) if matches!(agent.payload,AgentEvent::FinalAnswer(_))))
            .map(|event|event.sequence).ok_or_else(|| anyhow::anyhow!("缺原最终通知"))?;
        let batch = sink
            .batch
            .lock()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .take()
            .ok_or_else(|| anyhow::anyhow!("缺真正保存后get"))?;
        assert!(
            result_event.sequence < batch.sequence && batch.sequence < final_sequence,
            "get必须发生在原保存已确认、最终通知之前"
        );
        let before = saved_chart_specs(&batch.messages);
        assert_eq!(
            before.len(),
            if mode == "json" { 1 } else { 0 },
            "原保存后getter的图表资格错误: {mode}"
        );
        let actual_spec = match &result_event.payload {
            ChatDriverEvent::Agent(agent) => match &agent.payload {
                AgentEvent::ToolResult { result, .. } => result.data.clone(),
                _ => None,
            },
            _ => None,
        };
        if mode == "json" {
            assert_eq!(
                before.first(),
                actual_spec.as_ref(),
                "必须是实际工具data，不能手写expected spec冒producer"
            );
        }
        for _ in 0..12 {
            log.append(
                WORKSPACE,
                Some(CONVERSATION),
                ROOT_TURN,
                ChatDriverEvent::TurnStatus {
                    status: "completed".into(),
                },
            )?;
        }
        let tail = log.replay(WORKSPACE, Some(CONVERSATION), ROOT_TURN, 0)?;
        assert!(
            tail.retained_earliest_cursor
                .is_some_and(|floor| floor > result_event.sequence),
            "必须真正裁掉原结果再冷开"
        );
        drop(sink);
        drop(agent);
        drop(store);
        drop(log);
        let cold_store = FileConversationStore::new(&store_path)?;
        let cold_log = Arc::new(ChatEventLog::open(&log_path, retention)?);
        let cold = project_conversation_messages_with_native_history(
            cold_store.get_messages(CONVERSATION).await?,
            None,
            &std::collections::HashSet::new(),
            WORKSPACE,
            CONVERSATION,
            &cold_log,
            &echo_agent::agent::CancellationToken::new(),
        )
        .await?;
        assert_eq!(
            saved_chart_specs(&cold),
            before,
            "真实裁剪/cold/get不改变图表资格: {mode}"
        );
        if mode == "json"
            && let Some(output) = std::env::var_os("ECLIP_UI_STANDARD_CHART_FIXTURE_OUT")
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            std::io::Write::write_all(
                &mut file,
                &serde_json::to_vec_pretty(
                    &serde_json::json!({"verification":"actual ReactAgent->real GenerateChartTool->original ToolResult/Driver/FileStore/journal/checkpoint->actual saved/cold history wrapper; controlled model, not native App","events":events,"batch_sequence":batch.sequence,"after_batch_messages":batch.messages,"cold_messages":cold,"actual_spec":actual_spec}),
                )?,
            )?;
        }
    }
    Ok(())
}
