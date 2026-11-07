//! 真实Guard、原FileConversationStore和原ChatEventLog的冷恢复消费者。
//! 初始只挂旧loader行为反例；GREEN再接实际发布事实，不用模型phase推成功。

use super::*;
use echo_agent::agent::{
    AgentInvocationContext, AssistantMessageBlock, AssistantPhase, EventIdentity, ReactAgentBuilder,
};
use echo_agent::guard::{Guard, GuardDirection, GuardResult};
use echo_agent::llm::types::{DeltaMessage, Message};
use echo_agent::memory::{ConversationStore, FileConversationStore, NewConversation};
use echo_agent::runtime::{
    AgentTurnDriver, EventSink, SinkControl, TurnMode, TurnOutcome, TurnRequest,
};
use echo_agent::testing::{MockLlmClient, StreamChunk};
use echo_agent_app_core::api::chat_driver::ChatDriverEvent;
use echo_agent_app_core::api::chat_event_log::{
    ChatEventEnvelope, ChatEventLog, ChatEventRetention,
};
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};

const WORKSPACE: &str = "native-history-workspace";
const CONVERSATION: &str = "native-history-conversation";
const ROOT_TURN: &str = "native-history-root";
const EXECUTION: &str = "native-history-execution";

struct TransformOriginalUnknown(Arc<AtomicUsize>);

impl Guard for TransformOriginalUnknown {
    fn name(&self) -> &str {
        "eko-history-transform-unknown"
    }

    fn check<'a>(
        &'a self,
        content: &'a str,
        direction: GuardDirection,
    ) -> BoxFuture<'a, echo_agent::error::Result<GuardResult>> {
        Box::pin(async move {
            if direction == GuardDirection::Output && content == "SECRET-DRAFT" {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(GuardResult::Transform {
                    content: "已校验的安全正文".to_string(),
                    reasons: vec!["真实测试Guard替换原候选".to_string()],
                })
            } else {
                Ok(GuardResult::Pass)
            }
        })
    }
}

struct OriginalPublicationSink(Arc<ChatEventLog>);

#[async_trait::async_trait]
impl EventSink for OriginalPublicationSink {
    async fn on_event(
        &self,
        event: echo_agent::agent::EventEnvelope,
    ) -> echo_agent::error::Result<SinkControl> {
        self.0
            .append_async(
                WORKSPACE.to_string(),
                Some(CONVERSATION.to_string()),
                ROOT_TURN.to_string(),
                ChatDriverEvent::Agent(Box::new(event)),
            )
            .await
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
        Ok(SinkControl::Continue)
    }
}

#[path = "native_timeline_actual_backend_capture.rs"]
mod native_timeline_actual_backend_capture;

struct ColdNativeFixture {
    _root: tempfile::TempDir,
    stored: StoredMessage,
    publications: Vec<ChatEventEnvelope>,
}

async fn produce_and_reopen(transform: bool) -> anyhow::Result<ColdNativeFixture> {
    produce_and_reopen_with_unknown_blocks(transform, 2).await
}

// 分别构造合法single/multi响应，均由原ReAct形成raw/approved/publications；不能剪multi JSON。
async fn produce_and_reopen_with_unknown_blocks(
    transform: bool,
    block_count: usize,
) -> anyhow::Result<ColdNativeFixture> {
    let root = tempfile::tempdir()?;
    let parts = match block_count {
        1 => vec!["SECRET-DRAFT"],
        2 => vec!["SECRET-", "DRAFT"],
        _ => anyhow::bail!("native Unknown捕获factory只接受1或2块"),
    };
    let raw: Vec<_> = parts
        .into_iter()
        .enumerate()
        .map(|(index, text)| AssistantMessageBlock {
            provider_source: None,
            block_id: format!("unknown-{index}"),
            request_id: "history-request".to_string(),
            sequence: u64::try_from(index).unwrap_or(u64::MAX),
            phase: AssistantPhase::Unknown,
            delta: text.to_string(),
            complete: true,
            provider: Some("history-fixture".to_string()),
            reasoning: None,
        })
        .collect();
    let delta: DeltaMessage = serde_json::from_value(serde_json::json!({
        "role":"assistant", "content":"SECRET-DRAFT", "assistant_blocks":raw.clone(),
    }))?;
    let llm = Arc::new(MockLlmClient::new().with_stream_script(vec![
        StreamChunk::Delta(Box::new(delta)),
        StreamChunk::Terminal {
            finish_reason: Some("stop".to_string()),
            usage: None,
        },
    ]));
    let calls = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(FileConversationStore::new(root.path().join("transcript"))?);
    let log_root = root.path().join("events");
    let log = Arc::new(ChatEventLog::open(
        &log_root,
        ChatEventRetention::default(),
    )?);
    let mut builder = ReactAgentBuilder::new()
        .llm_client(llm)
        .auto_project_rules(false)
        .conversation_id(CONVERSATION)
        .conversation_store(store.clone())
        .state_store(Arc::new(echo_agent::state::FileRuntimeStateStore::new(
            root.path().join("runtime"),
        )?))
        .max_iterations(3);
    if transform {
        builder = builder.guard(Arc::new(TransformOriginalUnknown(calls.clone())));
    }
    let agent = builder.build()?;
    let identity =
        EventIdentity::for_chat(Some(CONVERSATION.to_string()), ROOT_TURN, ROOT_TURN, None)?
            .with_execution_id(EXECUTION)?;
    let invocation = AgentInvocationContext {
        runtime: Some(echo_agent::tools::ExternalRunContext {
            conversation_id: Some(CONVERSATION.to_string()),
            turn_id: Some(ROOT_TURN.to_string()),
            message_id: Some(ROOT_TURN.to_string()),
            execution_id: Some(EXECUTION.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let sink = OriginalPublicationSink(log.clone());
    let receipt = AgentTurnDriver
        .drive(
            &agent,
            TurnRequest::from_message(
                identity,
                Message::user("运行真实Guard转换并保存历史。".to_string()),
            )
            .mode(TurnMode::Chat)
            .invocation(invocation),
            &sink,
        )
        .await?;
    assert!(
        matches!(receipt.outcome, TurnOutcome::Completed),
        "fixture必须先有真实执行成功，不以fixture故障当loader RED"
    );
    if transform {
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    log.append_async(
        WORKSPACE.to_string(),
        Some(CONVERSATION.to_string()),
        ROOT_TURN.to_string(),
        ChatDriverEvent::TurnStatus {
            status: "completed".to_string(),
        },
    )
    .await?;
    drop(sink);
    drop(log);
    drop(agent);
    drop(store);

    let reopened = FileConversationStore::new(root.path().join("transcript"))?;
    let expected = if transform {
        "已校验的安全正文"
    } else {
        "SECRET-DRAFT"
    };
    let stored = reopened
        .get_messages(CONVERSATION)
        .await?
        .into_iter()
        .find(|row| row.role == "assistant" && row.content.as_deref() == Some(expected))
        .ok_or_else(|| anyhow::anyhow!("真正FileConversationStore重开缺最终候选"))?;
    let restored = echo_agent::memory::restore_message(&stored)?;
    assert_eq!(
        restored.assistant_blocks.as_deref(),
        Some(raw.as_slice()),
        "原provider原件必须仍可回放"
    );
    if transform {
        assert!(
            restored
                .approved_assistant_blocks
                .as_ref()
                .is_some_and(Vec::is_empty)
        );
    }
    let reopened_log = ChatEventLog::open(&log_root, ChatEventRetention::default())?;
    let publications = reopened_log
        .replay(WORKSPACE, Some(CONVERSATION), CONVERSATION, 0)?
        .events;
    Ok(ColdNativeFixture {
        _root: root,
        stored,
        publications,
    })
}

#[tokio::test]
async fn guard_transform_unknown_multiple_blocks_cold_loader_does_not_publish_provider_originals()
-> anyhow::Result<()> {
    let fixture = produce_and_reopen(true).await?;
    assert!(
        !fixture.publications.is_empty(),
        "必须真实关闭/重开原journal"
    );
    let projected = project_stored_message_with_publications(
        fixture.stored,
        Some(WORKSPACE),
        Some(CONVERSATION),
        &fixture.publications,
    )?;
    let gui_payload = serde_json::to_string(&projected)?;
    assert!(
        !gui_payload.contains("SECRET-"),
        "旧loader不能把已被Guard转换的raw Unknown灌进GUI"
    );
    assert!(!gui_payload.contains("DRAFT"));
    let actual = projected
        .execution_status
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("冷恢复缺实际 terminal 坐标"))?;
    assert_eq!(
        actual.status,
        echo_agent::agent::ExecutionStatusKind::Settled
    );
    let source = projected
        .final_answer_source
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Guard转换后缺最终呈现来源"))?;
    assert_eq!(
        (
            &source.execution_id,
            Some(source.request_id.as_str()),
            source.sequence
        ),
        (
            &actual.execution_id,
            actual.request_id.as_deref(),
            actual.sequence
        )
    );
    assert!(
        source.blocks.is_empty(),
        "Guard排除候选后不能从raw原块/同文补坐标"
    );
    Ok(())
}

#[tokio::test]
async fn imported_native_history_survives_source_close_and_target_cold_load() -> anyhow::Result<()>
{
    let fixture = produce_and_reopen(false).await?;
    let restored = echo_agent::memory::restore_message(&fixture.stored)?;
    let status = restored
        .execution_status
        .clone()
        .ok_or_else(|| anyhow::anyhow!("source native fixture lacks execution status"))?;
    anyhow::ensure!(
        restored
            .approved_assistant_blocks
            .as_ref()
            .is_some_and(|blocks| !blocks.is_empty()),
        "source native fixture lacks approved publication blocks"
    );
    let actual_settled = fixture
        .publications
        .iter()
        .find_map(|event| match &event.payload {
            ChatDriverEvent::Agent(agent) => match &agent.payload {
                echo_agent::agent::AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Settled =>
                {
                    Some(status.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("原真实publication缺Settled"))?;
    let first = fixture
        .publications
        .iter()
        .find(|event| matches!(event.payload, ChatDriverEvent::Agent(_)))
        .ok_or_else(|| anyhow::anyhow!("source publication Agent event missing"))?;
    let message_sha256 = {
        use sha2::{Digest as _, Sha256};
        hex::encode(Sha256::digest(serde_json::to_vec(&restored)?))
    };
    let import = echo_agent_app_core::api::conversation_projection::NativeHistoryImport {
        source_workspace_id: WORKSPACE.to_string(),
        source_conversation_id: CONVERSATION.to_string(),
        source_root_turn_id: first.root_turn_id.clone(),
        source_turn_id: first.turn_id.clone(),
        source_message_id: first.message_id.clone(),
        execution_id: status.execution_id.clone(),
        message_content_sha256: message_sha256,
        events: fixture.publications.clone(),
    };
    let mut imported = fixture.stored.clone();
    imported.conversation_id = CONVERSATION.to_string();
    let mut metadata: serde_json::Value = serde_json::from_str(
        imported
            .attachments_json
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("source canonical metadata missing"))?,
    )?;
    metadata
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("source canonical metadata is not an object"))?
        .insert(
            "native_history_import".to_string(),
            serde_json::to_value(import)?,
        );
    imported.attachments_json = Some(serde_json::to_string(&metadata)?);

    let target_root = tempfile::tempdir()?;
    let target = FileConversationStore::new(target_root.path().join("transcript"))?;
    target
        .create_conversation(NewConversation {
            conversation_id: CONVERSATION.to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("Imported native history".to_string()),
        })
        .await?;
    target.save_messages(CONVERSATION, &[imported]).await?;
    drop(target);
    // The source fixture is dropped here: target loading must use only its
    // stored import metadata and copied event payloads, never the source log.
    drop(fixture);
    let reopened_target = FileConversationStore::new(target_root.path().join("transcript"))?;
    let target_message = reopened_target
        .get_messages(CONVERSATION)
        .await?
        .into_iter()
        .find(|message| message.role == "assistant")
        .ok_or_else(|| anyhow::anyhow!("target imported message missing"))?;
    let projected = project_stored_message_with_publications(
        target_message,
        Some("target-workspace"),
        Some(CONVERSATION),
        &[],
    )?;
    assert!(projected.final_answer_confirmed);
    assert!(projected.content.is_some());
    assert_eq!(
        projected.execution_status.as_ref(),
        Some(&actual_settled),
        "必须回传原已匹配Settled，不得把durable Candidate改型"
    );
    assert_eq!(
        status.status,
        echo_agent::agent::ExecutionStatusKind::Candidate
    );
    let source = projected
        .final_answer_source
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("冷import缺精确最终呈现来源"))?;
    assert_eq!(
        (
            &source.execution_id,
            Some(source.request_id.as_str()),
            source.sequence
        ),
        (
            &actual_settled.execution_id,
            actual_settled.request_id.as_deref(),
            actual_settled.sequence
        )
    );
    assert_eq!(
        source.blocks.len(),
        2,
        "正常两块Unknown均须精确回传，不能只取末块"
    );
    for selected in &source.blocks {
        let exact = projected
            .assistant_blocks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|view| {
                view.block.request_id == selected.request_id
                    && view.block.block_id == selected.block_id
                    && view.block.sequence == selected.sequence
                    && view.execution_id.as_deref() == Some(source.execution_id.as_str())
                    && view.publication_event_id.as_deref()
                        == Some(selected.publication_event_id.as_str())
            });
        assert!(
            exact.is_some(),
            "最终候选精确坐标必须来自实际批准原publication"
        );
    }
    assert!(
        projected
            .assistant_blocks
            .as_ref()
            .is_some_and(|blocks| !blocks.is_empty())
    );
    Ok(())
}

// 实际生产入口的组合验收；不手造resource meta或Target Run/Goal终态。
// 由EKO同一target在新的F发布读窗运行，本文件本身不授权冻结F读取/Cargo。
mod resource_chain_acceptance {
    use super::*;
    use anyhow::Context as _;
    use base64::Engine as _;
    use echo_agent::llm::types::{ChatCompletionResponse, ContentPart, MessageContent};
    use echo_agent::llm::{ChatChunk, ChatRequest, ChatResponse, LlmClient};
    use echo_agent::tools::artifact::{ToolOutputArtifactConfig, ToolOutputArtifactRef};
    use echo_agent_app_core::api as core;
    use futures::stream::{self, BoxStream};
    use sha2::{Digest as _, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    type Expected = BTreeMap<String, Vec<u8>>;

    // 与原性能runner共用seed/validate/sample，不启动App或增加产品入口。
    mod performance_baseline {
        use super::*;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/native_history_gui_baseline.rs"
        ));
    }

    // 同一资源producer的离线GUI回调捕获；不增加产品入口或执行器。
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tauri/commands/native_timeline_tool_backend_capture.rs"
    ));

    const TOOL: &str = "resource_chain_output";
    const FINAL: &str = "真实执行已完成，原文、上传附件与工具结果保留供后续引用。";

    // 只替换模型，不替换driver、tool调用、spill producer、持久化、Fork或reader。
    // 第二个模型调用必须看到真实tool result，禁止夹具直接预填typed tool资源。
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum NativeFixtureMode {
        Original,
        Signed,
        ProgressSigned,
        MissingPhaseMixed,
        MixedReasoningUnknown,
        Guarded,
        EmptyApproved,
    }
    const MISSING_PHASE_BODY: &str = "先读取原文件与附件，再确认工具返回的完整结果。";
    const PROGRESS_COMMENTARY: &str = "先核对原文和上传附件的来源。";
    const PROGRESS_PARTIAL: &str = "已定位完整资源，接着读取工具结果。";
    fn explicit_progress_blocks() -> Vec<AssistantMessageBlock> {
        [
            (
                "resource-progress-commentary",
                AssistantPhase::Commentary,
                PROGRESS_COMMENTARY,
            ),
            (
                "resource-progress-partial",
                AssistantPhase::PartialAnswer,
                PROGRESS_PARTIAL,
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (id, phase, text))| AssistantMessageBlock {
            provider_source: None,
            block_id: id.into(),
            request_id: "resource-chain-request-1".into(),
            sequence: u64::try_from(index).unwrap_or(u64::MAX),
            phase,
            delta: text.into(),
            complete: true,
            provider: Some("resource-chain-model-fixture".into()),
            reasoning: None,
        })
        .collect()
    }
    fn ordinary_progress(block: &AssistantMessageBlock, has_tool_calls: bool) -> bool {
        (matches!(
            block.phase,
            AssistantPhase::Commentary | AssistantPhase::PartialAnswer
        ) || (has_tool_calls && block.phase == AssistantPhase::Unknown))
            && !block.delta.trim().is_empty()
            && block.reasoning.is_none()
            || (has_tool_calls
                && block.phase == AssistantPhase::Reasoning
                && (block.reasoning.is_some() || !block.delta.trim().is_empty()))
    }
    fn source_raw_progress(message: &Message) -> Vec<AssistantMessageBlock> {
        message
            .assistant_blocks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|block| {
                ordinary_progress(
                    block,
                    message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()),
                )
            })
            .cloned()
            .collect()
    }
    fn source_approved_progress(message: &Message) -> anyhow::Result<Vec<AssistantMessageBlock>> {
        let raw = source_raw_progress(message);
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        let approved = message
            .approved_assistant_blocks
            .as_deref()
            .context("真实producer未保留approved进度，不能手填或raw fallback")?;
        let selected = approved
            .iter()
            .filter(|block| {
                ordinary_progress(
                    block,
                    message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !selected.is_empty(),
            "真实producer的approved进度为空，不能通过暴露raw使正控变绿"
        );
        assert_eq!(
            serde_json::to_value(&selected)?,
            serde_json::to_value(&raw)?,
            "已公开进度须保原块，Unknown仅与真实非空tool_calls共同成立，不能回退raw或reasoning"
        );
        Ok(selected)
    }

    struct ResourceChainModel(AtomicUsize, NativeFixtureMode);
    fn preserved_reasoning() -> Vec<echo_agent::llm::types::ReasoningBlock> {
        use echo_agent::llm::types::ReasoningBlock;
        vec![
            ReasoningBlock::Signed {
                thinking: "原签名推理🙂".into(),
                signature: "native-copy-signature".into(),
            },
            ReasoningBlock::Redacted {
                data: "native-copy-redacted".into(),
            },
            ReasoningBlock::Opaque {
                provider: "responses".into(),
                id: "native-copy-opaque".into(),
                data: "native-copy-encrypted".into(),
                summary: vec!["native-copy-summary".into()],
            },
        ]
    }
    fn tool_adjacent_reasoning_and_unknown() -> Vec<AssistantMessageBlock> {
        let mut blocks = preserved_reasoning()
            .into_iter()
            .enumerate()
            .map(|(index, reasoning)| AssistantMessageBlock {
                provider_source: None,
                block_id: format!("mixed-original-reasoning-{index}"),
                request_id: "resource-chain-request-1".into(),
                sequence: u64::try_from(index).unwrap_or(u64::MAX),
                phase: AssistantPhase::Reasoning,
                delta: String::new(),
                complete: true,
                provider: Some("native-normalized-provider-fixture".into()),
                reasoning: Some(reasoning),
            })
            .collect::<Vec<_>>();
        blocks.push(AssistantMessageBlock {
            provider_source: None,
            block_id: "mixed-original-unknown-body".into(),
            request_id: "resource-chain-request-1".into(),
            sequence: u64::try_from(blocks.len()).unwrap_or(u64::MAX),
            phase: AssistantPhase::Unknown,
            delta: MISSING_PHASE_BODY.into(),
            complete: true,
            provider: Some("native-normalized-provider-fixture".into()),
            reasoning: None,
        });
        blocks
    }

    impl ResourceChainModel {
        fn next(
            &self,
            request: &ChatRequest,
        ) -> echo_agent::error::Result<(Message, DeltaMessage, &'static str)> {
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    let mut message = serde_json::from_value::<Message>(serde_json::json!({
                        "role":"assistant","content":null,"tool_calls":[
                            {"id":"resource-chain-call","type":"function","function":{"name":TOOL,"arguments":"{}"}}
                        ]
                    }))?;
                    if self.1 == NativeFixtureMode::ProgressSigned {
                        message.content = MessageContent::Text(format!(
                            "{PROGRESS_COMMENTARY}{PROGRESS_PARTIAL}"
                        ));
                        message.assistant_blocks = Some(explicit_progress_blocks());
                    }
                    if self.1 == NativeFixtureMode::MissingPhaseMixed {
                        message.content = MessageContent::Text(MISSING_PHASE_BODY.into());
                    }
                    if self.1 == NativeFixtureMode::MixedReasoningUnknown {
                        message.content = MessageContent::Text(MISSING_PHASE_BODY.into());
                        message.reasoning_blocks = Some(preserved_reasoning());
                        message.assistant_blocks = Some(tool_adjacent_reasoning_and_unknown());
                    }
                    // Streaming由后面的chat_stream先发送两个真实progress Delta，再发送这个tool-call Delta。
                    let delta = serde_json::from_value(
                        serde_json::json!({"role":"assistant","tool_calls":[
                            {"index":0,"id":"resource-chain-call","type":"function","function":{"name":TOOL,"arguments":"{}"}}
                        ]}),
                    )?;
                    let delta = if self.1 == NativeFixtureMode::MissingPhaseMixed {
                        serde_json::from_value(
                            serde_json::json!({"role":"assistant", "content":MISSING_PHASE_BODY,"tool_calls":[
                                {"index":0,"id":"resource-chain-call","type":"function","function":{"name":TOOL,"arguments":"{}"}}
                            ]}),
                        )?
                    } else {
                        delta
                    };
                    // MissingPhaseMixed不传assistant_blocks/phase/approved/status；保默认whole-output/Guard。
                    Ok((message, delta, "tool_calls"))
                }
                1 => {
                    if !request
                        .messages
                        .iter()
                        .any(|message| message.role == echo_agent::llm::types::Role::Tool)
                    {
                        return Err(echo_agent::error::ReactError::Other(
                            "真实tool result未进入第二次模型请求".into(),
                        ));
                    }
                    let guarded = matches!(
                        self.1,
                        NativeFixtureMode::Guarded | NativeFixtureMode::EmptyApproved
                    );
                    let text = if guarded { "SECRET-DRAFT" } else { FINAL };
                    let mut blocks = Vec::new();
                    if matches!(
                        self.1,
                        NativeFixtureMode::Signed
                            | NativeFixtureMode::ProgressSigned
                            | NativeFixtureMode::Guarded
                    ) {
                        for (index, reasoning) in preserved_reasoning().into_iter().enumerate() {
                            blocks.push(AssistantMessageBlock {
                                provider_source: None,
                                block_id: format!("resource-chain-reasoning-{index}"),
                                request_id: "resource-chain-request-2".into(),
                                sequence: u64::try_from(index).unwrap_or(u64::MAX),
                                phase: AssistantPhase::Reasoning,
                                delta: String::new(),
                                complete: true,
                                provider: Some("resource-chain-model-fixture".into()),
                                reasoning: Some(reasoning),
                            });
                        }
                    }
                    blocks.push(AssistantMessageBlock {
                        provider_source: None,
                        block_id: "resource-chain-final".into(),
                        request_id: "resource-chain-request-2".into(),
                        sequence: u64::try_from(blocks.len()).unwrap_or(u64::MAX),
                        phase: if guarded {
                            AssistantPhase::Unknown
                        } else {
                            AssistantPhase::FinalAnswer
                        },
                        delta: text.into(),
                        complete: true,
                        provider: Some("resource-chain-model-fixture".into()),
                        reasoning: None,
                    });
                    let mut message = Message::assistant(text.into());
                    message.assistant_blocks = Some(blocks.clone());
                    let delta = serde_json::from_value(serde_json::json!({
                        "role":"assistant", "content":text, "assistant_blocks":blocks,
                    }))?;
                    Ok((message, delta, "stop"))
                }
                _ => Err(echo_agent::error::ReactError::Other(
                    "非预期第三次模型请求，不能降级为空成功".into(),
                )),
            }
        }
    }
    impl LlmClient for ResourceChainModel {
        fn chat(
            &self,
            request: ChatRequest,
        ) -> BoxFuture<'_, echo_agent::error::Result<ChatResponse>> {
            Box::pin(async move {
                let (message, _, reason) = self.next(&request)?;
                Ok(ChatResponse {
                    message,
                    finish_reason: Some(reason.into()),
                    usage: None,
                    raw: ChatCompletionResponse::default(),
                })
            })
        }
        fn chat_stream(
            &self,
            request: ChatRequest,
        ) -> BoxFuture<
            '_,
            echo_agent::error::Result<BoxStream<'static, echo_agent::error::Result<ChatChunk>>>,
        > {
            Box::pin(async move {
                let (_, delta, reason) = self.next(&request)?;
                let mut chunks = Vec::new();
                if self.1 == NativeFixtureMode::ProgressSigned && reason == "tool_calls" {
                    for block in explicit_progress_blocks() {
                        let delta = serde_json::from_value(
                            serde_json::json!({"role":"assistant","content":block.delta,"assistant_blocks":[block]}),
                        )?;
                        chunks.push(Ok(ChatChunk {
                            delta,
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                if self.1 == NativeFixtureMode::MixedReasoningUnknown && reason == "tool_calls" {
                    for block in tool_adjacent_reasoning_and_unknown() {
                        let mut native = serde_json::json!({"role":"assistant", "content":block.delta, "assistant_blocks":[block.clone()]});
                        if let Some(reasoning) = block.reasoning.as_ref() {
                            native
                                .as_object_mut()
                                .ok_or_else(|| {
                                    echo_agent::error::ReactError::Other(
                                        "native fixture must be object".into(),
                                    )
                                })?
                                .insert("reasoning_blocks".into(), serde_json::json!([reasoning]));
                        }
                        let delta = serde_json::from_value(native)?;
                        chunks.push(Ok(ChatChunk {
                            delta,
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                chunks.extend([
                    Ok(ChatChunk {
                        delta,
                        finish_reason: None,
                        usage: None,
                    }),
                    Ok(ChatChunk {
                        delta: DeltaMessage::default(),
                        finish_reason: Some(reason.into()),
                        usage: None,
                    }),
                ]);
                Ok(Box::pin(stream::iter(chunks))
                    as BoxStream<'static, echo_agent::error::Result<ChatChunk>>)
            })
        }
        fn model_name(&self) -> &str {
            "resource-chain-fixture"
        }
    }

    struct PublicationSink {
        log: Arc<ChatEventLog>,
        workspace: String,
        conversation: String,
        root: String,
        surface: Option<Arc<dyn core::chat_driver::ChatSink>>,
    }
    #[async_trait::async_trait]
    impl EventSink for PublicationSink {
        async fn on_event(
            &self,
            event: echo_agent::agent::EventEnvelope,
        ) -> echo_agent::error::Result<SinkControl> {
            if let Some(surface) = self.surface.as_ref() {
                if !surface.on_event(ChatDriverEvent::Agent(Box::new(event))) {
                    return Err(echo_agent::error::ReactError::Other(
                        "原GUI journal/tool projection或捕获consumer拒绝事件".into(),
                    ));
                }
                return Ok(SinkControl::Continue);
            }
            self.log
                .append_async(
                    self.workspace.clone(),
                    Some(self.conversation.clone()),
                    self.root.clone(),
                    ChatDriverEvent::Agent(Box::new(event)),
                )
                .await
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            Ok(SinkControl::Continue)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SourceRows {
        Adapted,
        Untouched,
    }
    struct Seed {
        rows: Vec<StoredMessage>,
        producer_rows: Vec<StoredMessage>,
        producer_rows_sha256: String,
        producer_progress_events: Vec<ChatEventEnvelope>,
        expected: Expected,
        source_files: Vec<PathBuf>,
        image: Vec<u8>,
    }
    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }
    fn resource_refs(rows: &[StoredMessage]) -> anyhow::Result<Vec<ToolOutputArtifactRef>> {
        let mut result = Vec::new();
        for row in rows {
            let message = echo_agent::memory::restore_message(row)?;
            let MessageContent::Parts(parts) = message.content else {
                continue;
            };
            for part in parts {
                let ContentPart::ResourceLink { resource } = part else {
                    continue;
                };
                let Some(meta) = resource.meta else {
                    continue;
                };
                for key in ["eko_input_resource", "echo_tool_output_artifact"] {
                    if let Some(reference) =
                        meta.get(key).and_then(|binding| binding.get("reference"))
                    {
                        result.push(serde_json::from_value(reference.clone())?);
                    }
                }
            }
        }
        Ok(result)
    }
    fn assert_reader(
        rows: &[StoredMessage],
        config: &ToolOutputArtifactConfig,
        expected: &Expected,
        image: &[u8],
    ) -> anyhow::Result<()> {
        let references = resource_refs(rows)?;
        anyhow::ensure!(
            references.len() == 3,
            "必须是真实原文、长上传和真实工具producer三条引用，实际{}",
            references.len()
        );
        let mut seen = BTreeSet::new();
        for reference in references {
            anyhow::ensure!(
                reference.path.starts_with(&config.root_dir),
                "引用未迁入目标reader根"
            );
            let bytes = expected
                .get(&reference.sha256)
                .context("目标引用SHA不属于三份真实原件")?;
            let mut cursor = None;
            let mut read = String::new();
            let mut pages = 0usize;
            loop {
                let page = echo_agent::tools::files::artifact::read_artifact_page(
                    config,
                    &reference,
                    cursor.as_deref(),
                    echo_agent::tools::files::artifact::ArtifactPageLimit::Bytes(257),
                )?;
                read.push_str(&page.content);
                pages = pages.saturating_add(1);
                if page.complete {
                    break;
                }
                cursor = Some(page.next_cursor.context("真实reader缺少next_cursor")?);
                anyhow::ensure!(pages < 20_000, "reader没有完成分页");
            }
            anyhow::ensure!(pages > 1, "未覆盖真实分页");
            assert_eq!(read.as_bytes(), bytes.as_slice());
            assert_eq!(sha(read.as_bytes()), reference.sha256);
            assert_eq!(u64::try_from(read.len())?, reference.artifact_bytes);
            seen.insert(reference.sha256);
        }
        assert_eq!(seen, expected.keys().cloned().collect());
        let mut images = Vec::new();
        for row in rows {
            if let MessageContent::Parts(parts) = echo_agent::memory::restore_message(row)?.content
            {
                for part in parts {
                    if let ContentPart::ImageUrl { image_url } = part {
                        let (_, data) = image_url
                            .url
                            .split_once(',')
                            .context("内联图片缺data URL")?;
                        images.push(base64::engine::general_purpose::STANDARD.decode(data)?);
                    }
                }
            }
        }
        assert_eq!(
            images,
            vec![image.to_vec()],
            "原内联附件字节必须经FileStore/Fork保存"
        );
        Ok(())
    }

    fn assert_untouched_producer_rows(
        rows: &[StoredMessage],
        producer: &[StoredMessage],
        digest: &str,
    ) -> anyhow::Result<()> {
        assert_eq!(
            rows, producer,
            "不得补status、reproject或重写source canonical rows"
        );
        assert_eq!(
            sha(&serde_json::to_vec(rows)?),
            digest,
            "原始StoredMessage逐字摘要变化"
        );
        for (actual, original) in rows.iter().zip(producer) {
            let actual = echo_agent::memory::restore_message(actual)?;
            let original = echo_agent::memory::restore_message(original)?;
            assert_eq!(
                serde_json::to_value(&actual)?,
                serde_json::to_value(&original)?,
                "Raw/approved/phase/signature/Parts任何字段不得改变"
            );
            assert_eq!(
                sha(&serde_json::to_vec(&actual)?),
                sha(&serde_json::to_vec(&original)?)
            );
        }
        Ok(())
    }
    fn describe_original_assistants(
        rows: &[StoredMessage],
        journal_terminal: &echo_agent::agent::ExecutionStatus,
    ) -> anyhow::Result<()> {
        let last = rows
            .iter()
            .rposition(|row| row.role == "assistant")
            .context("真实source无assistant")?;
        let mut earlier_statusless = 0usize;
        for (index, row) in rows.iter().enumerate() {
            if row.role != "assistant" {
                continue;
            }
            let message = echo_agent::memory::restore_message(row)?;
            let calls = message.tool_calls.as_ref().map_or(0, Vec::len);
            if index != last
                && message.execution_status.is_none()
                && (calls > 0
                    || message.assistant_blocks.is_some()
                    || message.approved_assistant_blocks.is_some())
            {
                earlier_statusless = earlier_statusless.saturating_add(1);
            }
            eprintln!(
                "unadapted source row={index} final_row={} tool_calls={calls} raw_blocks={:?} approved_blocks={:?} message_status={:?} canonical_sha={}",
                index == last,
                message.assistant_blocks.as_ref().map(Vec::len),
                message.approved_assistant_blocks.as_ref().map(Vec::len),
                message.execution_status.as_ref().map(|status| (
                    &status.status,
                    &status.execution_id,
                    &status.request_id
                )),
                sha(&serde_json::to_vec(&message)?)
            );
            if let Some(status) = message.execution_status.as_ref().filter(|_| index == last) {
                assert_eq!(
                    status.execution_id, journal_terminal.execution_id,
                    "存在的final状态必须属于真实journal owner"
                );
            }
        }
        anyhow::ensure!(
            earlier_statusless > 0,
            "严格fixture必须实际覆盖较早无status的合法progress/tool-call，不得变成空覆盖"
        );
        eprintln!(
            "unadapted source rows_sha={} journal_terminal_execution={} journal_terminal_request={:?}; journal事实保留在原log，不写回任何Message",
            sha(&serde_json::to_vec(rows)?),
            journal_terminal.execution_id,
            journal_terminal.request_id
        );
        Ok(())
    }
    async fn assert_source_still_original(
        workspace: &core::workspace::Workspace,
        conversation: &str,
        seed: &Seed,
    ) -> anyhow::Result<()> {
        let source = FileConversationStore::new(
            core::workspace::layout::WorkspaceLayout::state_dir(&workspace.root),
        )?;
        let rows = source.get_messages(conversation).await?;
        drop(source);
        assert_untouched_producer_rows(&rows, &seed.producer_rows, &seed.producer_rows_sha256)
    }

    fn assert_original_progress_publications(
        rows: &[StoredMessage],
        publications: &[ChatEventEnvelope],
        workspace: &str,
        conversation: &str,
        root_turn: &str,
        execution: &str,
        mode: NativeFixtureMode,
    ) -> anyhow::Result<Vec<ChatEventEnvelope>> {
        let mut found = None;
        let mut original_events = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if row.role != "assistant" {
                continue;
            }
            let message = echo_agent::memory::restore_message(row)?;
            let blocks = source_raw_progress(&message);
            if blocks.is_empty() {
                continue;
            }
            anyhow::ensure!(
                found.is_none(),
                "进度fixture须明确属于同一原始assistant记录"
            );
            assert!(
                message.execution_status.is_none(),
                "普通进度正控必须实际无status，不补写"
            );
            let missing_phase = mode == NativeFixtureMode::MissingPhaseMixed;
            let adjacent_reasoning = mode == NativeFixtureMode::MixedReasoningUnknown;
            let expected_body = if missing_phase || adjacent_reasoning {
                MISSING_PHASE_BODY.to_string()
            } else {
                format!("{PROGRESS_COMMENTARY}{PROGRESS_PARTIAL}")
            };
            assert_eq!(
                message.content.as_text().as_deref(),
                Some(expected_body.as_str())
            );
            if !adjacent_reasoning {
                assert!(
                    message
                        .reasoning_content
                        .as_deref()
                        .is_none_or(str::is_empty)
                );
            }
            if adjacent_reasoning {
                assert_eq!(
                    serde_json::to_value(&message.reasoning_blocks)?,
                    serde_json::to_value(Some(preserved_reasoning()))?,
                    "typed reasoning原件须在真实source cold保持"
                );
                assert_eq!(
                    message.assistant_blocks.as_deref(),
                    Some(blocks.as_slice()),
                    "mixed完整raw不能被过滤丢失"
                );
                assert_eq!(
                    message.approved_assistant_blocks.as_deref(),
                    Some(blocks.as_slice()),
                    "Reasoning+Unknown均必须获原runtime批准，不能补许可"
                );
                assert_eq!(blocks.len(), 4);
                let reason = blocks
                    .iter()
                    .filter_map(|block| block.reasoning.clone())
                    .collect::<Vec<_>>();
                assert_eq!(
                    serde_json::to_value(reason)?,
                    serde_json::to_value(preserved_reasoning())?
                );
                assert!(
                    blocks
                        .iter()
                        .filter(|block| block.phase == AssistantPhase::Reasoning)
                        .all(|block| block.delta.is_empty()),
                    "空delta仍须保typed reasoning"
                );
                let body = blocks.last().context("mixed Unknown正文缺失")?;
                assert_eq!(body.phase, AssistantPhase::Unknown);
                assert_eq!(body.delta, MISSING_PHASE_BODY);
                assert!(body.reasoning.is_none());
            } else {
                assert!(message.reasoning_blocks.as_ref().is_none_or(Vec::is_empty));
            }
            if missing_phase {
                assert!(
                    message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()),
                    "Unknown普通进度必须属于实际混合tool-call消息"
                );
                assert_eq!(
                    blocks.len(),
                    1,
                    "默认纯text Delta只能产生一次普通正文，不人工拼Unknown块"
                );

                assert_eq!(
                    message.assistant_blocks.as_deref(),
                    Some(blocks.as_slice()),
                    "Missing模式完整raw不能含被过滤的额外块"
                );
                assert_eq!(
                    message.approved_assistant_blocks.as_deref(),
                    Some(blocks.as_slice()),
                    "Missing模式完整approved必须等原Unknown原件，不能只比过滤子集"
                );
                assert_eq!(
                    blocks.first().context("真实Unknown块缺失")?.phase,
                    AssistantPhase::Unknown
                );
                assert_eq!(
                    blocks.first().context("真实Unknown块缺失")?.delta,
                    MISSING_PHASE_BODY
                );
            }
            let expected = if missing_phase {
                blocks.clone()
            } else if adjacent_reasoning {
                tool_adjacent_reasoning_and_unknown()
            } else {
                explicit_progress_blocks()
            };
            assert_eq!(blocks.len(), expected.len());
            let mut previous = None;
            let mut scope = None;
            let mut event_ids = BTreeSet::new();
            for (actual, expected) in blocks.iter().zip(expected.iter()) {
                assert_eq!(
                    (
                        &actual.block_id,
                        &actual.request_id,
                        actual.sequence,
                        &actual.phase,
                        &actual.delta,
                        &actual.reasoning
                    ),
                    (
                        &expected.block_id,
                        &expected.request_id,
                        expected.sequence,
                        &expected.phase,
                        &expected.delta,
                        &expected.reasoning
                    )
                );
                let matching = publications.iter().filter(|event| matches!(&event.payload,
                    ChatDriverEvent::Agent(agent) if matches!(&agent.payload, echo_agent::agent::AgentEvent::AssistantMessage { block } if block == actual)
                )).collect::<Vec<_>>();
                anyhow::ensure!(
                    matching.len() == 1,
                    "原canonical进度必须唯一逐字段匹配publication，不能从跨owner候选中猜一个"
                );
                let event = *matching.first().context("原progress publication缺失")?;
                let ChatDriverEvent::Agent(agent) = &event.payload else {
                    anyhow::bail!("原progress必须是Agent publication");
                };
                assert_eq!(event.workspace_id, workspace);
                assert_eq!(event.conversation_id.as_deref(), Some(conversation));
                assert_eq!(event.root_turn_id, root_turn);
                assert_eq!(event.turn_id, root_turn);
                assert_eq!(event.message_id, root_turn);
                assert_eq!(
                    agent.execution_id.as_ref().map(|id| id.as_str()),
                    Some(execution)
                );
                original_events.push(event.clone());
                anyhow::ensure!(
                    agent
                        .execution_id
                        .as_ref()
                        .is_some_and(|id| !id.as_str().trim().is_empty()),
                    "原进度publication缺execution坐标，不能借final填充"
                );
                anyhow::ensure!(
                    event_ids.insert(event.event_id.clone()),
                    "原进度event_id不能重复使用"
                );
                eprintln!(
                    "source-progress-original-publication {}",
                    serde_json::to_string(event)?
                );
                if let Some(prior) = previous {
                    assert!(event.sequence > prior);
                }
                previous = Some(event.sequence);
                let identity = serde_json::json!([
                    event.workspace_id,
                    event.conversation_id,
                    event.root_turn_id,
                    event.turn_id,
                    event.message_id,
                    agent.execution_id
                ]);
                if let Some(original) = scope.as_ref() {
                    assert_eq!(&identity, original);
                } else {
                    scope = Some(identity);
                }
            }
            let tools = publications.iter().find(|event| matches!(&event.payload, ChatDriverEvent::Agent(agent)
                if matches!(&agent.payload, echo_agent::agent::AgentEvent::ToolCall { call_id, invocation } if call_id == "resource-chain-call" && invocation.name == TOOL))).context("真实tool call缺失")?;
            let result = publications.iter().find(|event| matches!(&event.payload, ChatDriverEvent::Agent(agent)
                if matches!(&agent.payload, echo_agent::agent::AgentEvent::ToolResult { call_id, name, .. } if call_id == "resource-chain-call" && name == TOOL))).context("真实tool result缺失")?;
            let final_event = publications.iter().find(|event| matches!(&event.payload, ChatDriverEvent::Agent(agent)
                if matches!(&agent.payload, echo_agent::agent::AgentEvent::FinalAnswer(text) if text == FINAL))).context("真实final缺失")?;
            assert!(
                previous.context("无原进度序列")? < tools.sequence
                    && tools.sequence < result.sequence
                    && result.sequence < final_event.sequence,
                "必须是原progress→tool调用/结果→real final，不借时间或终态重排"
            );
            eprintln!(
                "original progress row={index} approved={:?} original scope={scope:?} canonical_sha={}",
                message.approved_assistant_blocks.as_ref().map(Vec::len),
                sha(&serde_json::to_vec(&message)?)
            );
            eprintln!(
                "source-progress-original canonical={} matching_event_ids={event_ids:?} scope={scope:?}",
                serde_json::to_string(&message)?
            );
            eprintln!(
                "source-progress-original-stored-row {}",
                serde_json::to_string(row)?
            );
            // 先读取producer实际approved形状；若已发布事件与canonical许可不一致，保原反例退出。
            // 不补approved/status，不为推荐的EKO分流方案重新import source。
            let canonical_sha = sha(&serde_json::to_vec(&message)?);
            let approved = source_approved_progress(&message).with_context(|| format!("source progress许可反例: row={index}, canonical_sha={canonical_sha}, matching_event_ids={event_ids:?}, scope={scope:?}"))?;
            assert_eq!(
                serde_json::to_value(&approved)?,
                serde_json::to_value(&blocks)?
            );
            found = Some(index);
        }
        anyhow::ensure!(
            found.is_some(),
            "缺有正文Commentary/PartialAnswer正控，不能以空tool-call代替"
        );
        Ok(original_events)
    }

    fn assert_missing_phase_no_tools_or_approval_refused(
        rows: &[StoredMessage],
        original_events: &[ChatEventEnvelope],
        workspace: &str,
        conversation: &str,
    ) -> anyhow::Result<()> {
        let source = rows
            .iter()
            .find_map(|row| {
                let message = echo_agent::memory::restore_message(row).ok()?;
                (message.execution_status.is_none() && !source_raw_progress(&message).is_empty())
                    .then_some(message)
            })
            .context("MissingPhaseMixed真实原Unknown消息缺失")?;
        let first = original_events
            .first()
            .context("真实Unknown publication缺失")?;
        let ChatDriverEvent::Agent(agent) = &first.payload else {
            anyhow::bail!("真实Unknown publication非Agent");
        };
        let mut no_tools = source.clone();
        no_tools.tool_calls = None;
        let mut empty_tools = source.clone();
        empty_tools.tool_calls = Some(Vec::new());
        let mut no_approval = source.clone();
        no_approval.approved_assistant_blocks = None;
        let mut empty_approval = source.clone();
        empty_approval.approved_assistant_blocks = Some(Vec::new());
        for (label, changed) in [
            ("Unknown实际pub但tool_calls=None", no_tools),
            ("Unknown实际pub但tool_calls=Someempty", empty_tools),
            ("Unknown实际pub但approved=None", no_approval),
            ("Unknown实际pub但approved=Someempty", empty_approval),
        ] {
            // 只改内存副本；用改变后Message的有效摘要，不能只停在坏hash而冒充资格拒绝。
            let selected = core::conversation_projection::native_progress_publication_events(
                &changed,
                workspace,
                conversation,
                original_events,
            )
            .map_err(anyhow::Error::msg)?;
            assert!(
                selected.is_empty(),
                "{label}: 无资格Unknown不能被当普通进度"
            );
            let source_row = echo_agent::memory::project_message(conversation, &changed)?;
            let source_view = project_stored_message_with_publications(
                source_row,
                Some(workspace),
                Some(conversation),
                original_events,
            )?;
            assert!(
                !source_view.final_answer_confirmed && source_view.content.is_none(),
                "{label}"
            );
            assert!(
                source_view
                    .assistant_blocks
                    .as_ref()
                    .is_none_or(Vec::is_empty),
                "{label}: 不允许raw fallback"
            );
            let import = NativeHistoryImport {
                source_workspace_id: first.workspace_id.clone(),
                source_conversation_id: first
                    .conversation_id
                    .clone()
                    .context("原conversation缺失")?,
                source_root_turn_id: first.root_turn_id.clone(),
                source_turn_id: first.turn_id.clone(),
                source_message_id: first.message_id.clone(),
                execution_id: agent
                    .execution_id
                    .as_ref()
                    .context("原execution缺失")?
                    .to_string(),
                message_content_sha256: sha(&serde_json::to_vec(&changed)?),
                events: original_events.to_vec(),
            };
            assert_native_import_refused(&changed, &import)
                .with_context(|| format!("{label}: 实际import/loader必须拒绝"))?;
        }
        Ok(())
    }

    fn assert_adjacent_reasoning_approval_and_publication_gates(
        rows: &[StoredMessage],
        original_events: &[ChatEventEnvelope],
        workspace: &str,
        conversation: &str,
    ) -> anyhow::Result<()> {
        let source = rows
            .iter()
            .find_map(|row| {
                let message = echo_agent::memory::restore_message(row).ok()?;
                (message.execution_status.is_none()
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty())
                    && message.reasoning_blocks.is_some())
                .then_some(message)
            })
            .context("原相邻reasoning source缺失")?;
        let first = original_events
            .first()
            .context("原reasoning publication缺失")?;
        let ChatDriverEvent::Agent(agent) = &first.payload else {
            anyhow::bail!("原reasoning publication非Agent");
        };
        let import_for = |message: &Message,
                          events: Vec<ChatEventEnvelope>|
         -> anyhow::Result<NativeHistoryImport> {
            Ok(NativeHistoryImport {
                source_workspace_id: first.workspace_id.clone(),
                source_conversation_id: first
                    .conversation_id
                    .clone()
                    .context("原conversation缺失")?,
                source_root_turn_id: first.root_turn_id.clone(),
                source_turn_id: first.turn_id.clone(),
                source_message_id: first.message_id.clone(),
                execution_id: agent
                    .execution_id
                    .as_ref()
                    .context("原execution缺失")?
                    .to_string(),
                message_content_sha256: sha(&serde_json::to_vec(message)?),
                events,
            })
        };
        for empty in [false, true] {
            let mut changed = source.clone();
            changed.approved_assistant_blocks = if empty { Some(Vec::new()) } else { None };
            let selected = core::conversation_projection::native_progress_publication_events(
                &changed,
                workspace,
                conversation,
                original_events,
            )
            .map_err(anyhow::Error::msg)?;
            assert!(selected.is_empty());
            let row = echo_agent::memory::project_message(conversation, &changed)?;
            let view = project_stored_message_with_publications(
                row,
                Some(workspace),
                Some(conversation),
                original_events,
            )?;
            assert!(
                !view.final_answer_confirmed
                    && view.content.is_none()
                    && view.assistant_blocks.as_ref().is_none_or(Vec::is_empty),
                "raw reasoning/Unknown不得补批准许可"
            );
            assert_native_import_refused(
                &changed,
                &import_for(&changed, original_events.to_vec())?,
            )?;
        }
        let missing = original_events.iter().skip(1).cloned().collect::<Vec<_>>();
        assert!(
            core::conversation_projection::native_progress_publication_events(
                &source,
                workspace,
                conversation,
                &missing
            )
            .is_err(),
            "Reasoning原pub缺失不能用typed raw补授权"
        );
        assert_native_import_refused(&source, &import_for(&source, missing)?)?;
        Ok(())
    }

    async fn seed_and_completely_close(
        workspace: &core::workspace::Workspace,
        log_path: &Path,
        conversation: &str,
        mode: NativeFixtureMode,
        source_rows: SourceRows,
    ) -> anyhow::Result<Seed> {
        seed_and_close_with_model_mode(workspace, log_path, conversation, mode, source_rows, true)
            .await
    }

    async fn seed_and_close_with_model_mode(
        workspace: &core::workspace::Workspace,
        log_path: &Path,
        conversation: &str,
        mode: NativeFixtureMode,
        source_rows: SourceRows,
        model_streaming: bool,
    ) -> anyhow::Result<Seed> {
        seed_and_close_with_surface_capture(
            workspace,
            log_path,
            conversation,
            mode,
            source_rows,
            model_streaming,
            None,
        )
        .await
    }

    async fn seed_and_close_with_surface_capture(
        workspace: &core::workspace::Workspace,
        log_path: &Path,
        conversation: &str,
        mode: NativeFixtureMode,
        source_rows: SourceRows,
        model_streaming: bool,
        capture_renderer: Option<Arc<dyn core::chat_driver::ChatSink>>,
    ) -> anyhow::Result<Seed> {
        use core::workspace::layout::WorkspaceLayout;
        let artifacts = WorkspaceLayout::artifacts(&workspace.root);
        let raw = format!("{}\n原文完整尾部🙂", "原始用户中文🙂\n".repeat(6000));
        let upload = format!("{}\n上传完整尾部🙂", "上传数据Ω🙂\n".repeat(6000));
        let output = format!("{}\n工具完整尾部🙂", "工具真实返回数据🙂\n".repeat(6000));
        let staging = workspace.root.join("upload-staging");
        std::fs::create_dir_all(&staging)?;
        let upload_path = staging.join("真实上传.txt");
        std::fs::write(&upload_path, &upload)?;
        // 有效1x1 PNG：不需要图像供应商能力，仍验证原内联bytes完整保存。
        let image = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=")?;
        let image_path = staging.join("像素🙂.png");
        std::fs::write(&image_path, &image)?;
        let attachments = [
            core::attachments::AttachmentRef {
                path: upload_path,
                name: "真实上传.txt".into(),
                mime_type: "text/plain".into(),
                source: core::types::AttachmentSource::Upload,
            },
            core::attachments::AttachmentRef {
                path: image_path,
                name: "像素🙂.png".into(),
                mime_type: "image/png".into(),
                source: core::types::AttachmentSource::Upload,
            },
        ];
        let turn_id = format!("root-{}", uuid::Uuid::new_v4());
        let execution_id = format!("execution-{conversation}");
        let turn =
            core::prepared_turn::PreparedUserTurn::build(core::prepared_turn::UserTurnInput {
                text: &raw,
                attachments: &attachments,
                spill_dir: &artifacts.join("user-input"),
                conversation_id: Some(conversation),
                turn_id: Some(&turn_id),
            })?;
        let input = turn.to_message()?;
        let store = Arc::new(FileConversationStore::new(WorkspaceLayout::state_dir(
            &workspace.root,
        ))?);
        let weak = Arc::downgrade(&store);
        let log = Arc::new(ChatEventLog::open(log_path, ChatEventRetention::default())?);
        let log_weak = Arc::downgrade(&log);
        let model = Arc::new(ResourceChainModel(AtomicUsize::new(0), mode));
        let mut builder = ReactAgentBuilder::new()
            .llm_client(model.clone())
            .model_streaming(model_streaming)
            .auto_project_rules(false)
            .conversation_id(conversation)
            .conversation_store(store.clone())
            .state_store(Arc::new(echo_agent::state::FileRuntimeStateStore::new(
                WorkspaceLayout::sessions(&workspace.root),
            )?))
            .tool_output_artifacts(
                ToolOutputArtifactConfig::new(&artifacts, "conversation_or_30d").threshold_bytes(1),
            )
            .tool(Box::new(
                echo_agent::testing::MockTool::new(TOOL).with_response(&output),
            ))
            .max_iterations(4);
        let guard_calls = Arc::new(AtomicUsize::new(0));
        if matches!(
            mode,
            NativeFixtureMode::Guarded | NativeFixtureMode::EmptyApproved
        ) {
            builder = builder.guard(Arc::new(TransformOriginalUnknown(guard_calls.clone())));
        }
        let agent = builder.build()?;
        let surface = if let Some(renderer) = capture_renderer {
            let tools = Arc::new(core::tool_execution::ToolExecutionRepository::open(
                workspace.root.join("capture-tool-detail"),
            )?);
            tools.register_artifact_config(ToolOutputArtifactConfig::new(
                &artifacts,
                "conversation_or_30d",
            ));
            Some(core::chat_event_log::bind_surface_chat_sink(
                core::chat_event_log::ChatSurface::Gui,
                renderer,
                log.clone(),
                tools,
                workspace.id.to_string(),
                Some(conversation.into()),
                turn_id.clone(),
            ))
        } else {
            None
        };
        let sink = PublicationSink {
            log: log.clone(),
            workspace: workspace.id.to_string(),
            conversation: conversation.into(),
            root: turn_id.clone(),
            surface,
        };
        let identity =
            EventIdentity::for_chat(Some(conversation.into()), &turn_id, &turn_id, None)?
                .with_execution_id(execution_id.clone())?;
        let invocation = AgentInvocationContext {
            runtime: Some(echo_agent::tools::ExternalRunContext {
                conversation_id: Some(conversation.into()),
                turn_id: Some(turn_id.clone()),
                message_id: Some(turn_id.clone()),
                execution_id: Some(execution_id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let receipt = AgentTurnDriver
            .drive(
                &agent,
                TurnRequest::from_message(identity, input)
                    .mode(TurnMode::Chat)
                    .invocation(invocation),
                &sink,
            )
            .await?;
        anyhow::ensure!(
            matches!(receipt.outcome, TurnOutcome::Completed),
            "资源fixture真实driver未完成: {:?}",
            receipt.outcome
        );
        assert_eq!(model.0.load(Ordering::SeqCst), 2);
        if matches!(
            mode,
            NativeFixtureMode::Guarded | NativeFixtureMode::EmptyApproved
        ) {
            assert_eq!(guard_calls.load(Ordering::SeqCst), 1);
        }
        log.append_async(
            workspace.id.to_string(),
            Some(conversation.into()),
            turn_id.clone(),
            ChatDriverEvent::TurnStatus {
                status: "completed".into(),
            },
        )
        .await?;
        let publications = log
            .replay(workspace.id.as_str(), Some(conversation), &turn_id, 0)?
            .events;
        let terminals = publications
            .iter()
            .filter_map(|event| {
                if event.workspace_id != workspace.id.as_str()
                    || event.conversation_id.as_deref() != Some(conversation)
                    || event.root_turn_id != turn_id
                    || event.turn_id != turn_id
                    || event.message_id != turn_id
                {
                    return None;
                }
                let ChatDriverEvent::Agent(agent) = &event.payload else {
                    return None;
                };
                let echo_agent::agent::AgentEvent::ExecutionStatus(status) = &agent.payload else {
                    return None;
                };
                (agent
                    .execution_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == execution_id)
                    && status.execution_id == execution_id
                    && status.status == echo_agent::agent::ExecutionStatusKind::Settled)
                    .then_some((event.sequence, status.clone()))
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            terminals.len() == 1,
            "原真实journal必须给同一execution唯一Settled，不从Message status或同turn文字推断"
        );
        let (settled_sequence, execution_status) =
            terminals.first().context("原journal terminal缺失")?.clone();
        anyhow::ensure!(publications.iter().any(|event| event.sequence > settled_sequence
            && event.workspace_id == workspace.id.as_str() && event.conversation_id.as_deref() == Some(conversation)
            && event.root_turn_id == turn_id && event.turn_id == turn_id && event.message_id == turn_id
            && matches!(&event.payload, ChatDriverEvent::TurnStatus { status } if status == "completed")),
            "真实driver收据Completed后必须已有原产品完成日志");
        let producer_rows = store.get_messages(conversation).await?;
        let producer_rows_sha256 = sha(&serde_json::to_vec(&producer_rows)?);
        drop(receipt);
        drop(sink);
        drop(agent);
        drop(store);
        drop(log);
        drop(model);
        drop(turn);
        anyhow::ensure!(
            weak.upgrade().is_none(),
            "不能把仍有Agent持有的FileStore当cold关闭"
        );
        anyhow::ensure!(log_weak.upgrade().is_none(), "原事件log仍由producer持有");
        let reopened_log = ChatEventLog::open(log_path, ChatEventRetention::default())?;
        let cold_publications = reopened_log
            .replay(workspace.id.as_str(), Some(conversation), &turn_id, 0)?
            .events;
        assert_eq!(
            serde_json::to_value(&cold_publications)?,
            serde_json::to_value(&publications)?,
            "原journal cold后publication身份/时间/序列/原块不得变化"
        );
        drop(reopened_log);
        let reopened = FileConversationStore::new(WorkspaceLayout::state_dir(&workspace.root))?;
        let mut rows = reopened.get_messages(conversation).await?;
        assert_eq!(rows, producer_rows, "物理关闭/重开不得改Frame原始rows");
        assert_eq!(sha(&serde_json::to_vec(&rows)?), producer_rows_sha256);
        let producer_progress_events = if matches!(
            mode,
            NativeFixtureMode::ProgressSigned
                | NativeFixtureMode::MissingPhaseMixed
                | NativeFixtureMode::MixedReasoningUnknown
        ) {
            assert_original_progress_publications(
                &rows,
                &cold_publications,
                workspace.id.as_str(),
                conversation,
                &turn_id,
                &execution_id,
                mode,
            )?
        } else {
            Vec::new()
        };
        if mode == NativeFixtureMode::MissingPhaseMixed {
            assert_missing_phase_no_tools_or_approval_refused(
                &rows,
                &producer_progress_events,
                workspace.id.as_str(),
                conversation,
            )?;
        }
        if mode == NativeFixtureMode::MixedReasoningUnknown {
            assert_adjacent_reasoning_approval_and_publication_gates(
                &rows,
                &producer_progress_events,
                workspace.id.as_str(),
                conversation,
            )?;
        }
        if source_rows == SourceRows::Adapted {
            for row in &mut rows {
                let mut message = echo_agent::memory::restore_message(row)?;
                if message.role == echo_agent::llm::types::Role::Assistant
                    && (message.assistant_blocks.is_some()
                        || message.approved_assistant_blocks.is_some())
                    && message.execution_status.is_none()
                {
                    message.execution_status = Some(execution_status.clone());
                    let mut projected =
                        echo_agent::memory::project_message(conversation, &message)?;
                    projected.id = row.id;
                    projected.created_at = row.created_at.clone();
                    *row = projected;
                }
            }
            let authority = reopened
                .get_projection_authority(conversation)
                .await?
                .context("真实资源链 managed projection authority 缺失")?;
            let mut canonical_rows = rows.clone();
            for row in &mut canonical_rows {
                row.id = None;
                row.conversation_id = conversation.to_string();
            }
            let import = echo_agent::memory::ManagedConversationImport::prepare_for_generation(
                conversation,
                authority.epoch,
                authority.revision,
                conversation,
                canonical_rows,
            )?;
            reopened.import_managed_messages(import).await?;
            rows = reopened.get_messages(conversation).await?;
        } else {
            // 严格路径没有set execution_status、project_message或ManagedConversationImport。
            assert_untouched_producer_rows(&rows, &producer_rows, &producer_rows_sha256)?;
            describe_original_assistants(&rows, &execution_status)?;
        }
        drop(reopened);
        let expected: Expected = [raw, upload, output]
            .into_iter()
            .map(|text| (sha(text.as_bytes()), text.into_bytes()))
            .collect();
        if !matches!(
            mode,
            NativeFixtureMode::Original
                | NativeFixtureMode::MissingPhaseMixed
                | NativeFixtureMode::MixedReasoningUnknown
        ) {
            let final_message = echo_agent::memory::restore_message(
                rows.iter()
                    .rev()
                    .find(|row| row.role == "assistant")
                    .context("原生final缺失")?,
            )?;
            let raw_blocks = final_message
                .assistant_blocks
                .as_deref()
                .context("原生raw blocks丢失")?;
            if matches!(
                mode,
                NativeFixtureMode::Signed
                    | NativeFixtureMode::ProgressSigned
                    | NativeFixtureMode::Guarded
            ) {
                let reasoning = raw_blocks
                    .iter()
                    .filter_map(|block| block.reasoning.clone())
                    .collect::<Vec<_>>();
                assert_eq!(
                    serde_json::to_value(reasoning)?,
                    serde_json::to_value(preserved_reasoning())?,
                    "source冷恢复原signature/opaque必须逐值保真"
                );
                let approved = final_message
                    .approved_assistant_blocks
                    .as_ref()
                    .context("Guarded/Signed缺少approved原件")?;
                if mode == NativeFixtureMode::Guarded {
                    assert_eq!(approved.len(), preserved_reasoning().len());
                    assert!(
                        approved
                            .iter()
                            .all(|block| block.phase == AssistantPhase::Reasoning)
                    );
                } else {
                    assert!(!approved.is_empty());
                }
            } else {
                // 真正的empty-approved夹具只有被Guard替换的Unknown正文，没有合法reasoning需要保留。
                assert_eq!(
                    final_message
                        .approved_assistant_blocks
                        .as_ref()
                        .map(Vec::len),
                    Some(0)
                );
                assert!(raw_blocks.iter().all(|block| block.reasoning.is_none()));
            }
            if matches!(
                mode,
                NativeFixtureMode::Guarded | NativeFixtureMode::EmptyApproved
            ) {
                assert_eq!(
                    final_message.content.as_text().as_deref(),
                    Some("已校验的安全正文")
                );
                assert!(
                    raw_blocks
                        .iter()
                        .any(|block| block.phase == AssistantPhase::Unknown
                            && block.delta == "SECRET-DRAFT")
                );
            }
        }
        let refs = resource_refs(&rows)?;
        anyhow::ensure!(
            refs.len() == 3,
            "F真实tool producer尚未提供generic link，不能以人工fixture代替"
        );
        let source_files = refs
            .iter()
            .map(|reference| reference.path.clone())
            .chain(
                attachments
                    .iter()
                    .filter(|attachment| attachment.path.is_file())
                    .map(|attachment| attachment.path.clone()),
            )
            .collect();
        assert_reader(
            &rows,
            &ToolOutputArtifactConfig::new(&artifacts, "conversation_or_30d"),
            &expected,
            &image,
        )?;
        Ok(Seed {
            rows,
            producer_rows,
            producer_rows_sha256,
            producer_progress_events,
            expected,
            source_files,
            image,
        })
    }

    struct AppFixture {
        app: Arc<core::AppState>,
        runtime: core::runtime::AgentRuntime,
        owner: core::runtime::ApplicationLifecycleOwner,
    }
    impl AppFixture {
        async fn open(
            root: &Path,
            registry: Arc<core::workspace::registry::WorkspaceRegistry>,
            log_path: &Path,
            project: &Path,
        ) -> anyhow::Result<Self> {
            // 必须由runner设置隔离EKO_DATA_DIR；不在共享进程中修改环境变量/用户数据根。
            let isolated = std::env::var_os("EKO_DATA_DIR")
                .context("运行此组合验收必须设置主根内独立EKO_DATA_DIR")?;
            anyhow::ensure!(
                Path::new(&isolated).is_absolute(),
                "EKO_DATA_DIR必须绝对路径"
            );
            let mut config = core::config::EkoConfig::default();
            config.model.default_model_id = Some("fixture:resource-chain".into());
            config.model_providers.insert(
                "fixture".into(),
                core::config::ModelProviderConfig {
                    base_url: Some("http://127.0.0.1:9/v1".into()),
                    requires_api_key: false,
                    ..Default::default()
                },
            );
            config
                .configured_models
                .push(core::config::ConfiguredModel {
                    id: "fixture:resource-chain".into(),
                    provider: "fixture".into(),
                    model: "fixture".into(),
                    context_window: Some(120_000),
                    ..Default::default()
                });
            let browser = core::browser::BrowserRuntime::start(core::browser::BrowserConfig {
                enabled: false,
                extension_enabled: false,
                session_dir: root.join("browser"),
                ..Default::default()
            })
            .await;
            let runtime = core::runtime::AgentRuntime::bootstrap(
                &config,
                core::infra::AgentCreateParams {
                    working_dir: Some(project.into()),
                    workspace_data_root: Some(root.join("global")),
                    browser_runtime: Some(browser),
                    ..Default::default()
                },
                root.join("empty-mcp.json"),
            )
            .await?;
            let mut owner = runtime.lifecycle_owner(tokio_util::sync::CancellationToken::new())?;
            let pool = runtime
                .init_pool(
                    core::agent_pool::PoolConfig {
                        enable_background_agent: false,
                        ..Default::default()
                    },
                    None,
                )
                .await?;
            let mut state = core::AppState::from_shared(
                runtime.agent_handle.clone(),
                Some(runtime.model_consumers.clone()),
                runtime.hitl_dispatcher.clone(),
                None,
                runtime.state_store.clone(),
                config,
                runtime.mcp_config_runtime.clone(),
                runtime.product_data_io.clone(),
            )?
            .with_config_path(root.join("eko.yaml"))
            .with_command_cell_runtime(runtime.command_cell_runtime.clone());
            state.workspace.registry = registry;
            state.workspace.global_execution_root = project.into();
            state.storage.chat_events =
                Arc::new(ChatEventLog::open(log_path, ChatEventRetention::default())?);
            state.set_pool(pool.clone());
            let app = Arc::new(state);
            owner.bind_app_state(app.clone());
            owner.bind_pool(pool);
            if let Some(tasks) = app.tasks.runtime.as_ref() {
                owner.bind_task_runtime(tasks.clone());
            }
            Ok(Self {
                app,
                runtime,
                owner,
            })
        }
        async fn close(self) -> anyhow::Result<()> {
            let Self {
                app,
                runtime,
                owner,
            } = self;
            let weak_app = Arc::downgrade(&app);
            let weak_log = Arc::downgrade(&app.storage.chat_events);
            eprintln!("resource-chain lifecycle settle begin");
            let settle = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                owner
                    .settle(core::runtime::ApplicationLifecycleReason::Shutdown, None)
                    .await
                    .wait_final()
                    .await
            })
            .await;
            eprintln!("resource-chain lifecycle settle end: {settle:?}");
            let receipt = settle.context("资源链 App close 在30秒内未完成物理结算")?;
            anyhow::ensure!(
                receipt.is_clean(),
                "不能把未物理结算的app当cold关闭: {receipt}"
            );
            drop(receipt);
            drop(app);
            drop(runtime);
            anyhow::ensure!(
                weak_app.upgrade().is_none(),
                "app仍被旧owner持有，不能声称cold"
            );
            anyhow::ensure!(
                weak_log.upgrade().is_none(),
                "原log仍被旧owner持有，不能声称cold"
            );
            Ok(())
        }
    }
    #[tokio::test]
    async fn app_fixture_empty_bootstrap_closes_original_owner_and_weak_resources()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let project = code_fixture(root.path())?;
        let registry = Arc::new(core::workspace::registry::WorkspaceRegistry::with_base_dir(
            root.path().join("registry"),
        )?);
        let app = AppFixture::open(
            root.path(),
            registry,
            &root.path().join("empty-events"),
            &project,
        )
        .await?;
        anyhow::ensure!(
            app.app
                .command_cell_runtime
                .as_ref()
                .is_some_and(|service| Arc::ptr_eq(service, &app.runtime.command_cell_runtime)),
            "空载控制必须复用runtime已创建的原command-cell service"
        );
        // 对照只使用原startup/close：不打开workspace control，不Fork/调用模型。
        // 原helper继续检查完整物理receipt与app/log Weak，未加第二deadline。
        app.close().await
    }

    async fn store_weak(
        app: &Arc<core::AppState>,
        workspace: &str,
    ) -> anyhow::Result<std::sync::Weak<dyn ConversationStore>> {
        let control = app.workspace_control_for_scope(workspace).await?;
        let store = control
            .runtime()
            .conversation_store()
            .context("要关闭的store缺失")?;
        Ok(Arc::downgrade(&store))
    }
    fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "fixture git失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }
    fn code_fixture(root: &Path) -> anyhow::Result<PathBuf> {
        let repo = root.join("source-code");
        std::fs::create_dir_all(&repo)?;
        git(&repo, &["init", "--quiet"])?;
        std::fs::write(repo.join("tracked.txt"), "已提交原文🙂")?;
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        git(&repo, &["add", "tracked.txt", ".gitignore"])?;
        git(
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Resource Fixture",
                "-c",
                "user.email=fixture@eko.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        )?;
        std::fs::write(repo.join("tracked.txt"), "未提交原文🙂")?;
        std::fs::write(repo.join("untracked.txt"), "未跟踪🙂")?;
        std::fs::write(repo.join("ignored.txt"), "忽略🙂")?;
        Ok(repo.canonicalize()?)
    }

    async fn fork_request(
        app: &Arc<core::AppState>,
        workspace: &str,
        conversation: &str,
        destination: ReplyForkDestination,
    ) -> anyhow::Result<ReplyForkRequest> {
        fork_request_with_rows(app, workspace, conversation, destination, None).await
    }
    async fn fork_request_with_rows(
        app: &Arc<core::AppState>,
        workspace: &str,
        conversation: &str,
        destination: ReplyForkDestination,
        original_rows: Option<&[StoredMessage]>,
    ) -> anyhow::Result<ReplyForkRequest> {
        let control = app.workspace_control_for_scope(workspace).await?;
        let generation = control.generation();
        let prefix = if let Some(rows) = original_rows {
            // 仅重用原token结构；owned execute仍重读source并完整核验publication。
            // 不让preparation提前挡住对真实owned入口的验收，不跳过生产准入。
            branch_prefix(rows, 0, "reply-fork-prefix", true)?
        } else {
            let (_, prefix, _) =
                reply_fork_prefix(app, control.runtime(), conversation, 0, "reply-fork-prefix")
                    .await?;
            prefix
        };
        let id = uuid::Uuid::new_v4().to_string();
        let branch = format!("eko/fork-{id}");
        let mut worktree = ReplyForkWorktreePreparation {
            available: false,
            reason: None,
            repo_root: None,
            head: None,
            branch: branch.clone(),
            path: None,
            project_location: core::workspace::ProjectLocation::Local {
                path: control.project_root(),
            },
            ssh_target: None,
            source_connection_generation: None,
            draft_connection_id: None,
        };
        if destination == ReplyForkDestination::NewWorktree {
            let (repo, head, path) = crate::tauri::commands::panels::prepare_reply_worktree(
                app,
                workspace,
                &generation,
                branch,
            )
            .await?;
            worktree.available = true;
            worktree.repo_root = Some(repo);
            worktree.head = Some(head);
            worktree.path = Some(path);
        }
        Ok(ReplyForkRequest {
            preparation: ReplyForkPreparation {
                request_id: id,
                workspace_id: workspace.into(),
                workspace_generation: generation,
                source_id: conversation.into(),
                user_turn_index: 0,
                message_count: prefix.len(),
                prefix_sha256: reply_fork_prefix_digest(&prefix)?,
                worktree,
            },
            destination,
            ssh_connection_id: None,
        })
    }

    async fn target_facts(
        app: &Arc<core::AppState>,
        response: &serde_json::Value,
        original_status: &serde_json::Value,
    ) -> anyhow::Result<(
        core::workspace::Workspace,
        Vec<StoredMessage>,
        ToolOutputArtifactConfig,
    )> {
        anyhow::ensure!(
            response.get("success").and_then(serde_json::Value::as_bool) == Some(true),
            "真实Fork失败: {response}"
        );
        anyhow::ensure!(
            response
                .get("imported")
                .and_then(serde_json::Value::as_bool)
                == Some(true),
            "未走managed import"
        );
        let workspace = response
            .get("workspace_id")
            .and_then(serde_json::Value::as_str)
            .context("缺target workspace")?;
        let conversation = response
            .get("id")
            .and_then(serde_json::Value::as_str)
            .context("缺target conversation")?;
        let control = app.workspace_control_for_scope(workspace).await?;
        let runtime = control.runtime();
        let store = runtime.conversation_store().context("target store缺失")?;
        let rows = store.get_messages(conversation).await?;
        let config = runtime
            .primary_agent()
            .read(|agent| agent.tool_output_artifacts())
            .await
            .context("target实际reader配置缺失")?;
        let tasks = runtime.task_runtime().context("target原TaskRuntime缺失")?;
        assert!(
            tasks.list_runs_for_conversation(conversation)?.is_empty(),
            "Fork不能创建/复制目标Run或Goal"
        );
        if let Some(pool) = runtime.pool() {
            assert!(
                pool.conversation_model_override(conversation)
                    .await
                    .is_none()
            );
        }
        assert!(
            app.storage
                .chat_events
                .replay(workspace, Some(conversation), conversation, 0)?
                .events
                .is_empty(),
            "导入历史不能制造目标执行/Goal终态事件"
        );
        let final_row = rows
            .iter()
            .rev()
            .find(|row| row.role == "assistant")
            .context("target最终历史缺失")?;
        let final_message = echo_agent::memory::restore_message(final_row)?;
        assert_eq!(
            serde_json::to_value(&final_message.execution_status)?,
            *original_status,
            "只保留source原生状态，禁止制造target完成"
        );
        let publication = project_stored_message_with_publications(
            final_row.clone(),
            Some(workspace),
            Some(conversation),
            &[],
        )?;
        assert!(
            publication.final_answer_confirmed,
            "真实Fork必须保留原批准历史，不能降级删除native来源"
        );
        let metadata: serde_json::Value = serde_json::from_str(
            final_row
                .attachments_json
                .as_deref()
                .context("target metadata缺失")?,
        )?;
        assert!(metadata.get("native_history_import").is_some());
        let workspace = app
            .workspace
            .registry
            .inspect(&core::workspace::WorkspaceId::from_raw(workspace.into()))?;
        Ok((workspace, rows, config))
    }

    fn assert_progress_target_view(
        row: &StoredMessage,
        original: &Message,
        progress: &[AssistantMessageBlock],
        import: &NativeHistoryImport,
        target_workspace: &str,
        original_events: &[ChatEventEnvelope],
    ) -> anyhow::Result<()> {
        assert!(original.execution_status.is_none());
        assert_eq!(
            serde_json::to_value(&import.events)?,
            serde_json::to_value(original_events)?,
            "copy/cold/refork必须转移同一原publication，不能重造相同payload的新事件"
        );
        let origin = original_events.first().context("原producer进度证据缺失")?;
        let ChatDriverEvent::Agent(origin_agent) = &origin.payload else {
            anyhow::bail!("原progress证据非Agent");
        };
        assert_eq!(import.source_workspace_id, origin.workspace_id);
        assert_eq!(
            import.source_conversation_id.as_str(),
            origin
                .conversation_id
                .as_deref()
                .context("原conversation缺失")?
        );
        assert_eq!(import.source_root_turn_id, origin.root_turn_id);
        assert_eq!(import.source_turn_id, origin.turn_id);
        assert_eq!(import.source_message_id, origin.message_id);
        assert_eq!(
            Some(import.execution_id.as_str()),
            origin_agent.execution_id.as_ref().map(|id| id.as_str())
        );
        assert_eq!(
            import.events.len(),
            progress.len(),
            "进度provenance只能带原进度publication，不带terminal授权"
        );
        let mut event_ids = BTreeSet::new();
        let mut last_sequence = None;
        for (event, expected) in import.events.iter().zip(progress) {
            anyhow::ensure!(
                event_ids.insert(event.event_id.clone()),
                "原progress event被重复使用"
            );
            let ChatDriverEvent::Agent(agent) = &event.payload else {
                anyhow::bail!("进度import不能带TurnStatus或target终态");
            };
            assert_eq!(
                agent.execution_id.as_ref().map(|id| id.as_str()),
                Some(import.execution_id.as_str())
            );
            assert!(
                matches!(&agent.payload, echo_agent::agent::AgentEvent::AssistantMessage { block } if block == expected),
                "必须为原已发布块逐字段一致，不能借final/terminal"
            );
            if let Some(sequence) = last_sequence {
                assert!(event.sequence > sequence);
            }
            last_sequence = Some(event.sequence);
        }
        let view = project_stored_message_with_publications(
            row.clone(),
            Some(target_workspace),
            Some(&row.conversation_id),
            &[],
        )?;
        assert!(!view.final_answer_confirmed, "合法进度不变final");
        assert_eq!(view.role, "assistant");
        assert_eq!(
            serde_json::to_value(&view.tool_calls)?,
            serde_json::to_value(&original.tool_calls)?,
            "实际GUI DTO必须继续绑定原tool-call，不能为显示progress丢掉或换掉工具身份"
        );
        let displayed = view
            .assistant_blocks
            .as_deref()
            .context("target已批准进度正文消失")?;
        assert_eq!(displayed.len(), progress.len());
        for (actual, expected) in displayed.iter().zip(progress) {
            assert_eq!(
                serde_json::to_value(&actual.block)?,
                serde_json::to_value(expected)?
            );
            assert_eq!(actual.content, expected.delta);
            assert!(
                ordinary_progress(
                    &actual.block,
                    original
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty())
                ),
                "目标必须保持原known phase或合法mixed Unknown，不能重分类成reasoning/final"
            );
            if actual.block.phase == AssistantPhase::Reasoning {
                assert!(
                    actual.content.is_empty(),
                    "typed reasoning不能把signature/redacted/opaque data塞普通body"
                );
                assert!(actual.block.reasoning.is_some());
            } else {
                assert!(actual.block.reasoning.is_none());
            }
        }
        assert!(
            view.content.is_none(),
            "正文由普通native进度块呈现，不复制成聚合final"
        );
        assert!(
            serde_json::to_value(&view)?
                .get("thinking_segments")
                .is_none(),
            "普通进度不能进入reasoning区"
        );
        let steps = view
            .execution_steps
            .as_deref()
            .context("普通进度顺序丢失")?;
        let assistant_indices = steps
            .iter()
            .filter(|step| step.step_type == "assistant")
            .filter_map(|step| step.index)
            .collect::<Vec<_>>();
        assert_eq!(assistant_indices, (0..progress.len()).collect::<Vec<_>>());
        Ok(())
    }
    fn assert_progress_rows_order(
        rows: &[StoredMessage],
        original: &[StoredMessage],
    ) -> anyhow::Result<()> {
        let progress = original.iter().enumerate().find_map(|(index, row)| {
            let message = echo_agent::memory::restore_message(row).ok()?;
            (!source_raw_progress(&message).is_empty()).then_some(index)
        });
        if let Some(index) = progress {
            let tool = rows
                .iter()
                .position(|row| row.role == "tool")
                .context("进度后的tool result丢失")?;
            let final_index = rows
                .iter()
                .rposition(|row| row.role == "assistant")
                .context("最后final行丢失")?;
            assert!(
                index < tool && tool < final_index,
                "历史重载/reFork必须保持progress→tools→final的原行顺序"
            );
        }
        Ok(())
    }

    // 生产writer已由run_reply_fork_owned调用；这里不重造import或替写摘要。
    fn assert_actual_import_binding(
        rows: &[StoredMessage],
        original: &[StoredMessage],
        workspace: &str,
        conversation: &str,
        target_workspace: &str,
        original_progress_events: &[ChatEventEnvelope],
    ) -> anyhow::Result<()> {
        assert_eq!(rows.len(), original.len(), "import不能丢失原canonical行");
        assert_progress_rows_order(rows, original)?;
        for (row, source) in rows.iter().zip(original) {
            if row.role != "assistant" {
                continue;
            }
            let message = echo_agent::memory::restore_message(row)?;
            let original = echo_agent::memory::restore_message(source)?;
            assert_eq!(
                serde_json::to_value(&message)?,
                serde_json::to_value(&original)?,
                "复制不得丢失或改写source canonical原生事实"
            );
            let meta: serde_json::Value = serde_json::from_str(
                row.attachments_json
                    .as_deref()
                    .context("target native metadata缺失")?,
            )?;
            let progress = source_approved_progress(&original)?;
            if original.execution_status.is_none() && progress.is_empty() {
                assert!(
                    meta.get("native_history_import").is_none(),
                    "无Message execution身份的较早记录不得冒用最终terminal import"
                );
                let view = project_stored_message_with_publications(
                    row.clone(),
                    Some(target_workspace),
                    Some(&row.conversation_id),
                    &[],
                )?;
                assert!(
                    !view.final_answer_confirmed,
                    "无status的progress/tool-call不授予完成资格"
                );
                if original.assistant_blocks.is_some()
                    || original.approved_assistant_blocks.is_some()
                {
                    assert!(
                        view.content.is_none(),
                        "缺原publication坐标只能保历史，不能raw fallback"
                    );
                    assert!(
                        view.assistant_blocks.as_ref().is_none_or(Vec::is_empty),
                        "真实target scope无publication的raw块不能公开fallback"
                    );
                }
                continue;
            }
            let import: NativeHistoryImport = serde_json::from_value(
                meta.get("native_history_import")
                    .context("生产writer未写import")?
                    .clone(),
            )?;
            verify_native_history_import(&message, &import)?;
            if !progress.is_empty() && original.execution_status.is_none() {
                assert_progress_target_view(
                    row,
                    &original,
                    &progress,
                    &import,
                    target_workspace,
                    original_progress_events,
                )?;
            }
            if original
                .assistant_blocks
                .as_ref()
                .is_some_and(|blocks| blocks.iter().any(|block| block.delta == "SECRET-DRAFT"))
            {
                let view = project_stored_message_with_publications(
                    row.clone(),
                    Some(target_workspace),
                    Some(&row.conversation_id),
                    &[],
                )?;
                assert!(view.final_answer_confirmed);
                assert_eq!(view.content.as_deref(), Some("已校验的安全正文"));
                if original.approved_assistant_blocks.as_ref().map(Vec::len) == Some(0) {
                    assert!(
                        view.assistant_blocks.as_ref().is_none_or(Vec::is_empty),
                        "empty approved不能回退raw blocks"
                    );
                }
                assert!(
                    !serde_json::to_string(&view)?.contains("SECRET-DRAFT"),
                    "import/cold/refork GUI不得泄露原raw Unknown"
                );
            }
            assert_eq!(
                import.message_content_sha256,
                sha(&serde_json::to_vec(&message)?),
                "生产write/verify必须绑定同一完整canonical Message"
            );
            assert_eq!(import.source_workspace_id, workspace);
            assert_eq!(
                import.source_conversation_id, conversation,
                "二次Fork不能把target冒成新的source执行"
            );
            assert!(
                import
                    .events
                    .iter()
                    .all(|event| event.workspace_id == workspace
                        && event.conversation_id.as_deref() == Some(conversation))
            );
            let mut with_annotation = row.clone();
            let mut annotated = meta;
            annotated.as_object_mut().context("metadata非对象")?.insert(
                "display_only_probe".into(),
                serde_json::json!({"nil":null,"text":"不递归进canonical摘要"}),
            );
            with_annotation.attachments_json = Some(serde_json::to_string(&annotated)?);
            let restored = echo_agent::memory::restore_message(&with_annotation)?;
            assert_eq!(
                serde_json::to_value(restored.clone())?,
                serde_json::to_value(message)?
            );
            verify_native_history_import(&restored, &import)?;
        }
        Ok(())
    }

    fn complete_chain(
        destination: ReplyForkDestination,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> {
        // 直接返回同一 future，避免未优化测试逐层 poll 大型 async 转发包装。
        complete_chain_with_source(
            destination,
            NativeFixtureMode::Original,
            SourceRows::Adapted,
            None,
        )
    }
    fn complete_native_chain(
        destination: ReplyForkDestination,
        mode: NativeFixtureMode,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> {
        complete_chain_with_source(destination, mode, SourceRows::Adapted, None)
    }
    fn complete_chain_with_source(
        destination: ReplyForkDestination,
        mode: NativeFixtureMode,
        source_rows: SourceRows,
        unprojected_history_records: Option<u64>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> {
        complete_chain_with_model_mode(
            destination,
            mode,
            source_rows,
            unprojected_history_records,
            true,
        )
    }

    async fn complete_chain_with_model_mode(
        destination: ReplyForkDestination,
        mode: NativeFixtureMode,
        source_rows: SourceRows,
        unprojected_history_records: Option<u64>,
        model_streaming: bool,
    ) -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let repo = code_fixture(root.path())?;
        let registry = Arc::new(core::workspace::registry::WorkspaceRegistry::with_base_dir(
            root.path().join("registry"),
        )?);
        let source = registry.create_at(
            "resource-source",
            core::workspace::WorkspaceKind::General,
            root.path().join("source-data"),
        )?;
        let source = registry.link_project(&source.id, repo.clone())?;
        let source_id = "resource-source-conversation";
        let log_path = root.path().join("events");
        let seed = seed_and_close_with_model_mode(
            &source,
            &log_path,
            source_id,
            mode,
            source_rows,
            model_streaming,
        )
        .await?;
        if let Some(records) = unprojected_history_records {
            prepare_unprojected_cold_history_fixture(
                &log_path,
                source.id.as_str(),
                source_id,
                records,
            )?;
        }
        let original_status = serde_json::to_value(
            &echo_agent::memory::restore_message(
                seed.rows
                    .iter()
                    .rev()
                    .find(|row| row.role == "assistant")
                    .context("source final缺失")?,
            )?
            .execution_status,
        )?;
        if source_rows == SourceRows::Adapted {
            anyhow::ensure!(
                !original_status.is_null(),
                "原适配case必须保留真实Framework状态"
            );
        }
        // Untouched不要求把无status的行改成有status；真实owned入口自行判定final资格。
        if source_rows == SourceRows::Untouched {
            assert_source_still_original(&source, source_id, &seed).await?;
        }
        let app = AppFixture::open(root.path(), registry.clone(), &log_path, &repo).await?;
        let source_run_before = {
            let control = app
                .app
                .workspace_control_for_scope(source.id.as_str())
                .await?;
            let task = control
                .runtime()
                .task_runtime()
                .context("source TaskRuntime缺失")?;
            task.create_run(
                "source-goal-do-not-copy",
                source.id.as_str(),
                source_id,
                "source-control-root",
                core::tasks::task_runtime::DomainProfile::General,
                "仅source拥有的Goal🙂",
                "task",
                core::tasks::task_runtime::AttendedMode::Attended,
            )?;
            let pool = control.runtime().pool().context("source pool缺失")?;
            pool.configure_side_conversation(source_id, Some("fixture:resource-chain"))
                .await?;
            assert_eq!(
                pool.conversation_model_override(source_id).await.as_deref(),
                Some("fixture:resource-chain")
            );
            serde_json::to_value(
                task.get_run("source-goal-do-not-copy")?
                    .context("source goal缺失")?,
            )?
        };
        if source_rows == SourceRows::Untouched {
            assert_source_still_original(&source, source_id, &seed).await?;
        }
        let request = if source_rows == SourceRows::Untouched {
            fork_request_with_rows(
                &app.app,
                source.id.as_str(),
                source_id,
                destination,
                Some(&seed.rows),
            )
            .await?
        } else {
            fork_request(&app.app, source.id.as_str(), source_id, destination).await?
        };
        let fixed_head = request.preparation.worktree.head.clone();
        // 调用GUI同一个owned入口，不调用copy helper，也不手工构造managed import。
        let response = run_reply_fork_owned(
            app.app.clone(),
            source.id.to_string(),
            source_id.into(),
            0,
            request,
        )
        .await;
        if source_rows == SourceRows::Untouched {
            assert_source_still_original(&source, source_id, &seed).await?;
        }
        let response = response
            .context("未适配producer源被owned Fork拒绝；不得补status或重写source使其变绿")?;
        let (target, rows, config) = target_facts(&app.app, &response, &original_status).await?;
        assert_actual_import_binding(
            &rows,
            &seed.rows,
            source.id.as_str(),
            source_id,
            target.id.as_str(),
            &seed.producer_progress_events,
        )?;
        let target_id = response
            .get("id")
            .and_then(serde_json::Value::as_str)
            .context("target id缺失")?
            .to_string();
        assert_reader(&rows, &config, &seed.expected, &seed.image)?;
        let receipts: Vec<core::prepared_turn::ReplyForkResourceReceipt> =
            serde_json::from_value(response.get("resources").context("缺复制回执")?.clone())?;
        assert_eq!(receipts.len(), 3);
        assert!(receipts.iter().all(|item| item.reader_verified));
        let copied_paths: BTreeSet<_> = receipts
            .iter()
            .map(|receipt| receipt.target_path.clone())
            .collect();
        anyhow::ensure!(
            seed.source_files
                .iter()
                .all(|source| !copied_paths.contains(source)),
            "真实Fork须创建新conversation副本"
        );
        if destination == ReplyForkDestination::NewWorktree {
            let path = target
                .local_project_root()
                .context("新worktree不是本机project")?;
            assert_eq!(Some(git(&path, &["rev-parse", "HEAD"])?), fixed_head);
            assert_eq!(
                std::fs::read_to_string(path.join("tracked.txt"))?,
                "已提交原文🙂"
            );
            assert!(!path.join("untracked.txt").exists());
            assert!(!path.join("ignored.txt").exists());
            assert_ne!(target.id, source.id);
        } else {
            assert_eq!(target.id, source.id);
        }
        assert_eq!(
            std::fs::read_to_string(repo.join("tracked.txt"))?,
            "未提交原文🙂"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("untracked.txt"))?,
            "未跟踪🙂"
        );
        assert_eq!(std::fs::read_to_string(repo.join("ignored.txt"))?, "忽略🙂");
        {
            let control = app
                .app
                .workspace_control_for_scope(source.id.as_str())
                .await?;
            let task = control
                .runtime()
                .task_runtime()
                .context("source TaskRuntime缺失")?;
            assert_eq!(
                serde_json::to_value(
                    task.get_run("source-goal-do-not-copy")?
                        .context("source goal被删除")?
                )?,
                source_run_before
            );
            assert_eq!(
                control
                    .runtime()
                    .pool()
                    .context("source pool缺失")?
                    .conversation_model_override(source_id)
                    .await
                    .as_deref(),
                Some("fixture:resource-chain")
            );
        }
        let source_read = FileConversationStore::new(
            core::workspace::layout::WorkspaceLayout::state_dir(&source.root),
        )?;
        assert_eq!(
            source_read.get_messages(source_id).await?,
            seed.rows,
            "Fork不得修改source canonical历史"
        );
        drop(source_read);
        // 仅删除原3条已证明source文件，不递归删除同根里的目标副本。
        for file in &seed.source_files {
            std::fs::remove_file(file)?;
        }
        assert_reader(&rows, &config, &seed.expected, &seed.image)?;
        let target_store_weak = store_weak(&app.app, target.id.as_str()).await?;
        app.close().await?;
        anyhow::ensure!(
            target_store_weak.upgrade().is_none(),
            "target FileStore旧holder未释放"
        );
        let reopened = FileConversationStore::new(
            core::workspace::layout::WorkspaceLayout::state_dir(&target.root),
        )?;
        let cold_rows = reopened.get_messages(&target_id).await?;
        drop(reopened);
        assert_reader(&cold_rows, &config, &seed.expected, &seed.image)?;
        assert_actual_import_binding(
            &cold_rows,
            &seed.rows,
            source.id.as_str(),
            source_id,
            target.id.as_str(),
            &seed.producer_progress_events,
        )?;
        let app = AppFixture::open(root.path(), registry, &log_path, &repo).await?;
        {
            let control = app
                .app
                .workspace_control_for_scope(target.id.as_str())
                .await?;
            let pool = control.runtime().pool().context("refork source pool缺失")?;
            pool.configure_side_conversation(&target_id, Some("fixture:resource-chain"))
                .await?;
            assert_eq!(
                pool.conversation_model_override(&target_id)
                    .await
                    .as_deref(),
                Some("fixture:resource-chain")
            );
        }
        let second = fork_request(
            &app.app,
            target.id.as_str(),
            &target_id,
            ReplyForkDestination::CurrentWorkspace,
        )
        .await?;
        let second =
            run_reply_fork_owned(app.app.clone(), target.id.to_string(), target_id, 0, second)
                .await?;
        let (last, last_rows, last_config) =
            target_facts(&app.app, &second, &original_status).await?;
        assert_actual_import_binding(
            &last_rows,
            &seed.rows,
            source.id.as_str(),
            source_id,
            last.id.as_str(),
            &seed.producer_progress_events,
        )?;
        assert_reader(&last_rows, &last_config, &seed.expected, &seed.image)?;
        for file in copied_paths {
            std::fs::remove_file(file)?;
        }
        assert_reader(&last_rows, &last_config, &seed.expected, &seed.image)?;
        let final_id = second
            .get("id")
            .and_then(serde_json::Value::as_str)
            .context("refork id缺失")?
            .to_string();
        let last_store_weak = store_weak(&app.app, last.id.as_str()).await?;
        app.close().await?;
        anyhow::ensure!(
            last_store_weak.upgrade().is_none(),
            "refork FileStore旧holder未释放"
        );
        let final_store = FileConversationStore::new(
            core::workspace::layout::WorkspaceLayout::state_dir(&last.root),
        )?;
        let final_rows = final_store.get_messages(&final_id).await?;
        assert_reader(&final_rows, &last_config, &seed.expected, &seed.image)?;
        assert_actual_import_binding(
            &final_rows,
            &seed.rows,
            source.id.as_str(),
            source_id,
            last.id.as_str(),
            &seed.producer_progress_events,
        )?;
        if source_rows == SourceRows::Untouched {
            assert_source_still_original(&source, source_id, &seed).await?;
        }
        Ok(())
    }

    // 全部反控复用ProgressSigned真实执行、冷FileStore及原journal；只变异内存副本。
    struct StatuslessPublicationFixture {
        _root: tempfile::TempDir,
        workspace: core::workspace::Workspace,
        conversation: &'static str,
        log_path: PathBuf,
        seed: Seed,
        row: StoredMessage,
        message: Message,
        journal: Vec<ChatEventEnvelope>,
    }

    impl StatuslessPublicationFixture {
        async fn real_source() -> anyhow::Result<Self> {
            Self::real_source_with_root(tempfile::tempdir()?).await
        }

        async fn real_source_with_root(root: tempfile::TempDir) -> anyhow::Result<Self> {
            let registry = core::workspace::registry::WorkspaceRegistry::with_base_dir(
                root.path().join("registry"),
            )?;
            let workspace = registry.create_at(
                "statusless-boundary-source",
                core::workspace::WorkspaceKind::General,
                root.path().join("source-data"),
            )?;
            let conversation = "statusless-boundary-conversation";
            let log_path = root.path().join("events");
            let seed = seed_and_completely_close(
                &workspace,
                &log_path,
                conversation,
                NativeFixtureMode::ProgressSigned,
                SourceRows::Untouched,
            )
            .await?;
            let mut found = None;
            for row in &seed.rows {
                if row.role != "assistant" {
                    continue;
                }
                let message = echo_agent::memory::restore_message(row)?;
                if message.execution_status.is_none() && !source_raw_progress(&message).is_empty() {
                    anyhow::ensure!(found.is_none(), "正控须为唯一原始进度行");
                    assert_eq!(source_approved_progress(&message)?.len(), 2);
                    found = Some((row.clone(), message));
                }
            }
            let (row, message) = found.context("真实source缺statusless进度")?;
            assert_eq!(seed.producer_progress_events.len(), 2);
            let log = ChatEventLog::open(&log_path, ChatEventRetention::default())?;
            let journal = log
                .replay(workspace.id.as_str(), Some(conversation), conversation, 0)?
                .events;
            drop(log);
            Ok(Self {
                _root: root,
                workspace,
                conversation,
                log_path,
                seed,
                row,
                message,
                journal,
            })
        }

        fn events(&self) -> Vec<ChatEventEnvelope> {
            self.seed.producer_progress_events.clone()
        }

        fn import(
            &self,
            message: &Message,
            events: Vec<ChatEventEnvelope>,
        ) -> anyhow::Result<NativeHistoryImport> {
            // import身份只取真实正控原件，坏副本不能反向改变验证的expected owner。
            let first = self
                .seed
                .producer_progress_events
                .first()
                .context("原进度envelope缺失")?;
            let ChatDriverEvent::Agent(agent) = &first.payload else {
                anyhow::bail!("真实进度缺原Agent envelope");
            };
            Ok(NativeHistoryImport {
                source_workspace_id: first.workspace_id.clone(),
                source_conversation_id: first
                    .conversation_id
                    .clone()
                    .context("原conversation缺失")?,
                source_root_turn_id: first.root_turn_id.clone(),
                source_turn_id: first.turn_id.clone(),
                source_message_id: first.message_id.clone(),
                execution_id: agent
                    .execution_id
                    .as_ref()
                    .context("原execution缺失")?
                    .to_string(),
                // approved变异也重算其canonical摘要，避免以坏hash冒充publication拒绝。
                message_content_sha256: sha(&serde_json::to_vec(message)?),
                events,
            })
        }

        fn imported_row(
            &self,
            message: &Message,
            events: Vec<ChatEventEnvelope>,
        ) -> anyhow::Result<(StoredMessage, NativeHistoryImport)> {
            let import = self.import(message, events)?;
            let mut row = echo_agent::memory::project_message("statusless-import-target", message)?;
            attach_native_history_import(&mut row, import.clone())?;
            Ok((row, import))
        }

        async fn assert_unchanged(&self) -> anyhow::Result<()> {
            assert_source_still_original(&self.workspace, self.conversation, &self.seed).await?;
            let log = ChatEventLog::open(&self.log_path, ChatEventRetention::default())?;
            let actual = log
                .replay(
                    self.workspace.id.as_str(),
                    Some(self.conversation),
                    self.conversation,
                    0,
                )?
                .events;
            assert_eq!(
                serde_json::to_value(&actual)?,
                serde_json::to_value(&self.journal)?,
                "原progress/tool-call/result/final各stage及全部事件身份/序列/hash不得变异"
            );
            // envelope 内含映射字段；冷重开后的键遍历顺序不能改变同一事实的摘要。
            assert_eq!(
                sha(&echo_agent::utils::canonical_json::canonical_json_bytes(
                    &actual
                )?),
                sha(&echo_agent::utils::canonical_json::canonical_json_bytes(
                    &self.journal
                )?)
            );
            Ok(())
        }
    }

    fn progress_agent_mut(
        events: &mut [ChatEventEnvelope],
        index: usize,
    ) -> anyhow::Result<&mut echo_agent::agent::EventEnvelope> {
        match &mut events.get_mut(index).context("待变异envelope缺失")?.payload {
            ChatDriverEvent::Agent(agent) => Ok(agent.as_mut()),
            _ => anyhow::bail!("待变异项不是原Agent publication"),
        }
    }

    fn assert_statusless_publication_accepted(
        fixture: &StatuslessPublicationFixture,
        events: Vec<ChatEventEnvelope>,
    ) -> anyhow::Result<()> {
        let selected = core::conversation_projection::native_progress_publication_events(
            &fixture.message,
            fixture.workspace.id.as_str(),
            fixture.conversation,
            &events,
        );
        let (target_row, import) = fixture.imported_row(&fixture.message, events.clone())?;
        let verified = verify_native_history_import(&fixture.message, &import);
        let source_view = project_stored_message_with_publications(
            fixture.row.clone(),
            Some(fixture.workspace.id.as_str()),
            Some(fixture.conversation),
            &events,
        );
        let target_view = project_stored_message_with_publications(
            target_row,
            Some("statusless-import-target"),
            Some("statusless-import-target"),
            &[],
        );
        // 三个实际边界先全部调用，单个selector失败不能遮住verify/loader的结果。
        anyhow::ensure!(
            selected.is_ok() && verified.is_ok() && source_view.is_ok() && target_view.is_ok(),
            "合法原进度/幂等重放须成功: selector={selected:?}; verify={verified:?}; source={source_view:?}; target={target_view:?}"
        );
        let selected = selected.map_err(anyhow::Error::msg)?;
        assert_eq!(
            serde_json::to_value(&selected)?,
            serde_json::to_value(&fixture.seed.producer_progress_events)?,
            "只按原sequence恢复同一原envelopes，不能改写身份或重复正文"
        );
        let expected = source_approved_progress(&fixture.message)?;
        for view in [source_view?, target_view?] {
            assert!(!view.final_answer_confirmed);
            assert!(
                view.content.is_none(),
                "进度不得再复制聚合content作为最终正文"
            );
            assert!(
                serde_json::to_value(&view)?
                    .get("thinking_segments")
                    .is_none()
            );
            let actual = view.assistant_blocks.as_deref().context("合法进度丢失")?;
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(
                    serde_json::to_value(&actual.block)?,
                    serde_json::to_value(expected)?
                );
                assert_eq!(actual.content, expected.delta);
            }
        }
        assert!(
            validated_reply_fork_publication_events(
                &fixture.message,
                fixture.workspace.id.as_str(),
                fixture.conversation,
                &fixture.journal,
            )
            .is_err(),
            "合法普通进度仍不授予最终选定回复的Fork资格"
        );
        Ok(())
    }

    fn assert_statusless_publication_rejected(
        fixture: &StatuslessPublicationFixture,
        message: &Message,
        events: Vec<ChatEventEnvelope>,
        label: &str,
    ) -> anyhow::Result<()> {
        let selected = core::conversation_projection::native_progress_publication_events(
            message,
            fixture.workspace.id.as_str(),
            fixture.conversation,
            &events,
        );
        let (target_row, import) = fixture.imported_row(message, events.clone())?;
        let verified = verify_native_history_import(message, &import);
        let target = project_stored_message_with_publications(
            target_row,
            Some("statusless-import-target"),
            Some("statusless-import-target"),
            &[],
        );
        let source_row =
            if serde_json::to_value(message)? == serde_json::to_value(&fixture.message)? {
                fixture.row.clone()
            } else {
                echo_agent::memory::project_message(fixture.conversation, message)?
            };
        let source = project_stored_message_with_publications(
            source_row,
            Some(fixture.workspace.id.as_str()),
            Some(fixture.conversation),
            &events,
        );
        assert!(
            selected.is_err() && verified.is_err() && source.is_err() && target.is_err(),
            "{label}: selector={selected:?}; verify={verified:?}; source={source:?}; target={target:?}; 不能首条胜出或raw fallback"
        );
        Ok(())
    }

    // 只观察同一个真实source在既有默认journal保留/回放之后的历史；不造terminal。
    fn source_retention_loader_baseline(
        fixture: &StatuslessPublicationFixture,
    ) -> anyhow::Result<(StoredMessage, serde_json::Value, serde_json::Value)> {
        let final_row = fixture
            .seed
            .rows
            .iter()
            .rev()
            .find(|row| row.role == "assistant")
            .context("真实source最终行缺失")?
            .clone();
        let final_message = echo_agent::memory::restore_message(&final_row)?;
        assert!(
            matches!(final_message.execution_status.as_ref(), Some(status)
            if status.status == echo_agent::agent::ExecutionStatusKind::Candidate)
        );
        for row in [&fixture.row, &final_row] {
            let metadata: serde_json::Value = serde_json::from_str(
                row.attachments_json
                    .as_deref()
                    .context("原source native metadata缺失")?,
            )?;
            assert!(
                metadata.get("native_history_import").is_none(),
                "普通source不是已Fork/import目标，不能拿复制的pub事实遮住缺口"
            );
        }
        let progress = project_stored_message_with_publications(
            fixture.row.clone(),
            Some(fixture.workspace.id.as_str()),
            Some(fixture.conversation),
            &fixture.journal,
        )?;
        let final_view = project_stored_message_with_publications(
            final_row.clone(),
            Some(fixture.workspace.id.as_str()),
            Some(fixture.conversation),
            &fixture.journal,
        )?;
        assert_eq!(progress.assistant_blocks.as_ref().map(Vec::len), Some(2));
        assert!(!progress.final_answer_confirmed);
        assert!(final_view.final_answer_confirmed && final_view.content.is_some());
        Ok((
            final_row,
            serde_json::to_value(progress)?,
            serde_json::to_value(final_view)?,
        ))
    }

    async fn assert_source_history_after_default_replay(
        fixture: &StatuslessPublicationFixture,
        final_row: StoredMessage,
        baseline_progress: serde_json::Value,
        baseline_final: serde_json::Value,
        label: &str,
    ) -> anyhow::Result<()> {
        // 与get_conversation相同的真实批量consumer接线，不手动绕过05整合hunk。
        let log = Arc::new(ChatEventLog::open(
            &fixture.log_path,
            ChatEventRetention::default(),
        )?);
        let read_cancel = echo_agent::agent::CancellationToken::new();
        let replay = log.replay(
            fixture.workspace.id.as_str(),
            Some(fixture.conversation),
            fixture.conversation,
            0,
        )?;
        // 批量consumer遇到progress缺页会短路；先独立走同consumer读取final行，保两种真实症状。
        let final_only = project_conversation_messages_with_native_history(
            vec![final_row.clone()],
            None,
            &std::collections::HashSet::new(),
            fixture.workspace.id.as_str(),
            fixture.conversation,
            &log,
            &read_cancel,
        )
        .await;
        let loaded = project_conversation_messages_with_native_history(
            vec![fixture.row.clone(), final_row],
            None,
            &std::collections::HashSet::new(),
            fixture.workspace.id.as_str(),
            fixture.conversation,
            &log,
            &read_cancel,
        )
        .await;
        drop(log);
        // 先取两种实际loader结果，且确认canonical源未变，再报告哪个长期结果发生变化。
        assert_source_still_original(&fixture.workspace, fixture.conversation, &fixture.seed)
            .await?;
        eprintln!(
            "{label}: retained_floor={:?} returned_floor={:?} latest={} truncated={} final_only={final_only:?} consumer={loaded:?}",
            replay.retained_earliest_cursor,
            replay.returned_earliest_cursor,
            replay.latest_cursor,
            replay.truncated
        );
        let single_final = final_only?
            .into_iter()
            .next()
            .context("原consumer丢失独立final行")?;
        assert_eq!(
            serde_json::to_value(single_final)?,
            baseline_final,
            "{label}: 独立final确认也须保持，不被progress错误遮住"
        );
        let mut views = loaded
            .map_err(|error| {
                anyhow::anyhow!(
                    "{label}: 原source仍在，完整历史不能因publication窗口/裁剪而拒读: {error}"
                )
            })?
            .into_iter();
        let progress = views.next().context("原consumer丢失progress行")?;
        let final_view = views.next().context("原consumer丢失final行")?;
        assert!(views.next().is_none());
        assert_eq!(
            serde_json::to_value(progress)?,
            baseline_progress,
            "{label}: 同一原source的批准进度和顺序应跨默认保留/回放保持"
        );
        assert_eq!(
            serde_json::to_value(final_view)?,
            baseline_final,
            "{label}: 同一原source最终确认与正文不能随日志窗口消失"
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_source_history_survives_default_replay_cap_without_physical_prune()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let (final_row, baseline_progress, baseline_final) =
            source_retention_loader_baseline(&fixture)?;
        let retention = ChatEventRetention::default();
        let log = ChatEventLog::open(&fixture.log_path, retention)?;
        for _ in 0..retention.max_replay_events {
            // 合法后继过程日志仅Flush，无新terminal/safe-point，不触发物理prune。
            log.append(
                fixture.workspace.id.as_str(),
                Some(fixture.conversation),
                "later-running-root",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )?;
        }
        let replay = log.replay(
            fixture.workspace.id.as_str(),
            Some(fixture.conversation),
            fixture.conversation,
            0,
        )?;
        assert_eq!(
            replay.retained_earliest_cursor,
            Some(1),
            "本反控必须先证明磁盘原前缀未裁"
        );
        assert!(replay.truncated);
        assert_eq!(replay.events.len(), retention.max_replay_events);
        assert!(
            replay
                .events
                .iter()
                .all(|event| event.root_turn_id == "later-running-root")
        );
        drop(log);
        assert_source_history_after_default_replay(
            &fixture,
            final_row,
            baseline_progress,
            baseline_final,
            "默认4096窗口，物理前缀仍保留",
        )
        .await
    }

    #[tokio::test]
    async fn unpinned_native_source_history_survives_default_segment_prune() -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let (final_row, baseline_progress, baseline_final) =
            source_retention_loader_baseline(&fixture)?;
        let retention = ChatEventRetention::default();
        let log = ChatEventLog::open(&fixture.log_path, retention)?;
        let original_last = fixture.journal.last().context("原journal空")?.sequence;
        let identity = EventIdentity::for_chat(
            Some(fixture.conversation.into()),
            "retention-load-root",
            "retention-load-root",
            None,
        )?
        .with_execution_id("retention-load-execution")?;
        // 短于4096条但自然超过8个1MiB段，隔离物理prune而非回放条数限制。
        let bytes_per_event = usize::try_from(retention.segment_rollover_bytes)
            .context("默认segment size无法转换")?
            .checked_div(2)
            .context("默认segment size不可分")?;
        let load = "x".repeat(bytes_per_event);
        let batches = retention
            .max_segments
            .checked_add(3)
            .context("fixture batch overflow")?;
        let mut sequence = 0_u64;
        for _ in 0..batches {
            for _ in 0..3 {
                sequence = sequence.checked_add(1).context("fixture event序号溢出")?;
                let event = echo_agent::agent::EventEnvelope::new(
                    &identity,
                    sequence,
                    None,
                    echo_agent::agent::AgentEvent::AssistantMessage {
                        block: echo_agent::agent::AssistantMessageBlock {
                            provider_source: None,
                            block_id: format!("retention-load-{sequence}"),
                            request_id: "retention-load-request".to_string(),
                            sequence,
                            phase: echo_agent::agent::AssistantPhase::Commentary,
                            delta: load.clone(),
                            complete: false,
                            provider: Some("eko-native-test-fixture".to_string()),
                            reasoning: None,
                        },
                    },
                )?;
                log.append(
                    fixture.workspace.id.as_str(),
                    Some(fixture.conversation),
                    "retention-load-root",
                    ChatDriverEvent::Agent(Box::new(event)),
                )?;
            }
            // 复用既有SyncData配置safe-point触发默认maintain_retention；不是补造完成。
            log.append(
                fixture.workspace.id.as_str(),
                Some(fixture.conversation),
                "retention-load-root",
                ChatDriverEvent::TurnConfiguration {
                    permission_mode: "default".into(),
                    approval_policy: "on-request".into(),
                    attachments: Vec::new(),
                },
            )?;
        }
        let replay = log.replay(
            fixture.workspace.id.as_str(),
            Some(fixture.conversation),
            fixture.conversation,
            0,
        )?;
        assert!(
            replay.latest_cursor
                < u64::try_from(retention.max_replay_events).context("cap conversion")?
        );
        assert!(
            replay
                .retained_earliest_cursor
                .is_some_and(|floor| floor > original_last),
            "反控必须证明原native事实已由默认safe-point实际prune，不直接删events"
        );
        assert!(replay.truncated);
        drop(log);
        assert_source_history_after_default_replay(
            &fixture,
            final_row,
            baseline_progress,
            baseline_final,
            "默认8段保留，未持有其它input/cell pin的原native source",
        )
        .await
    }

    fn prepare_unprojected_cold_history_fixture(
        log_path: &Path,
        workspace: &str,
        conversation: &str,
        target_records: u64,
    ) -> anyhow::Result<()> {
        let log = ChatEventLog::open(log_path, ChatEventRetention::default())?;
        let before = log.replay(workspace, Some(conversation), conversation, 0)?;
        anyhow::ensure!(
            before.latest_cursor < target_records,
            "冷采用负载不小于原source前缀"
        );
        for _ in before.latest_cursor..target_records {
            log.append(
                workspace,
                Some(conversation),
                "later-cold-history-root",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )?;
        }
        let after = log.replay(workspace, Some(conversation), conversation, 0)?;
        assert_eq!(after.latest_cursor, target_records);
        assert_eq!(after.retained_earliest_cursor, Some(1));
        drop(log);
        // 模拟升级前从未生成投影：只移除本fresh fixture的派生目录，绝不删除原records/store。
        let mut removed = 0_u64;
        for entry in std::fs::read_dir(log_path)? {
            let path = entry?.path().join("native-publications");
            if path.is_dir() {
                std::fs::remove_dir_all(path)?;
                removed = removed.saturating_add(1);
            }
        }
        anyhow::ensure!(
            removed > 0,
            "真实producer新投影不存在，fixture模拟升级前缺投影失败"
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_history_gui_first_load_automatically_restores_4097_and_8193_unprojected_records()
    -> anyhow::Result<()> {
        for count in [4097_u64, 8193_u64] {
            let fixture = StatuslessPublicationFixture::real_source().await?;
            source_retention_loader_baseline(&fixture)?;
            let baseline = fixture
                .seed
                .rows
                .iter()
                .cloned()
                .map(|row| {
                    project_stored_message_with_publications(
                        row,
                        Some(fixture.workspace.id.as_str()),
                        Some(fixture.conversation),
                        &fixture.journal,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            prepare_unprojected_cold_history_fixture(
                &fixture.log_path,
                fixture.workspace.id.as_str(),
                fixture.conversation,
                count,
            )?;
            let log = Arc::new(ChatEventLog::open(
                &fixture.log_path,
                ChatEventRetention::default(),
            )?);
            let cancel = echo_agent::agent::CancellationToken::new();
            // 全部canonical行的第一次正常GUI consumer调用；没有final探针预热，也没有用户重试。
            let actual = project_conversation_messages_with_native_history(
                fixture.seed.rows.clone(),
                None,
                &std::collections::HashSet::new(),
                fixture.workspace.id.as_str(),
                fixture.conversation,
                &log,
                &cancel,
            )
            .await?;
            assert_eq!(
                serde_json::to_value(actual)?,
                serde_json::to_value(baseline)?,
                "首个普通冷载请求必须自动完成整个旧前缀并保原全部视图"
            );
            assert_source_still_original(&fixture.workspace, fixture.conversation, &fixture.seed)
                .await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn native_history_fork_first_use_automatically_restores_4097_unprojected_records()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::ProgressSigned,
            SourceRows::Untouched,
            Some(4097),
        )
        .await
    }

    #[tokio::test]
    async fn native_history_fork_first_use_automatically_restores_8193_unprojected_records()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::ProgressSigned,
            SourceRows::Untouched,
            Some(8193),
        )
        .await
    }

    #[tokio::test]
    async fn statusless_progress_original_and_reversed_container_keep_original_order()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        assert_statusless_publication_accepted(&fixture, fixture.events())?;
        let mut reversed = fixture.events();
        reversed.reverse();
        assert_statusless_publication_accepted(&fixture, reversed)?;
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn statusless_progress_identical_full_envelope_replay_is_idempotent() -> anyhow::Result<()>
    {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let mut replay = fixture.events();
        replay.extend(fixture.events());
        // 原完整outer+inner副本是相同事实，不等于一次新的append/publication。
        let result = assert_statusless_publication_accepted(&fixture, replay);
        fixture.assert_unchanged().await?;
        result
    }

    #[tokio::test]
    async fn statusless_progress_conflicting_outer_and_inner_event_ids_are_rejected()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let original = fixture.events();
        let first = original.first().context("缺第一条原进度")?;
        let mut outer = original.clone();
        outer.get_mut(1).context("缺第二条进度")?.event_id = first.event_id.clone();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            outer,
            "同outer ID对应不同原块/hash",
        )?;
        let mut inner = original.clone();
        let first_id = progress_agent_mut(&mut original.clone(), 0)?
            .event_id
            .clone();
        progress_agent_mut(&mut inner, 1)?.event_id = first_id;
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            inner,
            "不同outer复用同inner ID但内容冲突",
        )?;
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn statusless_progress_cross_owner_and_ambiguous_publications_are_rejected()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let original = fixture.events();
        let foreign = EventIdentity::for_chat(
            Some(fixture.conversation.into()),
            "foreign-root",
            "foreign-root",
            None,
        )?
        .with_execution_id("foreign-execution")?
        .with_run_id("foreign-run")?;
        let mut mixed_owner = original.clone();
        progress_agent_mut(&mut mixed_owner, 1)?.execution_id = foreign.execution_id.clone();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            mixed_owner,
            "两个批准块不能来自不同execution owner",
        )?;
        let mut mixed_stream = original.clone();
        progress_agent_mut(&mut mixed_stream, 1)?.stream_id = foreign.stream_id.clone();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            mixed_stream,
            "同execution标签不能拼接不同stream",
        )?;
        let mut mixed_run = original.clone();
        progress_agent_mut(&mut mixed_run, 1)?.run_id = foreign.run_id.clone();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            mixed_run,
            "同stream/execution不能拼接不同run owner",
        )?;
        let mut mixed_message = original.clone();
        let event = mixed_message.get_mut(1).context("缺第二条进度")?;
        event.root_turn_id = "foreign-root".into();
        event.turn_id = "foreign-root".into();
        event.message_id = "foreign-root".into();
        let agent = progress_agent_mut(&mut mixed_message, 1)?;
        agent.turn_id = foreign.turn_id.clone();
        agent.message_id = foreign.message_id.clone();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            mixed_message,
            "内部坐标自洽也不能跨另一原消息范围",
        )?;
        let mut uncorrelated_inner = original.clone();
        progress_agent_mut(&mut uncorrelated_inner, 1)?.message_id = None;
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            uncorrelated_inner,
            "outer/inner消息坐标不一致不能借outer补齐",
        )?;
        let mut missing_owner = original.clone();
        progress_agent_mut(&mut missing_owner, 1)?.execution_id = None;
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            missing_owner,
            "缺原execution不能借首条/最后final补坐标",
        )?;
        let mut ambiguous = original.clone();
        let mut alternative = original.first().context("缺原首项")?.clone();
        let agent = progress_agent_mut(std::slice::from_mut(&mut alternative), 0)?;
        agent.execution_id = foreign.execution_id;
        alternative.event_id.push_str("-foreign-publication");
        ambiguous.push(alternative);
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            ambiguous,
            "相同批准块出现第二owner候选必须拒绝歧义",
        )?;
        // 新outer publication复用同一inner不是原完整envelope重放，不能外推append幂等。
        let mut second_publication = original.clone();
        let mut second = original.first().context("缺原首项")?.clone();
        second.event_id.push_str("-new-outer");
        second.sequence = original
            .last()
            .context("缺原末项")?
            .sequence
            .saturating_add(1);
        second_publication.push(second);
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            second_publication,
            "新outer再次声称同一inner publication",
        )?;
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn statusless_progress_missing_publication_and_changed_approved_are_rejected()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let mut missing = fixture.events();
        missing.pop();
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            missing,
            "批准块缺一条实际publication",
        )?;
        let mut changed = fixture.message.clone();
        changed
            .approved_assistant_blocks
            .as_mut()
            .and_then(|blocks| blocks.first_mut())
            .context("缺原approved块")?
            .delta
            .push_str("未被原publication批准的追加🙂");
        assert_statusless_publication_rejected(
            &fixture,
            &changed,
            fixture.events(),
            "canonical摘要合法但approved字段不匹配原publication",
        )?;
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn statusless_progress_approved_outer_and_inner_sequence_conflicts_are_rejected()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let mut reordered_approval = fixture.message.clone();
        reordered_approval
            .approved_assistant_blocks
            .as_mut()
            .context("缺原approved")?
            .reverse();
        assert_statusless_publication_rejected(
            &fixture,
            &reordered_approval,
            fixture.events(),
            "approved顺序与原outer/inner次序冲突",
        )?;
        let mut changed_block_sequence = fixture.message.clone();
        let approved = changed_block_sequence
            .approved_assistant_blocks
            .as_mut()
            .and_then(|blocks| blocks.first_mut())
            .context("缺原approved序列")?;
        approved.sequence = approved.sequence.saturating_add(100);
        assert_statusless_publication_rejected(
            &fixture,
            &changed_block_sequence,
            fixture.events(),
            "approved块sequence与原publication逐字段不匹配",
        )?;
        let original = fixture.events();
        let mut outer = original.clone();
        let first_sequence = original.first().context("缺首项")?.sequence;
        outer.get_mut(1).context("缺第二项")?.sequence = first_sequence;
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            outer,
            "不同原publication不能占相同outer sequence",
        )?;
        let mut inner = original.clone();
        let first_sequence = progress_agent_mut(&mut original.clone(), 0)?.sequence;
        progress_agent_mut(&mut inner, 1)?.sequence = first_sequence.saturating_sub(1);
        assert_statusless_publication_rejected(
            &fixture,
            &fixture.message,
            inner,
            "outer仍递增但inner倒序不能被容器排序掩盖",
        )?;
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn statusless_progress_none_empty_unknown_and_final_keep_publication_gates()
    -> anyhow::Result<()> {
        let fixture = StatuslessPublicationFixture::real_source().await?;
        let mut none = fixture.message.clone();
        none.approved_assistant_blocks = None;
        let mut empty = fixture.message.clone();
        empty.approved_assistant_blocks = Some(Vec::new());
        let mut unknown = fixture.message.clone();
        let mut block = unknown
            .assistant_blocks
            .as_ref()
            .and_then(|blocks| blocks.first())
            .cloned()
            .context("缺原raw块")?;
        block.phase = AssistantPhase::Unknown;
        unknown.assistant_blocks = Some(vec![block.clone()]);
        unknown.approved_assistant_blocks = Some(vec![block]);
        // 旧Unknown负控只改phase，原pub仍是Commentary；它仍须拒绝，但原因是未有同块publication。
        // 合法approved+pub Unknown+tools另由MissingPhaseMixed真实producer完整链验证，不一概隐藏Unknown。
        assert_statusless_publication_rejected(
            &fixture,
            &unknown,
            fixture.events(),
            "仅改Unknown phase而原publication未匹配",
        )?;
        let mut final_without_status = fixture
            .seed
            .rows
            .iter()
            .rev()
            .filter(|row| row.role == "assistant")
            .map(echo_agent::memory::restore_message)
            .next()
            .context("缺真实最终行")??;
        assert!(final_without_status.execution_status.is_some());
        let original_final = final_without_status.clone();
        final_without_status.execution_status = None;
        let unchanged_clone = serde_json::to_value(&final_without_status)?;
        for (label, message) in [("None approved", none), ("Some(empty)", empty)] {
            let selected = core::conversation_projection::native_progress_publication_events(
                &message,
                fixture.workspace.id.as_str(),
                fixture.conversation,
                &fixture.journal,
            )
            .map_err(anyhow::Error::msg)?;
            assert!(selected.is_empty(), "{label}");
            let row = echo_agent::memory::project_message(fixture.conversation, &message)?;
            let view = project_stored_message_with_publications(
                row,
                Some(fixture.workspace.id.as_str()),
                Some(fixture.conversation),
                &fixture.journal,
            )?;
            assert!(
                !view.final_answer_confirmed && view.content.is_none(),
                "{label}: raw不能回退正文"
            );
            assert!(
                view.assistant_blocks.as_ref().is_none_or(Vec::is_empty),
                "{label}"
            );
            let (row, import) = fixture.imported_row(&message, fixture.events())?;
            assert!(
                verify_native_history_import(&message, &import).is_err(),
                "{label}"
            );
            assert!(
                project_stored_message_with_publications(
                    row,
                    Some("statusless-import-target"),
                    Some("statusless-import-target"),
                    &[],
                )
                .is_err(),
                "{label}: 坏import不能回退raw"
            );
            assert!(
                validated_reply_fork_publication_events(
                    &message,
                    fixture.workspace.id.as_str(),
                    fixture.conversation,
                    &fixture.journal,
                )
                .is_err(),
                "{label}: 不能成为最终选定回复"
            );
        }

        // 去掉终态坐标不会撤销原已批准发布的Reasoning；只撤销最终回复资格。
        // clone中的3块Reasoning和FinalAnswer原件均保留，不用删块或补status迎合断言。
        let expected_reasoning: Vec<_> = original_final
            .approved_assistant_blocks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|block| block.phase == AssistantPhase::Reasoning)
            .cloned()
            .collect();
        assert_eq!(expected_reasoning.len(), 3);
        assert_eq!(
            serde_json::to_value(
                expected_reasoning
                    .iter()
                    .filter_map(|block| block.reasoning.clone())
                    .collect::<Vec<_>>()
            )?,
            serde_json::to_value(preserved_reasoning())?
        );
        assert!(
            final_without_status
                .approved_assistant_blocks
                .as_ref()
                .is_some_and(|blocks| blocks
                    .iter()
                    .any(|block| block.phase == AssistantPhase::FinalAnswer)),
            "clone中的原FinalAnswer也须保留，由选择器拒绝其最终授权"
        );
        let original_status = original_final
            .execution_status
            .as_ref()
            .context("原final状态缺失")?;
        let mut original_reasoning_events = Vec::<ChatEventEnvelope>::new();
        for expected in &expected_reasoning {
            let matching = fixture.journal.iter().filter(|event| {
                event.workspace_id == fixture.workspace.id.as_str()
                    && event.conversation_id.as_deref() == Some(fixture.conversation)
                    && matches!(&event.payload, ChatDriverEvent::Agent(agent)
                        if agent.execution_id.as_ref().is_some_and(|id| id.as_str() == original_status.execution_id)
                            && matches!(&agent.payload, echo_agent::agent::AgentEvent::AssistantMessage { block } if block == expected))
            }).collect::<Vec<_>>();
            anyhow::ensure!(
                matching.len() == 1,
                "expected必须独立精确匹配原Reasoning publication，不用selector产物反推"
            );
            original_reasoning_events
                .push((*matching.first().context("原Reasoning publication缺失")?).clone());
        }
        let selected = core::conversation_projection::native_progress_publication_events(
            &final_without_status,
            fixture.workspace.id.as_str(),
            fixture.conversation,
            &fixture.journal,
        )
        .map_err(anyhow::Error::msg)?;
        assert_eq!(selected.len(), expected_reasoning.len());
        assert_eq!(
            serde_json::to_value(&selected)?,
            serde_json::to_value(&original_reasoning_events)?,
            "完整原envelope必须逐值保持"
        );
        let source_row =
            echo_agent::memory::project_message(fixture.conversation, &final_without_status)?;
        let source_view = project_stored_message_with_publications(
            source_row,
            Some(fixture.workspace.id.as_str()),
            Some(fixture.conversation),
            &fixture.journal,
        )?;
        let (target_row, pure_import) =
            fixture.imported_row(&final_without_status, original_reasoning_events)?;
        verify_native_history_import(&final_without_status, &pure_import)?;
        let target_view = project_stored_message_with_publications(
            target_row,
            Some("statusless-import-target"),
            Some("statusless-import-target"),
            &[],
        )?;
        for view in [source_view, target_view] {
            assert!(!view.final_answer_confirmed && view.content.is_none());
            let blocks = view
                .assistant_blocks
                .as_deref()
                .context("原Reasoning丢失")?;
            assert_eq!(
                serde_json::to_value(blocks.iter().map(|view| &view.block).collect::<Vec<_>>())?,
                serde_json::to_value(&expected_reasoning)?,
                "只呈现原Reasoning，不显示FinalAnswer块或聚合最终正文"
            );
            for (view, expected) in blocks.iter().zip(&expected_reasoning) {
                assert_eq!(view.block.phase, AssistantPhase::Reasoning);
                assert_eq!(view.content, expected.delta);
            }
        }
        assert!(
            validated_reply_fork_publication_events(
                &final_without_status,
                fixture.workspace.id.as_str(),
                fixture.conversation,
                &fixture.journal,
            )
            .is_err(),
            "原Reasoning发布不能替代最终选定回复的status/settlement门槛"
        );
        let mut forbidden_kinds = BTreeSet::new();
        for event in &fixture.journal {
            let kind = match &event.payload {
                ChatDriverEvent::Agent(agent) => match &agent.payload {
                    echo_agent::agent::AgentEvent::FinalAnswer(_) => Some("final"),
                    echo_agent::agent::AgentEvent::ExecutionStatus { .. } => Some("status"),
                    _ => None,
                },
                ChatDriverEvent::TurnStatus { .. } => Some("turn"),
                _ => None,
            };
            if let Some(kind) = kind {
                forbidden_kinds.insert(kind);
                let mut contaminated = pure_import.clone();
                contaminated.events.push(event.clone());
                assert_native_import_refused(&final_without_status, &contaminated)?;
            }
        }
        assert_eq!(forbidden_kinds.len(), 3);
        assert_eq!(
            serde_json::to_value(&final_without_status)?,
            unchanged_clone,
            "不能删改clone Reasoning或FinalAnswer做绿"
        );
        fixture.assert_unchanged().await
    }

    #[tokio::test]
    async fn reply_fork_unadapted_reasoning_unknown_tools_copy_cold_delete_refork()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::MixedReasoningUnknown,
            SourceRows::Untouched,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_unadapted_missing_phase_mixed_body_tools_copy_cold_delete_refork()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::MissingPhaseMixed,
            SourceRows::Untouched,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_unadapted_source_signed_parts_owned_copy_cold_delete_refork()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::ProgressSigned,
            SourceRows::Untouched,
            None,
        )
        .await
    }
    #[tokio::test]
    async fn reply_fork_unadapted_source_worktree_signed_parts_owned_copy_cold_delete_refork()
    -> anyhow::Result<()> {
        complete_chain_with_source(
            ReplyForkDestination::NewWorktree,
            NativeFixtureMode::ProgressSigned,
            SourceRows::Untouched,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_native_signed_opaque_preserved_across_copy_cold_refork()
    -> anyhow::Result<()> {
        complete_native_chain(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::Signed,
        )
        .await
    }
    #[tokio::test]
    async fn reply_fork_native_guarded_reasoning_approved_preserved_without_raw_fallback()
    -> anyhow::Result<()> {
        complete_native_chain(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::Guarded,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_actual_empty_approved_unknown_copy_cold_refork_no_raw_fallback()
    -> anyhow::Result<()> {
        complete_native_chain(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::EmptyApproved,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_nonstreaming_driver_cold_store_current_target_import_and_refork()
    -> anyhow::Result<()> {
        complete_chain_with_model_mode(
            ReplyForkDestination::CurrentWorkspace,
            NativeFixtureMode::Original,
            SourceRows::Untouched,
            None,
            false,
        )
        .await
    }

    #[tokio::test]
    async fn reply_fork_nonstreaming_driver_cold_store_worktree_import_and_refork()
    -> anyhow::Result<()> {
        complete_chain_with_model_mode(
            ReplyForkDestination::NewWorktree,
            NativeFixtureMode::Original,
            SourceRows::Untouched,
            None,
            false,
        )
        .await
    }

    #[test]
    fn reply_fork_plain_text_without_original_native_identity_is_rejected() {
        let message = Message::assistant("字符串不能证明最终发布".into());
        assert!(
            validated_reply_fork_publication_events(
                &message,
                "source-workspace",
                "source-conversation",
                &[],
            )
            .is_err(),
            "无原publication/actual status的文本不能作为已确认Fork来源"
        );
    }

    #[tokio::test]
    async fn reply_fork_resource_chain_cold_current_workspace_reader_source_delete_refork()
    -> anyhow::Result<()> {
        complete_chain(ReplyForkDestination::CurrentWorkspace).await
    }
    #[tokio::test]
    async fn reply_fork_resource_chain_cold_new_worktree_reader_source_delete_refork()
    -> anyhow::Result<()> {
        complete_chain(ReplyForkDestination::NewWorktree).await
    }
}

// 显式原生SSH验收：只有有真实target fixture的runner用--ignored调用，不能算默认测试GREEN。
// 不修改ssh配置、不记录凭据；目标JSON仅含已有TerminalTarget的host/user/port。
#[cfg(unix)]
#[tokio::test]
#[ignore = "需要EKO_RESOURCE_FORK_SSH_TARGET_JSON与原生SSH key/agent；非本机仿真"]
async fn reply_fork_real_ssh_target_binding_preserves_independent_target_after_source_close()
-> anyhow::Result<()> {
    use echo_agent_app_core::api::terminal::{TerminalService, TerminalTarget};
    let target: TerminalTarget =
        serde_json::from_str(&std::env::var("EKO_RESOURCE_FORK_SSH_TARGET_JSON")?)?;
    anyhow::ensure!(
        matches!(target, TerminalTarget::Ssh { .. }),
        "必须显式提供真实SSH target"
    );
    let service = TerminalService::new();
    let source_owner = format!("resource-source-{}", uuid::Uuid::new_v4());
    let draft_owner = format!("reply-fork-auth-{}", uuid::Uuid::new_v4());
    let result: anyhow::Result<()> = async {
        service
            .create_project_ssh_auth(source_owner.clone(), target.clone(), 24, 80)
            .await
            .map_err(anyhow::Error::msg)?;
        let source = service
            .project_ssh(&source_owner)
            .map_err(anyhow::Error::msg)?;
        // 首次OpenSSH复用socket建立可能尚在进行；只等这次真实连接，不切目标/不造authenticated状态。
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if source.check_authenticated().await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await?;
        let expected = source
            .effective_target()
            .map_err(anyhow::Error::msg)?
            .clone();
        source
            .verify_effective_target(&expected)
            .map_err(anyhow::Error::msg)?;
        let wrong = match &expected {
            TerminalTarget::Ssh { host, user, port } => TerminalTarget::Ssh {
                host: host.clone(),
                user: Some(format!(
                    "{}-wrong-account",
                    user.as_deref().unwrap_or("fixture")
                )),
                port: *port,
            },
            _ => anyhow::bail!("effective target不是SSH"),
        };
        anyhow::ensure!(source.verify_effective_target(&wrong).is_err());
        anyhow::ensure!(
            service
                .create_project_ssh_auth(source_owner.clone(), wrong, 24, 80)
                .await
                .is_err(),
            "相同owner不能换账号/目标"
        );
        anyhow::ensure!(Arc::ptr_eq(
            &source,
            &service
                .project_ssh(&source_owner)
                .map_err(anyhow::Error::msg)?
        ));
        service
            .create_project_ssh_auth(draft_owner.clone(), expected.clone(), 24, 80)
            .await
            .map_err(anyhow::Error::msg)?;
        let draft = service
            .project_ssh_exclusive(&draft_owner)
            .map_err(anyhow::Error::msg)?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if draft.check_authenticated().await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await?;
        anyhow::ensure!(!Arc::ptr_eq(&source, &draft));
        anyhow::ensure!(source.generation() != draft.generation());
        draft
            .verify_effective_target(&expected)
            .map_err(anyhow::Error::msg)?;
        let target_owner = format!("resource-target-{}", uuid::Uuid::new_v4());
        service
            .reassign_project_ssh_checked(&draft_owner, &target_owner, &draft)
            .await
            .map_err(anyhow::Error::msg)?;
        service
            .close_project_ssh_checked(&source_owner, &source)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(Arc::ptr_eq(
            &draft,
            &service
                .project_ssh(&target_owner)
                .map_err(anyhow::Error::msg)?
        ));
        draft
            .check_authenticated()
            .await
            .map_err(anyhow::Error::msg)?;
        draft
            .verify_effective_target(&expected)
            .map_err(anyhow::Error::msg)?;
        service
            .close_project_ssh_checked(&target_owner, &draft)
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(())
    }
    .await;
    let cleanup = service.close_all().await;
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(anyhow::anyhow!("原生SSH测试结算失败: {cleanup}")),
        (Err(primary), Err(cleanup)) => {
            Err(primary.context(format!("原生SSH测试结算失败: {cleanup}")))
        }
    }
}

fn assert_native_import_refused(
    message: &Message,
    import: &NativeHistoryImport,
) -> anyhow::Result<()> {
    assert!(verify_native_history_import(message, import).is_err());
    let mut row = echo_agent::memory::project_message("annotation-target", message)?;
    attach_native_history_import(&mut row, import.clone())?;
    assert!(
        project_stored_message_with_publications(
            row,
            Some("annotation-target"),
            Some("annotation-target"),
            &[]
        )
        .is_err(),
        "真实loader不能对坏native import回退raw正文"
    );
    Ok(())
}

// 对生产write端和原verify的直接负控，不代替资源链运行。
#[tokio::test]
async fn native_import_full_message_digest_preserves_annotation_boundary_and_rejects_tampering()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    use echo_agent::llm::types::{ContentPart, LinkedResource, MessageContent, ReasoningBlock};
    use sha2::{Digest as _, Sha256};
    let fixture = produce_and_reopen(false).await?;
    let mut original = echo_agent::memory::restore_message(&fixture.stored)?;
    original.reasoning_content = Some("原推理内容🙂".into());
    original.reasoning_blocks = Some(vec![
        ReasoningBlock::Signed {
            thinking: "原签名推理🙂".into(),
            signature: "native-signature-preserved".into(),
        },
        ReasoningBlock::Redacted {
            data: "native-redacted-preserved".into(),
        },
        ReasoningBlock::Opaque {
            provider: "responses".into(),
            id: "native-opaque-id".into(),
            data: "native-opaque-data".into(),
            summary: vec!["native-summary".into()],
        },
    ]);
    original.content = MessageContent::Parts(vec![
        ContentPart::Text {
            text: "canonical Parts🙂".into(),
        },
        ContentPart::ResourceLink {
            resource: Box::new(LinkedResource {
                annotations: Some(serde_json::json!({"audience":["assistant"]})),
                description: Some("外部原生resource".into()),
                mime_type: Some("text/plain".into()),
                name: "原生资源".into(),
                size: Some(8),
                title: Some("原标题".into()),
                uri: "opaque-resource:fixture".into(),
                meta: Some(
                    serde_json::json!({"unknown_extension":{"explicit_null":null,"exact":"原值🙂"}}),
                ),
            }),
        },
    ]);
    let before = serde_json::to_value(&original)?;
    let first = fixture
        .publications
        .iter()
        .find(|event| matches!(event.payload, ChatDriverEvent::Agent(_)))
        .context("原publication缺失")?;
    let status = original.execution_status.as_ref().context("原status缺失")?;
    let import = NativeHistoryImport {
        source_workspace_id: first.workspace_id.clone(),
        source_conversation_id: first
            .conversation_id
            .clone()
            .context("原conversation缺失")?,
        source_root_turn_id: first.root_turn_id.clone(),
        source_turn_id: first.turn_id.clone(),
        source_message_id: first.message_id.clone(),
        execution_id: status.execution_id.clone(),
        message_content_sha256: hex::encode(Sha256::digest(serde_json::to_vec(&original)?)),
        events: fixture.publications.clone(),
    };
    let mut row = echo_agent::memory::project_message("annotation-target", &original)?;
    attach_native_history_import(&mut row, import.clone())?;
    let mut metadata: serde_json::Value =
        serde_json::from_str(row.attachments_json.as_deref().context("metadata缺失")?)?;
    metadata
        .as_object_mut()
        .context("metadata不是对象")?
        .insert(
            "product_extension".into(),
            serde_json::json!({"display":"非canonical产品注解","nil":null}),
        );
    row.attachments_json = Some(serde_json::to_string(&metadata)?);
    let target = tempfile::tempdir()?;
    let store = FileConversationStore::new(target.path().join("transcript"))?;
    store
        .create_conversation(NewConversation {
            conversation_id: "annotation-target".into(),
            user_id: "fixture".into(),
            agent_type: None,
            title: None,
        })
        .await?;
    store.save_messages("annotation-target", &[row]).await?;
    drop(store);
    drop(fixture);
    let reopened = FileConversationStore::new(target.path().join("transcript"))?;
    let row = reopened
        .get_messages("annotation-target")
        .await?
        .into_iter()
        .next()
        .context("cold row缺失")?;
    let restored = echo_agent::memory::restore_message(&row)?;
    assert_eq!(
        serde_json::to_value(&restored)?,
        before,
        "phase/approved/raw/signature/ResourceLink meta不能为hash修复被删"
    );
    verify_native_history_import(&restored, &import)?;
    // 本专用序列化夹具改了正文；hash一致不等于原publication批准了新正文。
    let presentation = project_stored_message_with_publications(
        row.clone(),
        Some("annotation-target"),
        Some("annotation-target"),
        &[],
    )?;
    assert!(!presentation.final_answer_confirmed);
    assert!(
        presentation.content.is_none(),
        "原publication未批准的正文不能raw fallback"
    );
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&restored)?));
    assert_eq!(
        digest, import.message_content_sha256,
        "annotation本身不得递归进入canonical Message摘要"
    );
    let mut bad_hash = import.clone();
    bad_hash.message_content_sha256 = "0".repeat(64);
    assert_native_import_refused(&restored, &bad_hash)?;
    let mut bad_origin = import.clone();
    bad_origin.source_workspace_id = "unknown-source-workspace".into();
    assert_native_import_refused(&restored, &bad_origin)?;
    let mut bad_execution = import.clone();
    bad_execution.execution_id = "unrelated-execution".into();
    assert_native_import_refused(&restored, &bad_execution)?;
    let mut changed = restored.clone();
    changed.approved_assistant_blocks = Some(Vec::new());
    assert_native_import_refused(&changed, &import).context("approved变化必须坏hash")?;
    let mut changed = restored.clone();
    changed
        .assistant_blocks
        .as_mut()
        .and_then(|blocks| blocks.first_mut())
        .context("原raw block缺失")?
        .phase = AssistantPhase::Commentary;
    assert_native_import_refused(&changed, &import).context("phase变化必须坏hash")?;
    let mut changed = restored.clone();
    let signed = changed
        .reasoning_blocks
        .as_mut()
        .and_then(|blocks| blocks.first_mut())
        .context("签名缺失")?;
    if let ReasoningBlock::Signed { signature, .. } = signed {
        *signature = "tampered-signature".into();
    } else {
        anyhow::bail!("签名夹具类型错误");
    }
    assert_native_import_refused(&changed, &import).context("signature变化必须坏hash")?;
    let mut changed = restored.clone();
    let MessageContent::Parts(parts) = &mut changed.content else {
        anyhow::bail!("Parts缺失");
    };
    let link = parts
        .iter_mut()
        .find_map(|part| match part {
            ContentPart::ResourceLink { resource } => Some(resource),
            _ => None,
        })
        .context("ResourceLink缺失")?;
    link.meta = Some(serde_json::json!({"unknown_extension":"tampered"}));
    assert_native_import_refused(&changed, &import)
        .context("ResourceLink metadata变化必须坏hash")?;
    let preserved: serde_json::Value =
        serde_json::from_str(row.attachments_json.as_deref().context("metadata缺失")?)?;
    assert_eq!(
        preserved.get("product_extension"),
        metadata.get("product_extension")
    );
    Ok(())
}

#[path = "native_chart_producer_tests.rs"]
mod native_chart_producer_tests;
