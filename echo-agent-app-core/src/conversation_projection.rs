//! Application-owned conversation projection DTOs.
//!
//! Canonical conversation persistence and search belong to the framework
//! `ConversationStore`. These types only carry UI metadata at the Tauri
//! boundary; this module owns no files or storage lifecycle.

use serde::{Deserialize, Serialize};

/// 消息的序列化表示
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub role: String,
    /// Original canonical timestamp, never the GUI load time.
    pub created_at: String,
    pub content: Option<String>,
    /// 原 Framework 消息的块投影；它不拥有执行完成权威。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_blocks: Option<Vec<SavedAssistantBlock>>,
    /// 仅当原执行 owner、FinalAnswer、Settled 和产品 TurnStatus 均核对成功时为 true。
    #[serde(default, skip_serializing_if = "is_false")]
    pub final_answer_confirmed: bool,
    /// 既有 publication 校验匹配到的实际原 Settled，不由 durable Candidate 合成。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_status: Option<echo_agent::agent::ExecutionStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer_source: Option<NativeFinalAnswerSource>,
    /// True when this transcript segment came from AgentRouter coordination.
    /// Primary conversation views filter these messages before serialization;
    /// Side Conversation views retain and label them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub internal_agent: bool,
    /// Original typed AgentRouter payload preserved in the canonical input ResourceLink.
    /// This is display provenance, never a direct-user instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_input: Option<crate::agent_router::AgentMessage>,
    pub tool_calls: Option<Vec<SavedToolCall>>,
    /// Tool call result (for tool role messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// Execution order tracking: records the sequence of thinking and tool calls
    /// for correct chronological interleaving when loading from history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_steps: Option<Vec<SavedExecutionStep>>,
    /// User-uploaded attachments (images/documents) attached to this message.
    /// Small resources retain their data URL; artifact-backed text keeps only
    /// display metadata so its body is not duplicated in conversation JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<SavedAttachment>>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedAssistantBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_event_id: Option<String>,
    pub block: echo_agent::agent::AssistantMessageBlock,
    pub content: String,
}

/// 只读最终呈现坐标；两个 sequence 域保持独立。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeFinalAnswerSource {
    pub execution_id: String,
    pub request_id: String,
    /// 来自实际已匹配 Settled；不与块 sequence 比较。
    pub sequence: u64,
    pub blocks: Vec<NativeFinalAnswerBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeFinalAnswerBlock {
    pub request_id: String,
    pub block_id: String,
    /// approved 原块最后观测的模型响应内 sequence。
    pub sequence: u64,
    pub publication_event_id: String,
}

/// 只归并同一块的原增量。未知阶段可由后继显式事实补齐，不能覆盖已知阶段。
pub fn project_assistant_blocks(
    blocks: Vec<echo_agent::agent::AssistantMessageBlock>,
) -> Result<Vec<SavedAssistantBlock>, String> {
    let mut projected: Vec<SavedAssistantBlock> = Vec::new();
    for block in blocks {
        if let Some(previous) = projected.iter_mut().find(|entry| {
            entry.block.request_id == block.request_id && entry.block.block_id == block.block_id
        }) {
            if previous.block.complete {
                return Err("已完成原生消息块出现后继增量，原记录已保留".to_string());
            }
            if previous.block.phase != echo_agent::agent::AssistantPhase::Unknown
                && previous.block.phase != block.phase
            {
                return Err("原生消息块阶段冲突，原记录已保留".to_string());
            }
            previous.content.push_str(&block.delta);
            previous.block = block;
        } else {
            projected.push(SavedAssistantBlock {
                execution_id: None,
                publication_event_id: None,
                content: block.delta.clone(),
                block,
            });
        }
    }
    Ok(projected)
}

/// 已发布的原生历史视图；它只从原 ChatEventLog 派生，不拥有执行或 Goal 状态。
#[derive(Debug)]
pub struct NativeHistoryPresentation {
    pub blocks: Vec<SavedAssistantBlock>,
    pub content: Option<String>,
    pub final_answer_confirmed: bool,
    pub execution_status: Option<echo_agent::agent::ExecutionStatus>,
    pub final_answer_source: Option<NativeFinalAnswerSource>,
    pub unavailable: bool,
}

/// 已确认历史在 Fork 目标中的只读 provenance。它复用现有
/// `attachments_json` 产品元数据边界，不注册目标 execution/Run/Goal。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeHistoryImport {
    pub source_workspace_id: String,
    pub source_conversation_id: String,
    pub source_root_turn_id: String,
    pub source_turn_id: String,
    pub source_message_id: String,
    pub execution_id: String,
    pub message_content_sha256: String,
    pub events: Vec<crate::chat_event_log::ChatEventEnvelope>,
}

fn publication_agent(
    event: &crate::chat_event_log::ChatEventEnvelope,
) -> Option<&echo_agent::agent::EventEnvelope> {
    match &event.payload {
        crate::chat_driver::ChatDriverEvent::Agent(agent) => Some(agent.as_ref()),
        _ => None,
    }
}

fn publication_scope_matches(
    left: &crate::chat_event_log::ChatEventEnvelope,
    right: &crate::chat_event_log::ChatEventEnvelope,
) -> bool {
    left.workspace_id == right.workspace_id
        && left.conversation_id == right.conversation_id
        && left.root_turn_id == right.root_turn_id
        && left.turn_id == right.turn_id
        && left.message_id == right.message_id
}

/// 无终态坐标的中间块只能依据同一原消息的批准块与实际发布记录呈现。
/// 返回原 envelope，供 Fork 原样迁移；这条路径不赋予最终回复或执行完成资格。
pub fn native_progress_publication_events(
    message: &echo_agent::llm::types::Message,
    workspace_id: &str,
    conversation_id: &str,
    events: &[crate::chat_event_log::ChatEventEnvelope],
) -> Result<Vec<crate::chat_event_log::ChatEventEnvelope>, String> {
    use echo_agent::agent::{AgentEvent, AssistantPhase};
    use std::collections::HashSet;

    if message.role != echo_agent::llm::types::Role::Assistant || message.execution_status.is_some()
    {
        return Ok(Vec::new());
    }
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    let mut selected: Vec<crate::chat_event_log::ChatEventEnvelope> = Vec::new();
    let mut outer_ids = HashSet::new();
    let mut inner_ids = HashSet::new();
    for approved in message
        .approved_assistant_blocks
        .as_deref()
        .unwrap_or_default()
    {
        // Reasoning 保留其原生类型供独立折叠；缺 phase 的工具前正文保持 Unknown。
        // 无工具的 Unknown 和最终块仍不具有本路径的呈现或迁移资格。
        let eligible = match approved.phase {
            AssistantPhase::Reasoning => true,
            AssistantPhase::Commentary | AssistantPhase::PartialAnswer => {
                approved.reasoning.is_none()
            }
            AssistantPhase::Unknown => has_tool_calls && approved.reasoning.is_none(),
            _ => false,
        };
        if !eligible {
            continue;
        }
        let mut matches = events.iter().filter(|event| {
            event.workspace_id == workspace_id
                && event.conversation_id.as_deref() == Some(conversation_id)
                && publication_agent(event).is_some_and(|agent| {
                    matches!(&agent.payload, AgentEvent::AssistantMessage { block } if block == approved)
                })
        });
        let event = matches
            .next()
            .ok_or_else(|| "已批准进度缺少原发布记录，原始消息已保留".to_string())?;
        let original = echo_agent::utils::canonical_json::canonical_json_bytes(event)
            .map_err(|error| error.to_string())?;
        for replay in matches {
            // 同一完整原记录可以重放；新 outer/inner 身份、顺序或载荷仍是歧义。
            let replay = echo_agent::utils::canonical_json::canonical_json_bytes(replay)
                .map_err(|error| error.to_string())?;
            if replay != original {
                return Err("已批准进度的发布身份不唯一，原始消息已保留".to_string());
            }
        }
        let agent =
            publication_agent(event).ok_or_else(|| "普通进度缺少 Agent envelope".to_string())?;
        if agent.execution_id.is_none()
            || agent.conversation_id.as_ref().map(|id| id.as_str())
                != event.conversation_id.as_deref()
            || agent.turn_id.as_str() != event.turn_id
            || agent.message_id.as_ref().map(|id| id.as_str()) != Some(event.message_id.as_str())
            || event.root_turn_id != event.message_id
            || !outer_ids.insert(event.event_id.clone())
            || !inner_ids.insert(agent.event_id.to_string())
        {
            return Err("普通进度的原发布坐标冲突，原始消息已保留".to_string());
        }
        if let Some(first) = selected.first() {
            let original = publication_agent(first)
                .ok_or_else(|| "首条普通进度缺少 Agent envelope".to_string())?;
            if !publication_scope_matches(event, first)
                || agent.execution_id != original.execution_id
                || agent.stream_id != original.stream_id
                || agent.run_id != original.run_id
            {
                return Err("普通进度跨越原执行或消息边界，原始消息已保留".to_string());
            }
        }
        if let Some(previous) = selected.last() {
            let original = publication_agent(previous)
                .ok_or_else(|| "前条普通进度缺少 Agent envelope".to_string())?;
            if event.sequence <= previous.sequence || agent.sequence <= original.sequence {
                return Err("普通进度的原发布顺序冲突，原始消息已保留".to_string());
            }
        }
        selected.push(event.clone());
    }
    Ok(selected)
}

pub fn project_native_history_publication(
    message: &echo_agent::llm::types::Message,
    workspace_id: &str,
    conversation_id: &str,
    events: &[crate::chat_event_log::ChatEventEnvelope],
) -> Result<Option<NativeHistoryPresentation>, String> {
    use echo_agent::agent::{AgentEvent, ExecutionStatusKind};
    use std::collections::HashSet;

    if message.role != echo_agent::llm::types::Role::Assistant
        || (message.assistant_blocks.is_none()
            && message.approved_assistant_blocks.is_none()
            && message.execution_status.is_none())
    {
        return Ok(None);
    }
    let mut view = NativeHistoryPresentation {
        blocks: Vec::new(),
        content: None,
        final_answer_confirmed: false,
        execution_status: None,
        final_answer_source: None,
        unavailable: true,
    };
    let Some(expected) = message.execution_status.as_ref() else {
        let published =
            native_progress_publication_events(message, workspace_id, conversation_id, events)?;
        let blocks = published
            .iter()
            .filter_map(|event| match &publication_agent(event)?.payload {
                AgentEvent::AssistantMessage { block } => Some(block.clone()),
                _ => None,
            })
            .collect();
        view.blocks = project_assistant_blocks(blocks)?;
        for view in &mut view.blocks {
            view.publication_event_id = published.iter().find_map(|event| {
                let agent = publication_agent(event)?;
                matches!(&agent.payload, AgentEvent::AssistantMessage { block } if block == &view.block)
                    .then(|| agent.event_id.to_string())
            });
            view.execution_id = published.iter().find_map(|event| {
                let agent = publication_agent(event)?;
                match &agent.payload {
                    AgentEvent::AssistantMessage { block } if block == &view.block => {
                        agent.execution_id.as_ref().map(ToString::to_string)
                    }
                    _ => None,
                }
            });
        }
        view.unavailable = view.blocks.is_empty();
        return Ok(Some(view));
    };
    let Some(request_id) = expected.request_id.as_deref() else {
        return Ok(Some(view));
    };
    if expected.execution_id.trim().is_empty() || request_id.trim().is_empty() {
        return Ok(Some(view));
    }

    // 只接原实际 owner。EKO 原 for_chat 必须薄转交既有 driver_execution_context；
    // 头缺执行身份时保未确认，不从文字/块前缀/同一Run反推 owner。
    let owned: Vec<_> = events
        .iter()
        .filter(|event| {
            event.workspace_id == workspace_id
                && event.conversation_id.as_deref() == Some(conversation_id)
                && publication_agent(event).is_some_and(|agent| {
                    agent
                        .execution_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == expected.execution_id)
                })
        })
        .collect();
    let scopes: HashSet<_> = owned
        .iter()
        .map(|event| (&event.root_turn_id, &event.turn_id, &event.message_id))
        .collect();
    if scopes.len() != 1 {
        return Ok(Some(view));
    }

    // approved 本身只给 payload 资格；实际呈现还须与原已发布事件逐字段一致。
    // Some(empty) 和 None 均不回退 provider raw assistant_blocks。
    let mut used_publications = HashSet::new();
    let mut selected = Vec::new();
    let mut previous_sequence = None;
    let mut missing = false;
    for approved in message
        .approved_assistant_blocks
        .as_deref()
        .unwrap_or_default()
    {
        if approved.request_id != request_id {
            missing = true;
            continue;
        }
        let published = owned.iter().find_map(|event| {
            let agent = publication_agent(event)?;
            if used_publications.contains(agent.event_id.as_str()) {
                return None;
            }
            match &agent.payload {
                AgentEvent::AssistantMessage { block } if *block == *approved => {
                    Some((*event, agent.event_id.as_str(), block))
                }
                _ => None,
            }
        });
        let Some((event, event_id, block)) = published else {
            missing = true;
            continue;
        };
        if previous_sequence.is_some_and(|sequence| event.sequence <= sequence) {
            return Err("原生历史的已发布块顺序冲突，原始记录已保留".to_string());
        }
        previous_sequence = Some(event.sequence);
        used_publications.insert(event_id);
        selected.push(block.clone());
    }
    view.blocks = project_assistant_blocks(selected)?;
    // 上面每项均已逐字段匹配同一 owner 的实际原 publication；raw 回退不带本字段。
    for block in &mut view.blocks {
        block.execution_id = Some(expected.execution_id.clone());
        block.publication_event_id = owned.iter().find_map(|event| {
            let agent = publication_agent(event)?;
            matches!(&agent.payload, AgentEvent::AssistantMessage { block: published } if published == &block.block)
                .then(|| agent.event_id.to_string())
        });
    }

    let mut terminal_ids = HashSet::new();
    let terminals: Vec<_> = owned
        .iter()
        .filter_map(|event| {
            let agent = publication_agent(event)?;
            let AgentEvent::ExecutionStatus(status) = &agent.payload else {
                return None;
            };
            if status.execution_id != expected.execution_id
                || !matches!(
                    status.status,
                    ExecutionStatusKind::Settled
                        | ExecutionStatusKind::Failed
                        | ExecutionStatusKind::Cancelled
                )
                || !terminal_ids.insert(agent.event_id.as_str())
            {
                return None;
            }
            Some((*event, status))
        })
        .collect();
    let terminal = match terminals.as_slice() {
        [(event, status)]
            if status.status == ExecutionStatusKind::Settled
                && status.request_id == expected.request_id
                && status.sequence == expected.sequence =>
        {
            Some((*event, *status))
        }
        _ => None,
    };
    let Some((settled_event, actual_settled)) = terminal else {
        view.unavailable = missing || view.blocks.is_empty();
        return Ok(Some(view));
    };

    let mut final_ids = HashSet::new();
    let finals: Vec<_> = owned
        .iter()
        .filter_map(|event| {
            let agent = publication_agent(event)?;
            if event.sequence >= settled_event.sequence
                || !publication_scope_matches(event, settled_event)
                || !final_ids.insert(agent.event_id.as_str())
            {
                return None;
            }
            match &agent.payload {
                AgentEvent::FinalAnswer(answer) => Some(answer.as_str()),
                _ => None,
            }
        })
        .collect();
    let expected_text = message.content.as_text().unwrap_or_default();
    let answer_matches = match finals.as_slice() {
        [published] => {
            use sha2::{Digest, Sha256};
            !expected_text.trim().is_empty()
                && expected_text.as_bytes() == published.as_bytes()
                && Sha256::digest(expected_text.as_bytes()) == Sha256::digest(published.as_bytes())
        }
        _ => false,
    };

    // Frame AgentExecDone 与产品 Turn/TaskRun 仍分开。原产品关闭若失败/取消，
    // 不能因为前面曾有模型 final 或 Frame settled 就开放成功回复操作。
    let product_terminal = events
        .iter()
        .filter(|event| {
            event.sequence > settled_event.sequence
                && publication_scope_matches(event, settled_event)
        })
        .filter_map(|event| match &event.payload {
            crate::chat_driver::ChatDriverEvent::TurnStatus { status }
                if matches!(status.as_str(), "completed" | "failed" | "cancelled") =>
            {
                Some((event.sequence, status.as_str()))
            }
            _ => None,
        })
        .max_by_key(|(sequence, _)| *sequence);
    view.final_answer_confirmed =
        answer_matches && product_terminal.is_some_and(|(_, status)| status == "completed");
    if view.final_answer_confirmed {
        view.content = Some(expected_text.to_string());
        view.execution_status = Some(actual_settled.clone());
        // 这里只投影原已批准且实际发布的块坐标，不重做最终校验、不读 raw 候选。
        // 明确 final phase 存在时，Unknown 不是最终候选；否则原正常候选为 Unknown。
        let phase = if view
            .blocks
            .iter()
            .any(|item| item.block.phase == echo_agent::agent::AssistantPhase::FinalAnswer)
        {
            echo_agent::agent::AssistantPhase::FinalAnswer
        } else {
            echo_agent::agent::AssistantPhase::Unknown
        };
        let selected = view
            .blocks
            .iter()
            .filter(|item| item.block.phase == phase && item.block.request_id == request_id)
            .filter_map(|item| {
                Some(NativeFinalAnswerBlock {
                    request_id: item.block.request_id.clone(),
                    block_id: item.block.block_id.clone(),
                    sequence: item.block.sequence,
                    publication_event_id: item.publication_event_id.clone()?,
                })
            })
            .collect();
        view.final_answer_source = Some(NativeFinalAnswerSource {
            execution_id: actual_settled.execution_id.clone(),
            request_id: request_id.to_string(),
            sequence: actual_settled.sequence,
            blocks: selected,
        });
    }
    view.unavailable = missing || (!view.final_answer_confirmed && view.blocks.is_empty());
    Ok(Some(view))
}

/// A persisted attachment reference (stored inside SavedMessage).
///
/// `url` is a complete data URL for small inline resources and empty for
/// artifact-backed text resources.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedAttachment {
    pub name: String,
    pub mime_type: String,
    pub url: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Execution step for tracking thinking/tool interleaving order
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedExecutionStep {
    #[serde(rename = "type")]
    pub step_type: String, // "thinking" or "tool"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// 当前消息的 UI 附件元数据；仅接受 object，不解析旧 thinking 数组或 round 投影。
#[derive(Debug, Serialize, Deserialize)]
pub struct AttachmentsPayload {
    /// Stable GUI message identity used to attach TaskRun/Subagent history after reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Frontend display text. The canonical `StoredMessage.content` remains
    /// the agent-facing projection (for example an artifact reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    #[serde(default)]
    pub execution_steps: Vec<SavedExecutionStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_history_import: Option<NativeHistoryImport>,
    /// Real user-uploaded attachments (images/documents). Despite the column
    /// name `attachments_json` historically holding thinking segments, this key
    /// holds the actual attachment data URL so messages render on reload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SavedAttachment>,
}

impl AttachmentsPayload {
    /// 当前格式只有 object；旧数组不能被按字段顺序误解成当前 struct。
    pub fn parse(s: &str) -> Option<Self> {
        let value = serde_json::from_str::<serde_json::Value>(s).ok()?;
        if !value.is_object() {
            return None;
        }
        serde_json::from_value(value).ok()
    }
}

/// GUI 保留 Framework 原调用的 id/type/function/arguments，不另造平铺协议。
pub type SavedToolCall = echo_agent::llm::types::ToolCall;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_message_projection_keeps_exact_identity_and_phase() -> Result<(), String> {
        let first: echo_agent::agent::AssistantMessageBlock =
            serde_json::from_value(serde_json::json!({
                "block_id":"body", "request_id":"request-a", "sequence":0,
                "phase":"commentary", "delta":"先检查", "complete":false,
                "provider":"fixture", "reasoning":null,
            }))
            .map_err(|error| error.to_string())?;
        let mut second = first.clone();
        second.delta = "，再验证。".to_string();
        second.complete = true;
        let mut other = first.clone();
        other.request_id = "request-b".to_string();
        other.delta = "另一请求".to_string();
        other.complete = true;
        let projected = project_assistant_blocks(vec![first, second.clone(), other])?;
        assert_eq!(projected.len(), 2);
        let first = projected.first().ok_or("native block missing")?;
        assert_eq!(first.content, "先检查，再验证。");
        assert_eq!(first.block, second);
        let restored: Vec<SavedAssistantBlock> = serde_json::from_str(
            &serde_json::to_string(&projected).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(restored.first().map(|view| &view.block), Some(&second));
        Ok(())
    }

    #[test]
    fn native_message_projection_rejects_known_phase_conflict_and_late_delta() -> Result<(), String>
    {
        let first: echo_agent::agent::AssistantMessageBlock =
            serde_json::from_value(serde_json::json!({
                "block_id":"body", "request_id":"request", "sequence":0,
                "phase":"commentary", "delta":"公开正文", "complete":false,
                "provider":null, "reasoning":null,
            }))
            .map_err(|error| error.to_string())?;
        let mut conflict = first.clone();
        conflict.phase = echo_agent::agent::AssistantPhase::Reasoning;
        assert!(project_assistant_blocks(vec![first.clone(), conflict]).is_err());
        let mut completed = first.clone();
        completed.complete = true;
        assert!(project_assistant_blocks(vec![completed, first]).is_err());
        Ok(())
    }

    #[test]
    fn test_saved_message_serialize() -> Result<(), String> {
        let msg = SavedMessage {
            message_id: Some("message-1".to_string()),
            role: "user".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            content: Some("hello".to_string()),
            assistant_blocks: None,
            final_answer_confirmed: false,
            execution_status: None,
            final_answer_source: None,
            internal_agent: false,
            agent_input: None,
            tool_calls: None,
            execution_steps: None,
            tool_result: None,
            attachments: None,
        };
        let json = serde_json::to_string(&msg).map_err(|error| error.to_string())?;
        assert!(json.contains("\"role\":\"user\""));
        Ok(())
    }

    #[test]
    fn attachments_payload_round_trips_current_tool_steps_without_retired_fields()
    -> Result<(), String> {
        let payload = AttachmentsPayload {
            message_id: Some("message-1".to_string()),
            display_content: Some("visible text".to_string()),
            execution_steps: vec![SavedExecutionStep {
                step_type: "tool".to_string(),
                index: None,
                call_id: Some("call-1".to_string()),
            }],
            native_history_import: None,
            attachments: Vec::new(),
        };
        let encoded = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
        let decoded = AttachmentsPayload::parse(&encoded).ok_or("current payload missing")?;
        assert_eq!(
            decoded
                .execution_steps
                .first()
                .and_then(|step| step.call_id.as_deref()),
            Some("call-1")
        );
        let wire: serde_json::Value =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        assert!(wire.get("thinking_segments").is_none());
        assert!(wire.get("execution_rounds").is_none());
        Ok(())
    }

    #[test]
    fn execution_steps_store_tool_call_identity() -> Result<(), String> {
        let step = SavedExecutionStep {
            step_type: "tool".to_string(),
            index: None,
            call_id: Some("call-42".to_string()),
        };
        let value = serde_json::to_value(step).map_err(|error| error.to_string())?;
        assert_eq!(
            value.get("call_id").and_then(serde_json::Value::as_str),
            Some("call-42")
        );
        assert!(value.get("index").is_none());
        Ok(())
    }

    #[test]
    fn attachments_payload_rejects_non_object_input() {
        for raw in [r#"["分析","完成"]"#, "[]", "null", "42", r#""text""#] {
            assert!(AttachmentsPayload::parse(raw).is_none());
        }
    }
}
