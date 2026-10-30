// 被既有resource_chain_acceptance测试模块include；无产品入口或第二执行器。
#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ToolCaptureDelivery {
    Agent {
        // 只缩小测试enum布局，序列化仍是原完整envelope，不改变回调/原件顺序。
        envelope: Box<ChatEventEnvelope>,
    },
    Tool {
        event: String,
        summary: Box<core::tool_execution::ToolExecutionSummary>,
    },
}

#[derive(Default)]
struct ToolCaptureRenderer {
    deliveries: std::sync::Mutex<Vec<ToolCaptureDelivery>>,
}

impl ToolCaptureRenderer {
    fn push(&self, delivery: ToolCaptureDelivery) -> bool {
        match self.deliveries.lock() {
            Ok(mut entries) => {
                entries.push(delivery);
                true
            }
            Err(_) => false,
        }
    }

    fn snapshot(&self) -> anyhow::Result<Vec<ToolCaptureDelivery>> {
        self.deliveries
            .lock()
            .map(|entries| entries.clone())
            .map_err(|_| anyhow::anyhow!("捕获renderer锁已损坏，拒绝部分原件"))
    }
}

impl core::chat_driver::ChatSink for ToolCaptureRenderer {
    fn on_event(&self, _event: ChatDriverEvent) -> bool {
        // 只接受GUI原共享journal边界之后的回调，禁止旁路原持久化/投影。
        false
    }

    fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
        self.push(ToolCaptureDelivery::Agent {
            envelope: Box::new(envelope),
        })
    }

    fn on_tool_execution_projection(
        &self,
        update: &core::tool_execution_projection::ToolExecutionProjectionUpdate,
    ) -> bool {
        let event = match update.kind {
            core::tool_execution_projection::ToolExecutionProjectionKind::Started => "started",
            core::tool_execution_projection::ToolExecutionProjectionKind::Finished => "finished",
        };
        self.push(ToolCaptureDelivery::Tool {
            event: event.into(),
            summary: Box::new(update.summary.clone()),
        })
    }
}

fn validate_tool_capture(
    publications: &[ChatEventEnvelope],
    deliveries: &[ToolCaptureDelivery],
) -> anyhow::Result<()> {
    let original = publications
        .iter()
        .filter(|envelope| matches!(envelope.payload, ChatDriverEvent::Agent(_)))
        .collect::<Vec<_>>();
    let observed = deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            ToolCaptureDelivery::Agent { envelope } => Some(envelope),
            _ => None,
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        serde_json::to_value(&original)? == serde_json::to_value(&observed)?,
        "GUI回调与冷journal原envelope逐字节/顺序不同"
    );
    let mut call = None;
    let mut result = None;
    let mut started = None;
    let mut finished = None;
    let mut assistant = false;
    let mut settled = 0_usize;
    let mut final_notice = 0_usize;
    for (position, delivery) in deliveries.iter().enumerate() {
        match delivery {
            ToolCaptureDelivery::Agent { envelope } => {
                let ChatDriverEvent::Agent(agent) = &envelope.payload else {
                    anyhow::bail!("捕获回调不得把非Agent来源改成Agent")
                };
                match &agent.payload {
                    echo_agent::agent::AgentEvent::AssistantMessage { .. } => assistant = true,
                    echo_agent::agent::AgentEvent::ToolCall {
                        call_id,
                        invocation,
                    } => {
                        anyhow::ensure!(call.is_none() && invocation.name == TOOL, "原调用不唯一");
                        call = Some((position, call_id, envelope));
                    }
                    echo_agent::agent::AgentEvent::ToolResult {
                        call_id,
                        name,
                        result: raw,
                    } => {
                        anyhow::ensure!(
                            result.is_none() && name == TOOL && raw.success,
                            "原结果缺失/失败/重复"
                        );
                        result = Some((position, call_id));
                    }
                    echo_agent::agent::AgentEvent::ExecutionStatus(status)
                        if status.status == echo_agent::agent::ExecutionStatusKind::Settled =>
                    {
                        settled = settled.saturating_add(1);
                    }
                    echo_agent::agent::AgentEvent::FinalAnswer(_) => {
                        final_notice = final_notice.saturating_add(1);
                    }
                    _ => {}
                }
            }
            ToolCaptureDelivery::Tool { event, summary } => {
                anyhow::ensure!(summary.name == TOOL, "工具摘要不是原工具");
                match event.as_str() {
                    "started" => {
                        anyhow::ensure!(
                            started.is_none()
                                && summary.status
                                    == core::tool_execution::ToolExecutionStatus::Running,
                            "原started摘要不同"
                        );
                        started = Some((position, summary));
                    }
                    "finished" => {
                        anyhow::ensure!(
                            finished.is_none()
                                && summary.status
                                    == core::tool_execution::ToolExecutionStatus::Succeeded,
                            "原finished摘要不同"
                        );
                        finished = Some((position, summary));
                    }
                    _ => anyhow::bail!("未知原工具摘要事件"),
                }
            }
        }
    }
    let (call_position, call_id, call_envelope) = call.context("缺真实tool_call")?;
    let (result_position, result_id) = result.context("缺真实tool_result")?;
    let (start_position, start) = started.context("缺真实started摘要")?;
    let (finish_position, finish) = finished.context("缺真实finished摘要")?;
    // 原shared sink先提交摘要再送对应envelope；不按想象的call→summary重排。
    anyhow::ensure!(
        start_position < call_position
            && call_position < finish_position
            && finish_position < result_position,
        "GUI共用sink原摘要/envelope时序改变"
    );
    anyhow::ensure!(
        call_id == result_id
            && start.call_id == *call_id
            && finish.call_id == *call_id
            && start.id == finish.id,
        "原call/result/summary身份不同"
    );
    let owner = core::tool_execution::ToolExecutionOwner::Chat {
        message_id: call_envelope.message_id.clone(),
    };
    for summary in [start, finish] {
        anyhow::ensure!(
            summary.owner == owner
                && summary.workspace_id == call_envelope.workspace_id
                && summary.conversation_id == call_envelope.conversation_id,
            "GUI摘要owner/address与原envelope不同"
        );
    }
    anyhow::ensure!(
        assistant && settled == 1 && final_notice == 1,
        "AssistantMessage/真实唯一Settled/最终通知缺失，不填造字段"
    );
    Ok(())
}

pub(super) async fn capture_tool_case(directory: &Path) -> anyhow::Result<serde_json::Value> {
    use super::native_timeline_actual_backend_capture::write_original;
    use core::workspace::layout::WorkspaceLayout;
    let root = tempfile::Builder::new()
        .prefix("tool-capture-source-")
        .tempdir_in(directory)?;
    let registry =
        core::workspace::registry::WorkspaceRegistry::with_base_dir(root.path().join("registry"))?;
    let workspace = registry.create_at(
        "tool-capture-source",
        core::workspace::WorkspaceKind::General,
        root.path().join("workspace"),
    )?;
    let conversation = "native-tool-capture-conversation";
    let log_path = root.path().join("events");
    let renderer = Arc::new(ToolCaptureRenderer::default());
    let seed = seed_and_close_with_surface_capture(
        &workspace,
        &log_path,
        conversation,
        NativeFixtureMode::ProgressSigned,
        SourceRows::Untouched,
        true,
        Some(renderer.clone()),
    )
    .await?;
    let deliveries = renderer.snapshot()?;
    let first = deliveries
        .iter()
        .find_map(|delivery| match delivery {
            ToolCaptureDelivery::Agent { envelope } => Some(envelope),
            _ => None,
        })
        .context("缺原GUI Agent回调")?;
    let log = ChatEventLog::open(&log_path, ChatEventRetention::default())?;
    let publications = log
        .replay(
            workspace.id.as_str(),
            Some(conversation),
            &first.root_turn_id,
            0,
        )?
        .events;
    validate_tool_capture(&publications, &deliveries)?;
    // 只变异内存副本验证缺项/错owner失败；原callbacks/journal不修改。
    let missing = deliveries.iter().filter(|entry| !matches!(entry, ToolCaptureDelivery::Tool { event, .. } if event == "finished")).cloned().collect::<Vec<_>>();
    anyhow::ensure!(
        validate_tool_capture(&publications, &missing).is_err(),
        "缺summary不得成功"
    );
    let mut wrong_owner = deliveries.clone();
    for entry in &mut wrong_owner {
        if let ToolCaptureDelivery::Tool { summary, .. } = entry {
            summary.conversation_id = Some("wrong-conversation-negative-control".into());
        }
    }
    anyhow::ensure!(
        validate_tool_capture(&publications, &wrong_owner).is_err(),
        "错owner不得成功"
    );
    let store = FileConversationStore::new(WorkspaceLayout::state_dir(&workspace.root))?;
    let rows = store.get_messages(conversation).await?;
    anyhow::ensure!(
        rows == seed.producer_rows && sha(&serde_json::to_vec(&rows)?) == seed.producer_rows_sha256,
        "原producer rows在捕获中改变"
    );
    let final_row = rows
        .iter()
        .rev()
        .find(|row| row.role == "assistant")
        .context("缺原final行")?;
    let canonical = echo_agent::memory::restore_message(final_row)?;
    let view = project_stored_message_with_publications(
        final_row.clone(),
        Some(workspace.id.as_str()),
        Some(conversation),
        &publications,
    )?;
    anyhow::ensure!(
        view.final_answer_confirmed && view.final_answer_source.is_some(),
        "get未确认publication，不捕获伪成功"
    );
    let terminal = publications
        .iter()
        .find_map(|envelope| match &envelope.payload {
            ChatDriverEvent::Agent(agent) => match &agent.payload {
                echo_agent::agent::AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Settled =>
                {
                    Some(status)
                }
                _ => None,
            },
            _ => None,
        })
        .context("缺实际terminal")?;
    anyhow::ensure!(
        view.execution_status.as_ref() == Some(terminal),
        "get不是原Settled"
    );
    let source = view.final_answer_source.as_ref().context("缺最终来源")?;
    anyhow::ensure!(
        source.execution_id == terminal.execution_id
            && Some(source.request_id.as_str()) == terminal.request_id.as_deref()
            && source.sequence == terminal.sequence,
        "get来源坐标不同"
    );
    let case_dir = directory.join("tool-progress-signed");
    std::fs::create_dir(&case_dir)?;
    let tools = deliveries
        .iter()
        .filter(|delivery| matches!(delivery, ToolCaptureDelivery::Tool { .. }))
        .cloned()
        .collect::<Vec<_>>();
    let source_rows = write_original(&case_dir, "source-rows.json", &rows)?;
    let source_stored = write_original(&case_dir, "source-stored-message.json", final_row)?;
    let canonical_message = write_original(&case_dir, "source-canonical-message.json", &canonical)?;
    let envelopes = write_original(&case_dir, "source-envelopes.json", &publications)?;
    let source_get_message = write_original(&case_dir, "source-get-message.json", &view)?;
    let tool_events = write_original(&case_dir, "tool-events.json", &tools)?;
    let delivery_trace = write_original(&case_dir, "delivery-trace.json", &deliveries)?;
    let actual_terminal = write_original(&case_dir, "actual-terminal.json", terminal)?;
    Ok(serde_json::json!({
        "case_id":"tool-progress-signed", "workspace_id":workspace.id.as_str(),
        "conversation_id":conversation, "root_turn_id":first.root_turn_id,
        "message_id":first.message_id, "execution_id":terminal.execution_id,
        "source_message_sha256":sha(&serde_json::to_vec(&canonical)?),
        "source_rows":source_rows, "source_stored":source_stored, "canonical_message":canonical_message,
        "envelopes":envelopes, "source_get_message":source_get_message, "tool_events":tool_events,
        "delivery_trace":delivery_trace, "actual_terminal":actual_terminal,
        "projection_origin":"original bind_surface_chat_sink(Gui) callbacks; original ToolExecutionProjector and repository",
        "negative_controls":["missing_finished_summary_rejected", "wrong_summary_owner_rejected"]
    }))
}
