use super::*;
use crate::conversation_projection::{NativeTurnSource, SavedExecutionStep, SavedMessage};
use crate::tasks::task_runtime::{
    RuntimeEventKind,
    executor::{ExecEvent, SubagentEventMetadata},
};
use echo_agent::agent::{
    AgentEvent, AgentPhase, AssistantMessageBlock, AssistantPhase, EventEnvelope, EventIdentity,
    ExecutionStatus, ExecutionStatusKind, ToolInvocation,
};

fn source() -> NativeTurnSource {
    NativeTurnSource {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        root_turn_id: "turn".into(),
        turn_id: "turn".into(),
        execution_id: "main-execution".into(),
    }
}

fn append(
    log: &ChatEventLog,
    sequence: u64,
    payload: AgentEvent,
) -> Result<ChatEventEnvelope, String> {
    let identity = EventIdentity::new("main-stream", "turn")
        .and_then(|value| value.with_conversation_id("conversation"))
        .and_then(|value| value.with_message_id("turn"))
        .and_then(|value| value.with_execution_id("main-execution"))
        .map_err(|error| error.to_string())?;
    let event = EventEnvelope::new(&identity, sequence, None, payload)
        .map_err(|error| error.to_string())?;
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Agent(Box::new(event)),
    )
    .map_err(|error| error.to_string())
}

fn block(id: &str, phase: AssistantPhase, text: &str) -> AssistantMessageBlock {
    AssistantMessageBlock {
        provider_source: None,
        block_id: id.into(),
        request_id: format!("request-{id}"),
        sequence: 0,
        phase,
        delta: text.into(),
        complete: true,
        provider: Some("controlled-fixture".into()),
        reasoning: None,
    }
}

fn subagent(
    log: &ChatEventLog,
    ordinal: u64,
    event: RuntimeEventKind,
    parent_event: &str,
) -> Result<ChatEventEnvelope, String> {
    let payload = if event == RuntimeEventKind::Started {
        serde_json::json!({"agent":"child-agent","task":"核对原记录","message_id":"turn"})
    } else {
        serde_json::json!({"agent":"child-agent","output":"原子代理结果🙂","duration_ms":1,"message_id":"turn"})
    };
    let exec = ExecEvent::subagent(
        "workspace",
        "conversation",
        "run",
        "task",
        "child-execution",
        event,
        payload,
    )
    .with_agent("child-agent")
    .with_framework_event(SubagentEventMetadata {
        schema_version: echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION,
        event_id: format!("child-native-{ordinal}"),
        content_hash: format!("controlled-hash-{ordinal}"),
        sequence: ordinal,
        stream_id: "child-stream".into(),
        turn_id: "child-execution".into(),
        message_id: Some("turn".into()),
        execution_id: "child-execution".into(),
        parent_event_id: Some(parent_event.into()),
        timestamp: Utc::now().to_rfc3339(),
        parent_agent: "main".into(),
        agent_name: "child-agent".into(),
        parent_execution_id: Some("main-execution".into()),
        agent_path: None,
        task_id: Some("task".into()),
        attempt: Some(1),
        plan_revision: Some(1),
    });
    log.append(
        "workspace",
        Some("conversation"),
        "turn",
        ChatDriverEvent::Execution(Box::new(exec)),
    )
    .map_err(|error| error.to_string())
}

fn read(log: &ChatEventLog) -> Result<NativePresentationSupplement, String> {
    loop {
        match log.native_presentation(&source()) {
            Err(ChatEventLogError::NativeHistoryPending { .. }) => continue,
            value => return value.map_err(|error| error.to_string()),
        }
    }
}

fn saved(
    log: &ChatEventLog,
    id: &str,
    canonical: &echo_agent::llm::types::Message,
    supplement: Option<NativePresentationSupplement>,
) -> Result<SavedMessage, String> {
    let original = log
        .native_history_publications("workspace", "conversation", canonical)
        .map_err(|error| error.to_string())?;
    let view = crate::conversation_projection::project_native_history_publication(
        canonical,
        "workspace",
        "conversation",
        &original,
    )?
    .ok_or("native view missing")?;
    let mut steps = vec![SavedExecutionStep {
        step_type: "assistant".into(),
        index: Some(0),
        call_id: None,
    }];
    if canonical
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty())
    {
        steps.push(SavedExecutionStep {
            step_type: "tool".into(),
            index: None,
            call_id: Some("call-1".into()),
        });
    }
    Ok(SavedMessage {
        message_id: Some(id.into()),
        role: "assistant".into(),
        created_at: Utc::now().to_rfc3339(),
        content: view.content,
        native_presentation: supplement,
        presentation_unavailable: false,
        assistant_blocks: Some(view.blocks),
        final_answer_confirmed: view.final_answer_confirmed,
        execution_status: view.execution_status,
        final_answer_source: view.final_answer_source,
        turn_source: view.turn_source,
        internal_agent: false,
        agent_input: None,
        tool_calls: canonical.tool_calls.clone(),
        tool_result: None,
        execution_steps: Some(steps),
        attachments: None,
    })
}

#[test]
fn native_presentation_keeps_original_order_and_public_artifacts_after_4096_prune_cold_reopen()
-> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let retention = ChatEventRetention {
        segment_rollover_bytes: 1,
        max_segments: 2,
        max_replay_events: 4,
    };
    let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    append(
        &log,
        1,
        AgentEvent::ExecutionStatus(ExecutionStatus {
            adapter_generated: false,
            execution_id: "main-execution".into(),
            request_id: None,
            sequence: 1,
            phase: AgentPhase::Thinking,
            status: ExecutionStatusKind::Started,
            reason: None,
        }),
    )?;
    let a = block("a", AssistantPhase::Commentary, "先核对来源");
    let b = block("b", AssistantPhase::PartialAnswer, "已得到子运行记录");
    let final_block = block("final", AssistantPhase::FinalAnswer, "最终结果独立显示");
    let first = append(&log, 2, AgentEvent::AssistantMessage { block: a.clone() })?;
    let tool = append(
        &log,
        3,
        AgentEvent::ToolCall {
            call_id: "call-1".into(),
            invocation: ToolInvocation {
                requested_name: "agent_tool".into(),
                requested_args: serde_json::json!({"private_argument":"do-not-copy-this-argument"}),
                name: "agent_tool".into(),
                args: serde_json::json!({"private_argument":"do-not-copy-this-argument"}),
                rewrites: Vec::new(),
            },
        },
    )?;
    let native_tool = match &tool.payload {
        ChatDriverEvent::Agent(event) => event.event_id.to_string(),
        _ => return Err("tool identity missing".into()),
    };
    let child = subagent(&log, 1, RuntimeEventKind::Started, &native_tool)?;
    let spec = serde_json::json!({"$schema":"https://vega.github.io/schema/vega-lite/v5.json","mark":"bar","data":{"values":[{"name":"原图表🙂","value":7}]}});
    let chart = append(&log, 4, AgentEvent::Chart { spec: spec.clone() })?;
    let second = append(&log, 5, AgentEvent::AssistantMessage { block: b.clone() })?;
    subagent(&log, 2, RuntimeEventKind::Completed, &native_tool)?;
    append(
        &log,
        6,
        AgentEvent::AssistantMessage {
            block: final_block.clone(),
        },
    )?;
    let settled = ExecutionStatus {
        adapter_generated: false,
        execution_id: "main-execution".into(),
        request_id: Some("request-final".into()),
        sequence: 2,
        phase: AgentPhase::Terminal,
        status: ExecutionStatusKind::Settled,
        reason: None,
    };
    append(&log, 7, AgentEvent::FinalAnswer("最终结果独立显示".into()))?;
    append(&log, 8, AgentEvent::ExecutionStatus(settled.clone()))?;
    let live = read(&log)?;
    assert_eq!(
        live.records
            .iter()
            .map(|record| record.cursor.sequence)
            .collect::<Vec<_>>(),
        vec![
            tool.sequence,
            child.sequence,
            chart.sequence,
            second.sequence.saturating_add(1)
        ]
    );
    assert!(live.prefix_complete);
    let serialized = serde_json::to_string(&live).map_err(|error| error.to_string())?;
    assert!(
        !serialized.contains("do-not-copy-this-argument"),
        "只保存工具引用，不能永久复制原参数"
    );
    for _ in 0..4100 {
        log.append(
            "workspace",
            Some("conversation"),
            "turn",
            ChatDriverEvent::TurnStatus {
                status: "completed".into(),
            },
        )
        .map_err(|error| error.to_string())?;
    }
    let tail = log
        .replay("workspace", Some("conversation"), "turn", 0)
        .map_err(|error| error.to_string())?;
    assert!(
        tail.retained_earliest_cursor
            .is_some_and(|floor| floor > second.sequence)
    );
    drop(log);
    let cold_log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
    let cold = read(&cold_log)?;
    assert_eq!(
        serde_json::to_value(&live.records).map_err(|error| error.to_string())?,
        serde_json::to_value(&cold.records).map_err(|error| error.to_string())?
    );
    assert_eq!(cold.journal_identity, live.journal_identity);
    let canonical = |value: AssistantMessageBlock| {
        let mut message = echo_agent::llm::types::Message::assistant(value.delta.clone());
        message.assistant_blocks = Some(vec![value.clone()]);
        message.approved_assistant_blocks = Some(vec![value]);
        message
    };
    let mut ca = canonical(a);
    ca.tool_calls = Some(vec![echo_agent::llm::types::ToolCall {
        provider_source: None,
        id: "call-1".into(),
        call_type: "function".into(),
        function: echo_agent::llm::types::FunctionCall {
            name: "agent_tool".into(),
            arguments: "{}".into(),
        },
    }]);
    let cb = canonical(b);
    let mut cf = canonical(final_block);
    cf.execution_status = Some(settled);
    let messages = vec![
        saved(&cold_log, "a", &ca, Some(cold.clone()))?,
        saved(&cold_log, "b", &cb, None)?,
        saved(&cold_log, "final", &cf, None)?,
    ];
    assert_eq!(
        messages
            .first()
            .and_then(|message| message.assistant_blocks.as_ref())
            .and_then(|blocks| blocks.first())
            .and_then(|block| block.journal_source.as_ref())
            .map(|cursor| cursor.sequence),
        Some(first.sequence)
    );
    assert!(
        messages
            .last()
            .is_some_and(|message| message.final_answer_confirmed)
    );
    // 给同源React消费的控制夹具；默认测试不写产品数据，不运行模型/工具/App。
    if let Some(output) = std::env::var_os("ECLIP_UI_PRESENTATION_FIXTURE_OUT") {
        let data = serde_json::json!({"verification":"controlled original ChatEventLog -> real checkpoint/prune/cold DTO; not live provider/native App","live":live,"cold":cold,"messages":messages});
        fs::write(
            PathBuf::from(output),
            serde_json::to_vec_pretty(&data).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    let selected =
        stream_id("workspace", Some("conversation"), "turn").map_err(|error| error.to_string())?;
    let path = cold_log
        .stream_dir(&selected)
        .join(NATIVE_HISTORY_DIRECTORY);
    let coverage =
        FileCheckpointStore::<NativePublicationCoverage>::open(path.join("coverage.json"));
    let mut old = coverage
        .load()
        .map_err(|error| error.to_string())?
        .ok_or("coverage missing")?;
    old.state.presentation = None;
    coverage
        .save(&old.journal_identity, &old.state, old.sequence)
        .map_err(|error| error.to_string())?;
    fs::remove_file(path.join(format!(
        "{}.json",
        presentation_key(&selected, "turn").map_err(|error| error.to_string())?
    )))
    .map_err(|error| error.to_string())?;
    let partial = read(&cold_log)?;
    assert!(!partial.prefix_complete);
    assert!(partial.records.is_empty());
    assert!(
        saved(&cold_log, "final", &cf, None)?.final_answer_confirmed,
        "展示缺口不改变原final许可"
    );
    Ok(())
}

#[test]
fn native_presentation_new_generation_never_proves_old_parent_had_no_artifacts()
-> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
        .map_err(|error| error.to_string())?;
    log.append(
        "workspace",
        Some("conversation"),
        "new-turn",
        ChatDriverEvent::TurnStatus {
            status: "completed".into(),
        },
    )
    .map_err(|error| error.to_string())?;
    assert!(matches!(
        log.native_presentation(&source()),
        Err(ChatEventLogError::NativeHistoryGap { .. })
    ));
    Ok(())
}

// 正向调用真实标准工具；反控只变原结果/调用见证，不假造Chart事件。
#[tokio::test]
async fn standard_chart_result_projection_requires_declared_public_data_and_exact_call_scope()
-> Result<(), String> {
    use echo_agent::tools::{Tool as _, ToolParameters};
    for mode in [
        "json",
        "duplicate",
        "native-content-conflict",
        "call-pruned",
        "duplicate-pruned",
        "plain",
        "failed",
        "truncated",
        "foreign-call",
        "foreign-execution",
        "child",
        "bad-mime",
        "bad-schema",
        "ambiguous",
        "missing-call",
    ] {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = if mode == "call-pruned" || mode == "duplicate-pruned" {
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 2,
                max_replay_events: 4,
            }
        } else {
            ChatEventRetention::default()
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        append(
            &log,
            1,
            AgentEvent::ExecutionStatus(ExecutionStatus {
                adapter_generated: false,
                execution_id: "main-execution".into(),
                request_id: None,
                sequence: 1,
                phase: AgentPhase::Thinking,
                status: ExecutionStatusKind::Started,
                reason: None,
            }),
        )?;
        let args = serde_json::json!({"chart_type":"bar","x_field":"类","y_field":"值","data":[{"类":"甲🙂","值":7}]});
        let parameters: ToolParameters =
            serde_json::from_value(args.clone()).map_err(|error| error.to_string())?;
        let mut result = echo_agent::tools::chart::GenerateChartTool
            .execute(parameters)
            .await
            .map_err(|error| error.to_string())?;
        result.metadata.insert(
            "private_log".into(),
            "must-not-copy-private-chart-log".into(),
        );
        if mode != "missing-call" {
            let payload = AgentEvent::ToolCall {
                call_id: "actual-chart-call".into(),
                invocation: ToolInvocation {
                    requested_name: "generate_chart".into(),
                    requested_args: args.clone(),
                    name: "generate_chart".into(),
                    args: args.clone(),
                    rewrites: Vec::new(),
                },
            };
            let call = if mode == "foreign-execution" || mode == "child" {
                let turn = if mode == "child" {
                    "child-turn"
                } else {
                    "turn"
                };
                let identity = EventIdentity::new("foreign-chart-stream", turn)
                    .and_then(|id| id.with_conversation_id("conversation"))
                    .and_then(|id| id.with_message_id("turn"))
                    .and_then(|id| id.with_execution_id("foreign-execution"))
                    .map_err(|error| error.to_string())?;
                log.append(
                    "workspace",
                    Some("conversation"),
                    "turn",
                    ChatDriverEvent::Agent(Box::new(
                        EventEnvelope::new(&identity, 2, None, payload)
                            .map_err(|error| error.to_string())?,
                    )),
                )
                .map_err(|error| error.to_string())?
            } else {
                append(&log, 2, payload)?
            };
            if mode == "call-pruned" || mode == "duplicate-pruned" {
                for _ in 0..20 {
                    log.append(
                        "workspace",
                        Some("conversation"),
                        "turn",
                        ChatDriverEvent::TurnStatus {
                            status: "running".into(),
                        },
                    )
                    .map_err(|error| error.to_string())?;
                }
                // running 只 Flush；追加原非终态 SyncData safe point，才能触发真实 coverage/prune。
                log.append(
                    "workspace",
                    Some("conversation"),
                    "turn",
                    ChatDriverEvent::TurnConfiguration {
                        permission_mode: "default".into(),
                        approval_policy: "on-request".into(),
                        attachments: Vec::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                assert!(
                    log.replay("workspace", Some("conversation"), "turn", 0)
                        .map_err(|error| error.to_string())?
                        .retained_earliest_cursor
                        .is_some_and(|floor| floor > call.sequence)
                );
            }
            if mode == "duplicate" || mode == "duplicate-pruned" {
                log.append(
                    "workspace",
                    Some("conversation"),
                    "turn",
                    call.payload.clone(),
                )
                .map_err(|error| error.to_string())?;
            }
            if mode == "native-content-conflict" {
                let ChatDriverEvent::Agent(original) = &call.payload else {
                    return Err("原调用类型丢失".into());
                };
                let mut changed = original.as_ref().clone();
                if let AgentEvent::ToolCall { invocation, .. } = &mut changed.payload {
                    invocation
                        .args
                        .as_object_mut()
                        .ok_or("原调用args不是对象")?
                        .insert(
                            "title".into(),
                            serde_json::json!("同nativeID的不同调用内容"),
                        );
                }
                // 仅故障反控：保相同原nativeID，改变内容摘要，不能冒第二次真实工具执行。
                changed.content_hash = format!("conflict-{}", changed.content_hash);
                log.append(
                    "workspace",
                    Some("conversation"),
                    "turn",
                    ChatDriverEvent::Agent(Box::new(changed)),
                )
                .map_err(|error| error.to_string())?;
            }
            if mode == "ambiguous" {
                append(
                    &log,
                    3,
                    AgentEvent::ToolCall {
                        call_id: "actual-chart-call".into(),
                        invocation: ToolInvocation {
                            requested_name: "generate_chart".into(),
                            requested_args: args.clone(),
                            name: "generate_chart".into(),
                            args,
                            rewrites: Vec::new(),
                        },
                    },
                )?;
            }
        }
        match mode {
            "plain" => {
                result.data = None;
                result.mime_type = None;
            }
            "failed" => {
                result.success = false;
                result.error = Some("原工具失败".into());
            }
            "truncated" => result.truncated = true,
            "bad-mime" => result.mime_type = Some("application/json".into()),
            "bad-schema" => {
                let data = result
                    .data
                    .as_mut()
                    .and_then(serde_json::Value::as_object_mut)
                    .ok_or("真实标准工具data缺失")?;
                data.insert(
                    "$schema".into(),
                    serde_json::json!("https://example.invalid/not-vega"),
                );
            }
            _ => {}
        }
        append(
            &log,
            4,
            AgentEvent::ToolResult {
                call_id: if mode == "foreign-call" {
                    "other-call"
                } else {
                    "actual-chart-call"
                }
                .into(),
                name: "generate_chart".into(),
                result,
            },
        )?;
        let supplement = read(&log)?;
        let charts = supplement
            .records
            .iter()
            .filter(|record| matches!(record.payload, NativePresentationPayload::Chart { .. }))
            .count();
        assert_eq!(
            charts,
            usize::from(mode == "json" || mode == "duplicate" || mode == "call-pruned"),
            "原调用/公开data资格错误: {mode}"
        );
        let serialized = serde_json::to_string(&supplement).map_err(|error| error.to_string())?;
        assert!(!serialized.contains("must-not-copy-private-chart-log"));
        assert!(!serialized.contains("requested_args"));
    }
    Ok(())
}
