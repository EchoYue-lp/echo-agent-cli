//! 原生消息消费与真实journal重开；测试不发送任何外部渠道消息。
use super::{ChannelRenderEvent, ChannelSurfaceSink, aggregate_by_sentence};
use crate::chat_driver::{ChatDriverEvent, ChatSink, TurnOutcome};
use echo_agent::agent::{
    AgentEvent, AssistantMessageBlock, AssistantPhase, CancellationToken, EventEnvelope,
    EventIdentity,
};
use echo_agent::channels::ChatType;
use futures::{FutureExt, StreamExt, TryStreamExt};

fn agent_event(sequence: u64, payload: AgentEvent) -> echo_agent::error::Result<ChatDriverEvent> {
    let identity = EventIdentity::new("native-im-stream", "native-im-turn")?
        .with_conversation_id("native-conversation")?;
    Ok(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
        &identity, sequence, None, payload,
    )?)))
}

fn block(phase: AssistantPhase, text: &str, sequence: u64, complete: bool) -> AgentEvent {
    AgentEvent::AssistantMessage {
        block: AssistantMessageBlock {
            provider_source: None,
            block_id: format!("block-{sequence}"),
            request_id: "request-original".into(),
            sequence,
            phase,
            delta: text.into(),
            complete,
            provider: Some("native-test".into()),
            reasoning: None,
        },
    }
}

fn render(event: ChatDriverEvent) -> echo_agent::error::Result<ChannelRenderEvent> {
    Ok(ChannelRenderEvent::Driver(event))
}

async fn texts(
    events: Vec<echo_agent::error::Result<ChannelRenderEvent>>,
) -> anyhow::Result<Vec<String>> {
    let output = aggregate_by_sentence(
        futures::stream::iter(events).boxed(),
        "qqbot".into(),
        "local-test".into(),
        ChatType::Direct,
    )
    .await;
    Ok(output.map_ok(|message| message.text).try_collect().await?)
}

#[test]
fn native_sink_keeps_reasoning_signature_and_original_envelope_order() -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
    let sink = ChannelSurfaceSink::new(tx, CancellationToken::new());
    let mut reasoning = block(AssistantPhase::Reasoning, "", 1, false);
    if let AgentEvent::AssistantMessage { block } = &mut reasoning {
        block.reasoning = Some(echo_agent::llm::types::ReasoningBlock::Signed {
            thinking: "原生思考🙂".into(),
            signature: "signature-original".into(),
        });
    }
    let events = [
        agent_event(1, reasoning)?,
        agent_event(2, block(AssistantPhase::Commentary, "公开汇报。", 2, true))?,
    ];
    for event in &events {
        assert!(sink.on_event(event.clone()));
    }
    assert!(sink.on_event(agent_event(4, AgentEvent::FinalAnswer("确认回答".into()))?));
    for expected in events {
        let ChannelRenderEvent::Driver(actual) = rx.try_recv()? else {
            return Err(anyhow::anyhow!("原生事件失去原envelope"));
        };
        assert_eq!(
            serde_json::to_value(actual)?,
            serde_json::to_value(expected)?
        );
    }
    let ChannelRenderEvent::Driver(ChatDriverEvent::Agent(final_event)) = rx.try_recv()? else {
        return Err(anyhow::anyhow!("原确认回答未按顺序交付"));
    };
    assert!(matches!(final_event.payload, AgentEvent::FinalAnswer(_)));
    assert!(rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn native_public_blocks_keep_tool_order_and_only_confirmed_answer_is_sent_once()
-> anyhow::Result<()> {
    let before = agent_event(
        1,
        block(AssistantPhase::Commentary, "先核对原文件。", 1, true),
    )?;
    let after = agent_event(
        3,
        block(AssistantPhase::PartialAnswer, "已找到具体问题。", 3, true),
    )?;
    let final_answer = agent_event(6, AgentEvent::FinalAnswer("最终确认回答🙂".into()))?;
    let events = vec![
        render(before.clone()),
        render(before),
        render(agent_event(
            2,
            AgentEvent::ToolCall {
                call_id: "call-original".into(),
                invocation: echo_agent::agent::ToolInvocation {
                    requested_name: "read_file".into(),
                    requested_args: serde_json::json!({"path":"src/main.rs"}),
                    name: "read_file".into(),
                    args: serde_json::json!({"path":"src/main.rs"}),
                    rewrites: Vec::new(),
                },
            },
        )?),
        render(after),
        render(agent_event(
            4,
            block(AssistantPhase::FinalAnswer, "未校验候选，不能发送", 4, true),
        )?),
        render(final_answer.clone()),
        render(final_answer),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
    ];
    let output = texts(events).await?;
    assert_eq!(output.first().map(String::as_str), Some("先核对原文件。"));
    assert!(
        output
            .get(1)
            .is_some_and(|value| value.contains("[tool:call-original] started read_file"))
    );
    assert_eq!(output.get(2).map(String::as_str), Some("已找到具体问题。"));
    assert_eq!(output.last().map(String::as_str), Some("最终确认回答🙂"));
    assert_eq!(output.len(), 4);
    assert!(!output.concat().contains("未校验候选"));
    Ok(())
}

#[tokio::test]
async fn native_unknown_text_is_released_only_when_same_block_is_classified_commentary()
-> anyhow::Result<()> {
    let initial = block(AssistantPhase::Unknown, "明确工具前汇报。", 1, false);
    let mut classified = block(AssistantPhase::Commentary, "", 2, true);
    if let AgentEvent::AssistantMessage { block } = &mut classified {
        block.block_id = "block-1".into();
    }
    let output = texts(vec![
        render(agent_event(1, initial)?),
        render(agent_event(2, classified)?),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
    ])
    .await?;
    assert_eq!(output, vec!["明确工具前汇报。"]);
    Ok(())
}

#[tokio::test]
async fn native_candidate_and_confirmed_answer_never_close_the_original_foreground()
-> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(render(agent_event(
        1,
        block(AssistantPhase::FinalAnswer, "候选", 1, true),
    )?))
    .await
    .map_err(|_| anyhow::anyhow!("native test channel closed"))?;
    let events = futures::stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|event| (event, rx))
    })
    .boxed();
    let mut output = aggregate_by_sentence(
        events,
        "qqbot".into(),
        "local-test".into(),
        ChatType::Direct,
    )
    .await;
    assert!(
        output.next().now_or_never().is_none(),
        "块complete不能制造已确认回答或关闭原foreground"
    );
    tx.send(render(agent_event(
        2,
        AgentEvent::FinalAnswer("原执行已确认回答".into()),
    )?))
    .await
    .map_err(|_| anyhow::anyhow!("native test channel closed"))?;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await
            .map_err(|_| anyhow::anyhow!("OLD channel renderer swallowed the runtime-confirmed answer before the original foreground Terminal was sent"))?
            .transpose()?
            .map(|message| message.text)
            .as_deref(),
        Some("原执行已确认回答")
    );
    assert!(
        output.next().now_or_never().is_none(),
        "确认回答不关闭foreground"
    );
    tx.send(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)))
        .await
        .map_err(|_| anyhow::anyhow!("native test channel closed"))?;
    // Sender remains live: only the actual Terminal may close this renderer.
    let after_terminal = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
        .await
        .map_err(|_| {
            anyhow::anyhow!("original foreground Terminal did not close the channel renderer")
        })?;
    assert!(after_terminal.is_none());
    Ok(())
}

#[tokio::test]
async fn native_candidate_or_unknown_never_publishes_without_confirmed_final() -> anyhow::Result<()>
{
    for phase in [
        AssistantPhase::FinalAnswer,
        AssistantPhase::Unknown,
        AssistantPhase::Reasoning,
    ] {
        let output = texts(vec![
            render(agent_event(1, block(phase, "未确认内容", 1, true))?),
            Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
        ])
        .await?;
        assert!(output.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn native_failed_cancelled_or_receipt_eof_never_releases_unconfirmed_candidate()
-> anyhow::Result<()> {
    for terminal in [
        Some(TurnOutcome::Cancelled),
        Some(TurnOutcome::Failed(
            echo_agent::error::AgentFailure::message("test-failed", "原结算失败"),
        )),
        None,
    ] {
        let mut events = vec![render(agent_event(
            1,
            block(AssistantPhase::FinalAnswer, "不得发送的候选正文", 1, true),
        )?)];
        if let Some(outcome) = terminal {
            events.push(Ok(ChannelRenderEvent::Terminal(outcome)));
        }
        let output = texts(events).await?;
        assert!(!output.concat().contains("不得发送的候选正文"));
    }
    Ok(())
}

#[tokio::test]
async fn native_real_journal_reopen_preserves_signature_block_coordinates_and_replay_order()
-> anyhow::Result<()> {
    use crate::chat_event_log::{ChatEventLog, ChatEventRetention};
    let temporary = tempfile::tempdir()?;
    let native_reasoning = [
        echo_agent::llm::types::ReasoningBlock::Signed {
            thinking: "signed-original".into(),
            signature: "signature-original".into(),
        },
        echo_agent::llm::types::ReasoningBlock::Redacted {
            data: "redacted-original".into(),
        },
        echo_agent::llm::types::ReasoningBlock::Opaque {
            provider: "responses".into(),
            id: "opaque-item-original".into(),
            data: "encrypted-original".into(),
            summary: vec!["provider-summary-original".into()],
        },
    ];
    let mut original = Vec::new();
    for (index, reasoning) in native_reasoning.into_iter().enumerate() {
        let sequence = u64::try_from(index)?.saturating_add(1);
        let mut payload = block(AssistantPhase::Reasoning, "", sequence, true);
        if let AgentEvent::AssistantMessage { block } = &mut payload {
            block.reasoning = Some(reasoning);
        }
        original.push(agent_event(sequence, payload)?);
    }
    original.push(agent_event(
        4,
        block(AssistantPhase::Commentary, "reopened progress", 4, true),
    )?);
    original.push(agent_event(
        5,
        AgentEvent::FinalAnswer("reopened confirmed answer".into()),
    )?);
    let hashes;
    {
        let log = ChatEventLog::open(temporary.path(), ChatEventRetention::default())?;
        hashes = original
            .iter()
            .map(|event| {
                log.append(
                    "native-workspace",
                    Some("native-conversation"),
                    "native-im-turn",
                    event.clone(),
                )
                .map(|event| event.content_hash)
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    let reopened = ChatEventLog::open(temporary.path(), ChatEventRetention::default())?;
    let replay = reopened.replay(
        "native-workspace",
        Some("native-conversation"),
        "native-im-turn",
        0,
    )?;
    assert_eq!(replay.events.len(), original.len());
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let sink = ChannelSurfaceSink::new(tx, CancellationToken::new());
    for ((actual, expected), hash) in replay.events.iter().zip(&original).zip(hashes) {
        assert_eq!(actual.content_hash, hash);
        assert_eq!(
            serde_json::to_value(&actual.payload)?,
            serde_json::to_value(expected)?
        );
        assert!(sink.on_journaled_event(actual.clone()));
        let ChannelRenderEvent::Journaled(delivered) = rx.try_recv()? else {
            return Err(anyhow::anyhow!("重开事件失去journal坐标"));
        };
        assert_eq!(
            serde_json::to_value(&delivered)?,
            serde_json::to_value(actual)?
        );
    }
    let mut events = Vec::new();
    for event in replay.events {
        events.push(Ok(ChannelRenderEvent::Journaled(Box::new(event.clone()))));
        events.push(Ok(ChannelRenderEvent::Journaled(Box::new(event))));
    }
    events.push(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)));
    assert_eq!(
        texts(events).await?,
        vec!["reopened progress", "reopened confirmed answer"]
    );
    Ok(())
}

#[test]
fn native_more_than_512_deltas_preserve_coordinates_with_a_drained_reader() -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
    let cancel = CancellationToken::new();
    let sink = ChannelSurfaceSink::new(tx, cancel.clone());
    for sequence in 1..=513 {
        let mut delta = block(AssistantPhase::Commentary, "x", sequence, false);
        if let AgentEvent::AssistantMessage { block } = &mut delta {
            block.block_id = "stable-block".into();
        }
        let event = agent_event(sequence, delta)?;
        assert!(sink.on_event(event.clone()));
        let ChannelRenderEvent::Driver(delivered) = rx.try_recv()? else {
            return Err(anyhow::anyhow!("native delta identity lost"));
        };
        assert_eq!(
            serde_json::to_value(delivered)?,
            serde_json::to_value(event)?
        );
    }
    assert!(!cancel.is_cancelled());
    assert!(sink.on_event(agent_event(
        514,
        AgentEvent::FinalAnswer("confirmed".into())
    )?));
    assert!(
        matches!(rx.try_recv()?, ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope)) if matches!(&envelope.payload, AgentEvent::FinalAnswer(_)))
    );
    Ok(())
}

#[test]
fn native_actual_queue_backpressure_still_cancels_the_original_token() -> anyhow::Result<()> {
    let (tx, _rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
    let cancel = CancellationToken::new();
    let sink = ChannelSurfaceSink::new(tx, cancel.clone());
    for index in 0..super::CHANNEL_EVENT_QUEUE_CAPACITY {
        let sequence = u64::try_from(index)?.saturating_add(1);
        assert!(sink.on_event(agent_event(
            sequence,
            block(AssistantPhase::Commentary, "x", sequence, false)
        )?));
    }
    assert!(!sink.on_event(agent_event(
        65,
        block(AssistantPhase::Commentary, "x", 65, false)
    )?));
    assert!(cancel.is_cancelled());
    Ok(())
}

fn scoped_event(
    stream: &str,
    turn: &str,
    sequence: u64,
    payload: AgentEvent,
) -> echo_agent::error::Result<ChatDriverEvent> {
    Ok(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
        &EventIdentity::new(stream, turn)?,
        sequence,
        None,
        payload,
    )?)))
}

#[tokio::test]
async fn native_confirmed_execution_a_stays_before_execution_b_progress_and_cancel()
-> anyhow::Result<()> {
    let output = texts(vec![
        render(scoped_event(
            "stream-a",
            "turn-a",
            1,
            block(AssistantPhase::FinalAnswer, "candidate-a", 1, true),
        )?),
        render(scoped_event(
            "stream-a",
            "turn-a",
            2,
            AgentEvent::FinalAnswer("confirmed-a".into()),
        )?),
        render(scoped_event(
            "stream-b",
            "turn-b",
            1,
            block(AssistantPhase::Commentary, "progress-b", 1, true),
        )?),
        render(scoped_event(
            "stream-b",
            "turn-b",
            2,
            block(AssistantPhase::FinalAnswer, "candidate-b", 2, true),
        )?),
        Ok(ChannelRenderEvent::Prompt(
            "/reply original-target answer".into(),
        )),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Cancelled)),
    ])
    .await?;
    assert_eq!(output.first().map(String::as_str), Some("confirmed-a"));
    assert_eq!(output.get(1).map(String::as_str), Some("progress-b"));
    assert_eq!(
        output.get(2).map(String::as_str),
        Some("/reply original-target answer")
    );
    assert!(
        output
            .last()
            .is_some_and(|value| value.starts_with("[cancelled]"))
    );
    assert!(!output.concat().contains("candidate"));
    Ok(())
}

#[tokio::test]
async fn native_confirmed_answer_and_cancellation_both_survive_exhausted_ordinary_output()
-> anyhow::Result<()> {
    // 只构造旧版已有的render事件，最终沿原aggregate/transport生成OutboundMessage。
    // 256条普通提示会耗尽原消息额度；使用飞书既有节流策略约需32秒，不改配额或时钟。
    let mut events = (0..256)
        .map(|index| Ok(ChannelRenderEvent::Prompt(format!("message-{index}"))))
        .collect::<Vec<_>>();
    events.push(render(agent_event(
        1,
        block(
            AssistantPhase::FinalAnswer,
            "unconfirmed-candidate",
            1,
            true,
        ),
    )?));
    events.push(render(agent_event(
        2,
        AgentEvent::FinalAnswer("original confirmed answer".into()),
    )?));
    events.push(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Cancelled)));
    let output = aggregate_by_sentence(
        futures::stream::iter(events).boxed(),
        "feishu".into(),
        "local-test".into(),
        ChatType::Direct,
    )
    .await
    .map_ok(|message| message.text)
    .try_collect::<Vec<_>>()
    .await?;
    assert!(output.len() <= super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
    assert_eq!(
        output
            .iter()
            .filter(|value| value.contains("original confirmed answer"))
            .count(),
        1
    );
    assert_eq!(
        output.last().map(String::as_str),
        Some("[cancelled] The channel turn was cancelled.")
    );
    assert!(!output.concat().contains("unconfirmed-candidate"));
    Ok(())
}

#[tokio::test]
async fn native_unknown_buffer_is_bounded_and_never_crosses_execution_identity()
-> anyhow::Result<()> {
    let mut classification = block(AssistantPhase::Commentary, "", 2, true);
    if let AgentEvent::AssistantMessage { block } = &mut classification {
        block.block_id = "block-1".into();
    }
    let output = texts(vec![
        render(scoped_event(
            "stream-a",
            "turn-a",
            1,
            block(AssistantPhase::Unknown, "private-a", 1, false),
        )?),
        render(scoped_event("stream-b", "turn-b", 1, classification)?),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
    ])
    .await?;
    assert!(output.is_empty());
    let result = texts(vec![render(agent_event(
        1,
        block(
            AssistantPhase::Unknown,
            // 原生产private事件预算为512KiB；构造其精确越界，不扩大可见性。
            &"x".repeat((512_usize * 1024).saturating_add(1)),
            1,
            false,
        ),
    )?)])
    .await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn native_incremental_same_block_is_joined_without_losing_or_repeating_deltas()
-> anyhow::Result<()> {
    let mut events = Vec::new();
    for (index, (text, complete)) in [("先", false), ("核对。", false), ("", true)]
        .into_iter()
        .enumerate()
    {
        let sequence = u64::try_from(index)?.saturating_add(1);
        let mut delta = block(AssistantPhase::Commentary, text, sequence, complete);
        if let AgentEvent::AssistantMessage { block } = &mut delta {
            block.block_id = "stable-block".into();
        }
        let event = agent_event(sequence, delta)?;
        events.push(render(event.clone()));
        events.push(render(event));
    }
    events.push(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)));
    assert_eq!(texts(events).await?, vec!["先核对。"]);
    Ok(())
}

fn execution_event(
    stream: &str,
    turn: &str,
    execution: &str,
    sequence: u64,
    payload: AgentEvent,
) -> echo_agent::error::Result<ChatDriverEvent> {
    let identity = EventIdentity::new(stream, turn)?.with_execution_id(execution)?;
    Ok(ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
        &identity, sequence, None, payload,
    )?)))
}

#[tokio::test]
async fn native_unknown_scope_keeps_same_stream_different_turn_or_execution_isolated()
-> anyhow::Result<()> {
    for same_turn in [false, true] {
        let mut classification = block(AssistantPhase::Commentary, "", 2, true);
        if let AgentEvent::AssistantMessage { block } = &mut classification {
            block.block_id = "block-1".into();
        }
        let output = texts(vec![
            render(execution_event(
                "shared-stream",
                "turn-a",
                "execution-a",
                1,
                block(AssistantPhase::Unknown, "must-stay-in-a", 1, false),
            )?),
            render(execution_event(
                "shared-stream",
                if same_turn { "turn-a" } else { "turn-b" },
                "execution-b",
                2,
                classification,
            )?),
            Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
        ])
        .await?;
        assert!(output.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn native_finished_execution_releases_unknown_budget_before_continuation()
-> anyhow::Result<()> {
    for failure in [false, true] {
        let mut events = Vec::new();
        for index in 0..5 {
            let execution = format!("execution-{index}");
            let first_sequence = u64::try_from(index)?.saturating_mul(2).saturating_add(1);
            events.push(render(execution_event(
                "same-stream",
                "same-turn",
                &execution,
                first_sequence,
                block(AssistantPhase::Unknown, &"x".repeat(128 * 1024), 1, false),
            )?));
            let terminal = if failure {
                AgentEvent::error_message("test", "execution failed before confirmation")
            } else {
                AgentEvent::FinalAnswer(format!("confirmed-{index}"))
            };
            events.push(render(execution_event(
                "same-stream",
                "same-turn",
                &execution,
                first_sequence.saturating_add(1),
                terminal,
            )?));
        }
        events.push(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)));
        let output = texts(events).await?;
        if failure {
            assert!(output.is_empty());
        } else {
            assert_eq!(
                output,
                (0..5)
                    .map(|index| format!("confirmed-{index}"))
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn native_empty_confirmation_does_not_consume_the_original_execution_slot()
-> anyhow::Result<()> {
    let output = texts(vec![
        render(agent_event(
            1,
            block(AssistantPhase::FinalAnswer, "candidate", 1, true),
        )?),
        render(agent_event(2, AgentEvent::FinalAnswer(String::new()))?),
        render(agent_event(
            3,
            AgentEvent::FinalAnswer("confirmed-original".into()),
        )?),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
    ])
    .await?;
    assert_eq!(output, vec!["confirmed-original"]);
    Ok(())
}

#[tokio::test]
async fn native_second_reserved_answer_is_an_explicit_projection_failure() -> anyhow::Result<()> {
    let mut events = (0..256)
        .map(|index| Ok(ChannelRenderEvent::Prompt(format!("message-{index}"))))
        .collect::<Vec<_>>();
    events.push(render(scoped_event(
        "quota-stream-a",
        "quota-turn-a",
        1,
        block(AssistantPhase::FinalAnswer, "candidate-a", 1, true),
    )?));
    events.push(render(scoped_event(
        "quota-stream-a",
        "quota-turn-a",
        2,
        AgentEvent::FinalAnswer("confirmed-first".into()),
    )?));
    events.push(render(scoped_event(
        "quota-stream-b",
        "quota-turn-b",
        1,
        AgentEvent::FinalAnswer("confirmed-second".into()),
    )?));
    events.push(Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)));
    let mut output = aggregate_by_sentence(
        futures::stream::iter(events).boxed(),
        "feishu".into(),
        "local-test".into(),
        ChatType::Direct,
    )
    .await;
    let mut messages = Vec::new();
    let mut failed = false;
    while let Some(item) = output.next().await {
        match item {
            Ok(message) => messages.push(message.text),
            Err(error) => {
                failed = error.to_string().contains("reserve exhausted");
                break;
            }
        }
    }
    assert!(failed);
    assert_eq!(
        messages
            .iter()
            .filter(|text| text.contains("confirmed-first"))
            .count(),
        1
    );
    assert!(!messages.concat().contains("confirmed-second"));
    assert!(messages.len() <= super::CHANNEL_OUTBOUND_TOTAL_MESSAGES);
    Ok(())
}

#[tokio::test]
async fn native_execution_status_is_visible_but_never_confirms_an_answer_or_closes_foreground()
-> anyhow::Result<()> {
    let status = agent_event(
        1,
        AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
            execution_id: "original-execution".into(),
            request_id: Some("request-original".into()),
            sequence: 1,
            phase: AgentEvent::Cancelled.phase(),
            status: echo_agent::agent::ExecutionStatusKind::Settled,
            reason: None,
            adapter_generated: false,
        }),
    )?;
    let output = texts(vec![
        render(status.clone()),
        render(status),
        render(agent_event(
            2,
            block(AssistantPhase::Commentary, "TaskRun continues", 2, true),
        )?),
        render(agent_event(
            3,
            block(
                AssistantPhase::FinalAnswer,
                "unconfirmed-candidate",
                3,
                true,
            ),
        )?),
        Ok(ChannelRenderEvent::Terminal(TurnOutcome::Cancelled)),
    ])
    .await?;
    assert_eq!(
        output.first().map(String::as_str),
        Some("[execution:original-execution] agent_execution_settled")
    );
    assert_eq!(output.get(1).map(String::as_str), Some("TaskRun continues"));
    assert!(
        output
            .last()
            .is_some_and(|message| message.starts_with("[cancelled]"))
    );
    assert!(!output.concat().contains("unconfirmed-candidate"));
    Ok(())
}

// 只替代外部Agent源；驱动、envelope、sink和渠道发送投影全部使用原生产入口。
#[derive(Clone, Copy, Debug)]
enum OriginalExecutionTail {
    Clean,
    TypedError,
    OrdinaryError,
    Unsettled,
}

struct OriginalTailAgent {
    tail: OriginalExecutionTail,
    confirm_body: bool,
    reached_eof: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl echo_agent::agent::Agent for OriginalTailAgent {
    fn name(&self) -> &str {
        "original-tail-source"
    }
    fn model_name(&self) -> &str {
        "in-memory-script"
    }
    fn system_prompt(&self) -> &str {
        ""
    }
    fn execute_stream_message_with_invocation_context<'a>(
        &'a self,
        _message: echo_agent::llm::types::Message,
        _cancel: CancellationToken,
        _invocation: echo_agent::agent::AgentInvocationContext,
    ) -> futures::future::BoxFuture<
        'a,
        echo_agent::error::Result<
            futures::stream::BoxStream<'a, echo_agent::error::Result<AgentEvent>>,
        >,
    > {
        let tail = self.tail;
        let confirm_body = self.confirm_body;
        let reached_eof = self.reached_eof.clone();
        Box::pin(async move {
            Ok(async_stream::stream! {
                // v03只调用原生typed入口；外部source显式提供自身生命周期，不再借旧适配器补终态。
                let status = |kind| AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
                    adapter_generated: false,
                    execution_id: "original-tail-execution".into(),
                    request_id: None,
                    sequence: if kind == echo_agent::agent::ExecutionStatusKind::Started { 0 } else { 1 },
                    phase: if kind == echo_agent::agent::ExecutionStatusKind::Started {
                        echo_agent::agent::AgentPhase::Thinking
                    } else {
                        echo_agent::agent::AgentPhase::Terminal
                    },
                    status: kind,
                    reason: None,
                });
                yield Ok(status(echo_agent::agent::ExecutionStatusKind::Started));
                yield Ok(block(AssistantPhase::FinalAnswer, "unconfirmed-tail-candidate", 1, true));
                if confirm_body {
                    yield Ok(AgentEvent::FinalAnswer("original-tail-answer".into()));
                }
                match tail {
                    OriginalExecutionTail::Clean => {
                        yield Ok(status(echo_agent::agent::ExecutionStatusKind::Settled));
                    },
                    OriginalExecutionTail::TypedError => {
                        yield Ok(AgentEvent::error_message("original-tail", "original typed cleanup failed"));
                        yield Ok(status(echo_agent::agent::ExecutionStatusKind::Failed));
                    },
                    OriginalExecutionTail::OrdinaryError => {
                        yield Err(echo_agent::error::ReactError::Other("original tail producer failed".into()));
                        yield Ok(status(echo_agent::agent::ExecutionStatusKind::Failed));
                    },
                    OriginalExecutionTail::Unsettled => {
                        yield Err(echo_agent::error::ReactError::ExecutionNotSettled {
                            run_id: "original-tail-run".into(), execution_id: Some("original-tail-execution".into()),
                        });
                    },
                }
                reached_eof.store(true, std::sync::atomic::Ordering::SeqCst);
            }.boxed())
        })
    }
}

struct OriginalTailChannelSink {
    surface: ChannelSurfaceSink,
    seen_final: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    statuses: std::sync::Arc<std::sync::Mutex<Vec<echo_agent::agent::ExecutionStatus>>>,
}

#[async_trait::async_trait]
impl echo_agent::runtime::EventSink for OriginalTailChannelSink {
    async fn on_event(
        &self,
        envelope: EventEnvelope,
    ) -> echo_agent::error::Result<echo_agent::runtime::SinkControl> {
        if matches!(&envelope.payload, AgentEvent::FinalAnswer(_)) {
            self.seen_final
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if let AgentEvent::ExecutionStatus(status) = &envelope.payload {
            self.statuses
                .lock()
                .map_err(|_| {
                    echo_agent::error::ReactError::Other(
                        "test status observation lock poisoned".into(),
                    )
                })?
                .push(status.clone());
        }
        if self
            .surface
            .on_event(ChatDriverEvent::Agent(Box::new(envelope)))
        {
            Ok(echo_agent::runtime::SinkControl::Continue)
        } else {
            Err(echo_agent::error::ReactError::Other(
                "original channel sink rejected an envelope".into(),
            ))
        }
    }
}

async fn original_tail_channel_execution(
    tail: OriginalExecutionTail,
    confirm_body: bool,
) -> anyhow::Result<(
    echo_agent::error::Result<echo_agent::runtime::TurnReceipt>,
    usize,
    bool,
    Vec<String>,
    Vec<echo_agent::agent::ExecutionStatus>,
)> {
    let (tx, rx) = tokio::sync::mpsc::channel(super::CHANNEL_EVENT_QUEUE_CAPACITY);
    let cancel = CancellationToken::new();
    let seen_final = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reached_eof = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let statuses = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = OriginalTailChannelSink {
        surface: ChannelSurfaceSink::new(tx, cancel.clone()),
        seen_final: seen_final.clone(),
        statuses: statuses.clone(),
    };
    let identity = EventIdentity::new("original-tail-stream", "original-tail-turn")?
        .with_conversation_id("original-tail-conversation")?
        .with_run_id("original-tail-run")?
        .with_execution_id("original-tail-execution")?;
    let request = echo_agent::runtime::TurnRequest::new(identity, "verify original physical tail")
        .mode(echo_agent::runtime::TurnMode::Execute)
        .cancel(cancel.clone());
    let result = echo_agent::runtime::AgentTurnDriver
        .drive(
            &OriginalTailAgent {
                tail,
                confirm_body,
                reached_eof: reached_eof.clone(),
            },
            request,
            &sink,
        )
        .await;
    let outcome = match &result {
        Ok(receipt) => Ok(receipt.outcome.clone()),
        Err(echo_agent::error::ReactError::ExecutionNotSettled {
            run_id,
            execution_id,
        }) => Err(crate::chat_driver::ChatTurnError::Observation {
            run_id: run_id.clone(),
            execution_id: execution_id.clone(),
        }),
        Err(error) => Err(crate::chat_driver::ChatTurnError::Error(error.to_string())),
    };
    drop(sink);
    let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
    terminal_tx
        .send(outcome)
        .map_err(|_| anyhow::anyhow!("original terminal observer unavailable"))?;
    let (prompt_tx, prompt_rx) = tokio::sync::broadcast::channel(1);
    drop(prompt_tx);
    let events = super::channel_render_event_stream(
        rx,
        prompt_rx,
        terminal_rx,
        super::ChannelStreamDropGuard(cancel),
    );
    let mut output = aggregate_by_sentence(
        events,
        "feishu".into(),
        "local-test".into(),
        ChatType::Direct,
    )
    .await;
    let mut delivered = Vec::new();
    while let Some(item) = output.next().await {
        match item {
            Ok(message) => delivered.push(message.text),
            Err(_) => break,
        }
    }
    let observed_statuses = statuses
        .lock()
        .map_err(|_| anyhow::anyhow!("test status observation lock poisoned"))?
        .clone();
    Ok((
        result,
        seen_final.load(std::sync::atomic::Ordering::SeqCst),
        reached_eof.load(std::sync::atomic::Ordering::SeqCst),
        delivered,
        observed_statuses,
    ))
}

#[tokio::test]
async fn native_same_execution_tail_failures_never_publish_success_to_channel_sink()
-> anyhow::Result<()> {
    for tail in [
        OriginalExecutionTail::TypedError,
        OriginalExecutionTail::OrdinaryError,
        OriginalExecutionTail::Unsettled,
    ] {
        let (result, seen_final, reached_eof, delivered, _statuses) =
            original_tail_channel_execution(tail, false).await?;
        assert!(
            reached_eof,
            "the original raw producer must reach physical EOF"
        );
        assert_eq!(
            seen_final, 0,
            "a candidate without runtime publication cannot reach the real channel sink"
        );
        assert!(!delivered.concat().contains("original-tail-answer"));
        assert!(!delivered.concat().contains("unconfirmed-tail-candidate"));
        match tail {
            OriginalExecutionTail::Unsettled => assert!(matches!(result,
                Err(echo_agent::error::ReactError::ExecutionNotSettled { run_id, execution_id })
                    if run_id == "original-tail-run" && execution_id.as_deref() == Some("original-tail-execution"))),
            _ => assert!(
                matches!(result, Ok(receipt) if matches!(receipt.outcome, TurnOutcome::Failed(_)))
            ),
        }
    }
    Ok(())
}

#[tokio::test]
async fn native_clean_execution_tail_publishes_one_confirmed_answer_through_original_channel()
-> anyhow::Result<()> {
    let (result, seen_final, reached_eof, delivered, statuses) =
        original_tail_channel_execution(OriginalExecutionTail::Clean, true).await?;
    assert!(reached_eof);
    assert!(matches!(result, Ok(receipt) if receipt.outcome == TurnOutcome::Completed));
    assert_eq!(seen_final, 1);
    assert_eq!(statuses.len(), 2);
    let started = statuses
        .first()
        .ok_or_else(|| anyhow::anyhow!("original Started observation missing"))?;
    let settled = statuses
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("original Settled observation missing"))?;
    assert_eq!(
        started.status,
        echo_agent::agent::ExecutionStatusKind::Started
    );
    assert_eq!(
        settled.status,
        echo_agent::agent::ExecutionStatusKind::Settled
    );
    assert_eq!(started.execution_id, settled.execution_id);
    assert_eq!(
        delivered,
        vec![
            format!("[execution:{}] started", started.execution_id),
            "original-tail-answer".to_string(),
            format!(
                "[execution:{}] agent_execution_settled",
                settled.execution_id
            ),
        ]
    );
    assert!(!delivered.concat().contains("unconfirmed-tail-candidate"));
    Ok(())
}

#[tokio::test]
async fn native_published_body_keeps_original_tail_failure_without_promoting_completion()
-> anyhow::Result<()> {
    for tail in [
        OriginalExecutionTail::TypedError,
        OriginalExecutionTail::OrdinaryError,
        OriginalExecutionTail::Unsettled,
    ] {
        let (result, seen_final, reached_eof, delivered, statuses) =
            original_tail_channel_execution(tail, true).await?;
        assert!(
            reached_eof,
            "the original typed producer must reach physical EOF"
        );
        assert_eq!(
            seen_final, 1,
            "a legal runtime publication remains a single fact"
        );
        assert_eq!(
            delivered
                .iter()
                .filter(|text| text.as_str() == "original-tail-answer")
                .count(),
            1,
            "tail={tail:?}, delivered={delivered:?}, result={result:?}"
        );
        assert!(!delivered.concat().contains("unconfirmed-tail-candidate"));
        assert!(
            !statuses
                .iter()
                .any(|status| status.status == echo_agent::agent::ExecutionStatusKind::Settled)
        );
        match tail {
            OriginalExecutionTail::Unsettled => assert!(matches!(result,
                Err(echo_agent::error::ReactError::ExecutionNotSettled { run_id, execution_id })
                    if run_id == "original-tail-run" && execution_id.as_deref() == Some("original-tail-execution"))),
            _ => {
                let receipt = result?;
                assert!(matches!(receipt.outcome, TurnOutcome::Failed(_)));
                assert!(receipt.final_answer.is_none());
                assert!(receipt.final_message_id.is_none());
            }
        }
    }
    Ok(())
}

/// 原HITL provider与原renderer共享同一个foreground；物理收尾不由展示事件推断。
#[tokio::test]
async fn native_hitl_response_target_and_settled_status_keep_original_foreground()
-> anyhow::Result<()> {
    use crate::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};
    use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
    for channel in ["weixin", "qqbot", "feishu"] {
        let hitl = ChannelHumanLoopProvider::new();
        let prompts = hitl.subscribe_prompts();
        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(4);
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let events = super::channel_render_event_stream(
            driver_rx,
            prompts,
            terminal_rx,
            super::ChannelStreamDropGuard(cancel.clone()),
        );
        let mut output = aggregate_by_sentence(
            events,
            channel.into(),
            "local-test".into(),
            ChatType::Direct,
        )
        .await;
        driver_tx
            .try_send(ChannelRenderEvent::Driver(execution_event(
                "physical-stream",
                "physical-turn",
                "physical-execution",
                1,
                AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
                    execution_id: "physical-execution".into(),
                    request_id: Some("native-request-original".into()),
                    sequence: 1,
                    phase: AgentEvent::Cancelled.phase(),
                    status: echo_agent::agent::ExecutionStatusKind::Settled,
                    reason: None,
                    adapter_generated: false,
                }),
            )?))
            .map_err(|_| anyhow::anyhow!("original renderer receiver closed"))?;
        let settled = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("settled status missing"))?;
        assert!(settled.text.contains("[execution:physical-execution]"));
        assert!(!cancel.is_cancelled());

        let mut request = HumanLoopRequest::input("确认原生任务继续🙂");
        request.request_id = Some("native-request-original".into());
        // 不改现300s上限，也不实际等待；直接poll原future完成真实注册。
        let pending = hitl.request(request);
        futures::pin_mut!(pending);
        assert!(pending.as_mut().now_or_never().is_none());
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("original HITL prompt missing"))?;
        let response_target = prompt
            .text
            .split("Use /reply ")
            .nth(1)
            .and_then(|text| text.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("original response_target missing"))?;
        assert_ne!(response_target, "native-request-original");
        assert!(matches!(
            hitl.try_resolve_message("/reply native-request-original continue"),
            ChannelHumanLoopResolution::Rejected(_)
        ));
        assert!(pending.as_mut().now_or_never().is_none());
        driver_tx
            .try_send(ChannelRenderEvent::Driver(execution_event(
                "physical-stream",
                "physical-turn",
                "physical-execution",
                2,
                block(
                    AssistantPhase::FinalAnswer,
                    "unconfirmed-native-candidate",
                    2,
                    true,
                ),
            )?))
            .map_err(|_| anyhow::anyhow!("original renderer receiver closed"))?;
        assert!(output.next().now_or_never().is_none());
        assert!(matches!(
            hitl.try_resolve_message(&format!("/reply {response_target} continue")),
            ChannelHumanLoopResolution::Resolved(_)
        ));
        assert!(matches!(pending.await?, HumanLoopResponse::Text(text) if text == "continue"));
        driver_tx
            .try_send(ChannelRenderEvent::Driver(execution_event(
                "physical-stream",
                "physical-turn",
                "physical-execution",
                3,
                AgentEvent::FinalAnswer("original runtime confirmed answer🙂".into()),
            )?))
            .map_err(|_| anyhow::anyhow!("original renderer receiver closed"))?;
        let answer = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("confirmed answer missing"))?;
        assert_eq!(answer.text, "original runtime confirmed answer🙂");
        assert!(output.next().now_or_never().is_none());
        assert!(!cancel.is_cancelled());
        terminal_tx
            .send(Ok(TurnOutcome::Completed))
            .map_err(|_| anyhow::anyhow!("original terminal receiver closed"))?;
        assert!(output.next().now_or_never().is_none(), "必须先排空原driver");
        drop(driver_tx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
                .await?
                .is_none()
        );
        assert!(cancel.is_cancelled());
    }
    Ok(())
}

/// Generic final来自原运行时的验证和结算通知，无原生正文块也必须保留。
#[tokio::test]
async fn native_runtime_final_notice_without_blocks_is_deduplicated_for_all_im_channels()
-> anyhow::Result<()> {
    for channel in ["weixin", "qqbot", "feishu"] {
        let final_event = agent_event(
            1,
            AgentEvent::FinalAnswer("original-runtime-notice🙂".to_string()),
        )?;
        let events = futures::stream::iter(vec![
            render(final_event.clone()),
            render(final_event),
            Ok(ChannelRenderEvent::Terminal(TurnOutcome::Completed)),
        ])
        .boxed();
        let output = aggregate_by_sentence(
            events,
            channel.to_string(),
            "local-test".into(),
            ChatType::Direct,
        )
        .await;
        let messages: Vec<_> = output.map_ok(|message| message.text).try_collect().await?;
        assert_eq!(messages, vec!["original-runtime-notice🙂"]);
    }
    Ok(())
}

/// 超时只结算该HITL请求；原foreground与发送观察必须继续由原owner物理收尾。
#[tokio::test]
async fn native_hitl_timeout_and_reused_request_id_keep_original_foreground_owner()
-> anyhow::Result<()> {
    use crate::foreground_turn::{ForegroundTurnControl, ForegroundTurnSurface};
    use crate::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};
    use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
    for channel in ["weixin", "qqbot", "feishu"] {
        let foreground = ForegroundTurnControl::default();
        let conversation = format!("native-timeout-{channel}");
        let lease = foreground.begin(
            ForegroundTurnSurface::Channel,
            &conversation,
            "original-root",
        )?;
        let cancel = lease.cancellation_token();
        let hitl = ChannelHumanLoopProvider::new();
        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(4);
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let events = super::channel_render_event_stream(
            driver_rx,
            hitl.subscribe_prompts(),
            terminal_rx,
            super::ChannelStreamDropGuard(cancel.clone()),
        );
        let mut output = aggregate_by_sentence(
            events,
            channel.to_string(),
            "local-test".into(),
            ChatType::Direct,
        )
        .await;
        let mut expiring = HumanLoopRequest::input("original expiring native request");
        expiring.request_id = Some("same-request-id".into());
        // 只缩短此夹具的请求期限；生产默认300s上限不变，也不主动sleep。
        expiring.timeout = Some(std::time::Duration::from_millis(1));
        // 注册后即允许合法超时；同时驱动原请求与已订阅提示，不要求首次poll必须Pending。
        let (expired, first_prompt) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(1), hitl.request(expiring)),
            tokio::time::timeout(std::time::Duration::from_secs(1), output.next()),
        );
        assert!(matches!(expired??, HumanLoopResponse::Timeout));
        let first_prompt = first_prompt?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("original timeout prompt missing"))?;
        let expired_target = first_prompt
            .text
            .split("Use /reply ")
            .nth(1)
            .and_then(|value| value.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("expired target missing"))?
            .to_string();
        assert!(matches!(
            hitl.try_resolve_message(&format!("/reply {expired_target} stale")),
            ChannelHumanLoopResolution::Rejected(_)
        ));
        assert!(
            foreground
                .snapshot(ForegroundTurnSurface::Channel, &conversation)
                .is_some()
        );
        assert!(!cancel.is_cancelled());
        assert!(
            output.next().now_or_never().is_none(),
            "HITL超时不能制造foreground完成"
        );

        let mut replacement = HumanLoopRequest::input("replacement native request");
        replacement.request_id = Some("same-request-id".into());
        let pending = hitl.request(replacement);
        futures::pin_mut!(pending);
        assert!(pending.as_mut().now_or_never().is_none());
        let second_prompt = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("replacement prompt missing"))?;
        let current_target = second_prompt
            .text
            .split("Use /reply ")
            .nth(1)
            .and_then(|value| value.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("replacement target missing"))?
            .to_string();
        assert_ne!(expired_target, current_target);
        for wrong in [expired_target.as_str(), "same-request-id"] {
            assert!(matches!(
                hitl.try_resolve_message(&format!("/reply {wrong} stale")),
                ChannelHumanLoopResolution::Rejected(_)
            ));
            assert!(pending.as_mut().now_or_never().is_none());
        }
        assert!(matches!(
            hitl.try_resolve_message(&format!("/reply {current_target} current🙂")),
            ChannelHumanLoopResolution::Resolved(_)
        ));
        assert!(matches!(pending.await?, HumanLoopResponse::Text(text) if text == "current🙂"));
        assert!(
            foreground
                .snapshot(ForegroundTurnSurface::Channel, &conversation)
                .is_some()
        );
        assert!(!cancel.is_cancelled());

        driver_tx
            .try_send(ChannelRenderEvent::Driver(execution_event(
                "original-stream",
                "original-root",
                "original-execution",
                1,
                AgentEvent::ExecutionStatus(echo_agent::agent::ExecutionStatus {
                    execution_id: "original-execution".into(),
                    request_id: None,
                    sequence: 1,
                    phase: AgentEvent::Cancelled.phase(),
                    status: echo_agent::agent::ExecutionStatusKind::Cancelled,
                    reason: None,
                    adapter_generated: false,
                }),
            )?))
            .map_err(|_| anyhow::anyhow!("original status observer closed"))?;
        let observation = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("original execution observation missing"))?;
        assert_eq!(
            observation.text,
            "[execution:original-execution] agent_execution_cancelled"
        );
        assert!(
            foreground
                .snapshot(ForegroundTurnSurface::Channel, &conversation)
                .is_some()
        );
        assert!(!cancel.is_cancelled());
        assert!(output.next().now_or_never().is_none());
        lease.settle_after_observers(TurnOutcome::Cancelled).await?;
        assert!(
            foreground
                .snapshot(ForegroundTurnSurface::Channel, &conversation)
                .is_none()
        );
        terminal_tx
            .send(Ok(TurnOutcome::Cancelled))
            .map_err(|_| anyhow::anyhow!("original terminal observer closed"))?;
        assert!(
            output.next().now_or_never().is_none(),
            "原Terminal仍须排空driver"
        );
        drop(driver_tx);
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await?
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("original cancellation receipt missing"))?;
        assert_eq!(terminal.text, "[cancelled] The channel turn was cancelled.");
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
                .await?
                .is_none()
        );
        assert!(cancel.is_cancelled());
    }
    Ok(())
}
