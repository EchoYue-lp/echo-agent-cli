//! Existing text projections over the original Agent routing services.
use crate::agent_router::{AgentAddress, AgentDeliveryRecord, AgentGroupMember, AgentMessage};
use crate::state::AppState;
use crate::workspace::WorkspaceId;
use std::sync::Arc;
/// Read/parse/admission knowledge from the original command owner. These are
/// plain result values; the channel's existing delivery journal owns receipts.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentTextCommandError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Operation(String),
}

pub async fn list_agent_endpoints(state: Option<&Arc<AppState>>) -> String {
    list_agent_endpoints_result(state)
        .await
        .unwrap_or_else(|error| error.to_string())
}

pub(crate) async fn list_agent_endpoints_result(
    state: Option<&Arc<AppState>>,
) -> Result<String, AgentTextCommandError> {
    let state = state.ok_or_else(|| {
        AgentTextCommandError::Unavailable("Agent routing is not initialized.".to_string())
    })?;
    let endpoints = state.discover_agent_endpoints().await.map_err(|error| {
        AgentTextCommandError::Unavailable(format!("Agent discovery failed: {error}"))
    })?;
    if endpoints.is_empty() {
        return Ok("No persisted Agent conversations.".to_string());
    }
    Ok(endpoints
        .into_iter()
        .map(|endpoint| {
            format!(
                "{}/{}  {}",
                endpoint.address.workspace_id,
                endpoint.address.conversation_id,
                endpoint
                    .conversation_title
                    .as_deref()
                    .unwrap_or("Untitled conversation")
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub async fn send_agent_text(
    state: Option<&Arc<AppState>>,
    from: Option<AgentAddress>,
    workspace_id: &str,
    conversation_id: &str,
    text: &str,
) -> String {
    send_agent_text_result(state, from, workspace_id, conversation_id, text)
        .await
        .unwrap_or_else(|error| error.to_string())
}

pub(crate) async fn send_agent_text_result(
    state: Option<&Arc<AppState>>,
    from: Option<AgentAddress>,
    workspace_id: &str,
    conversation_id: &str,
    text: &str,
) -> Result<String, AgentTextCommandError> {
    let state = state.ok_or_else(|| {
        AgentTextCommandError::Unavailable("Agent routing is not initialized.".to_string())
    })?;
    let target = AgentAddress::new(
        WorkspaceId::from_raw(workspace_id.to_string()),
        conversation_id.to_string(),
    );
    let receipt = state
        .send_agent_message_owned(AgentMessage::user_text(from, target, text))
        .await
        .map_err(|error| AgentTextCommandError::Operation(format!("Agent send failed: {error}")))?;
    let display = format!(
        "Message {} queued for {}/{}.",
        receipt.message_id, receipt.target.workspace_id, receipt.target.conversation_id
    );
    // Queue admission is the operation result; it is not recipient execution.
    // A degraded durable receipt must never become an Applied channel control.
    if !matches!(
        receipt.durability,
        crate::agent_router::AgentDeliveryDurability::Confirmed
    ) {
        return Err(AgentTextCommandError::Operation(display));
    }
    Ok(display)
}

pub async fn agent_delivery_status(
    state: Option<&Arc<AppState>>,
    workspace_id: &str,
    conversation_id: &str,
    message_id: Option<&str>,
) -> String {
    agent_delivery_status_result(state, workspace_id, conversation_id, message_id)
        .await
        .unwrap_or_else(|error| error.to_string())
}

pub(crate) async fn agent_delivery_status_result(
    state: Option<&Arc<AppState>>,
    workspace_id: &str,
    conversation_id: &str,
    message_id: Option<&str>,
) -> Result<String, AgentTextCommandError> {
    let state = state.ok_or_else(|| {
        AgentTextCommandError::Unavailable("Agent routing is not initialized.".to_string())
    })?;
    let target = AgentAddress::new(
        WorkspaceId::from_raw(workspace_id.to_string()),
        conversation_id.to_string(),
    );
    state
        .agent_delivery_records(&target)
        .await
        .map(|records| format_delivery_records(records, message_id))
        .map_err(|error| {
            AgentTextCommandError::Unavailable(format!("Agent status failed: {error}"))
        })
}

const AGENT_GROUP_USAGE: &str = "Usage: /agent-group <list|create|update|delete> [args]\n\
create <name> <leader-workspace> <leader-conversation> <role> <workspace> <conversation> [...]\n\
update <group-id> <name> <leader-workspace> <leader-conversation> <role> <workspace> <conversation> [...]\n\
delete <group-id>";

fn parse_group_members(args: &[&str]) -> Result<Vec<AgentGroupMember>, String> {
    if args.is_empty() || !args.len().is_multiple_of(3) {
        return Err("members must be repeated as <role> <workspace> <conversation>".to_string());
    }
    let mut members = Vec::with_capacity(args.len() / 3);
    let (chunks, _) = args.as_chunks::<3>();
    for chunk in chunks {
        let mut values = chunk.iter().copied();
        let role = values
            .next()
            .ok_or_else(|| "member role is missing".to_string())?;
        let workspace_id = values
            .next()
            .ok_or_else(|| "member workspace is missing".to_string())?;
        let conversation_id = values
            .next()
            .ok_or_else(|| "member conversation is missing".to_string())?;
        members.push(AgentGroupMember {
            address: AgentAddress::new(
                WorkspaceId::from_raw(workspace_id.to_string()),
                conversation_id.to_string(),
            ),
            subagent_role: role.to_string(),
            label: None,
        });
    }
    Ok(members)
}

fn format_agent_groups(groups: Vec<crate::agent_router::AgentGroup>) -> String {
    if groups.is_empty() {
        return "No Agent groups.".to_string();
    }
    groups
        .into_iter()
        .map(|group| {
            let members = group
                .members
                .into_iter()
                .map(|member| {
                    format!(
                        "{}={}/{}",
                        member.subagent_role,
                        member.address.workspace_id,
                        member.address.conversation_id
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}  {}  leader={}/{}  members=[{}]",
                group.group_id,
                group.name,
                group.leader.workspace_id,
                group.leader.conversation_id,
                members
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn execute_agent_group_command(state: Option<&Arc<AppState>>, args: &[&str]) -> String {
    execute_agent_group_command_result(state, args)
        .await
        .unwrap_or_else(|error| error.to_string())
}

pub(crate) async fn execute_agent_group_command_result(
    state: Option<&Arc<AppState>>,
    args: &[&str],
) -> Result<String, AgentTextCommandError> {
    let state = state.ok_or_else(|| {
        AgentTextCommandError::Unavailable("Agent routing is not initialized.".to_string())
    })?;
    let usage = || AgentTextCommandError::InvalidInput(AGENT_GROUP_USAGE.to_string());
    let operation = args.first().copied().ok_or_else(usage)?;
    match operation {
        "list" => state
            .list_agent_groups()
            .await
            .map(format_agent_groups)
            .map_err(|error| {
                AgentTextCommandError::Unavailable(format!("Agent group list failed: {error}"))
            }),
        "create" => {
            let (Some(name), Some(leader_workspace), Some(leader_conversation)) =
                (args.get(1), args.get(2), args.get(3))
            else {
                return Err(usage());
            };
            let members = match args.get(4..).map(parse_group_members) {
                Some(Ok(members)) => members,
                Some(Err(error)) => {
                    return Err(AgentTextCommandError::InvalidInput(format!(
                        "Agent group create failed: {error}"
                    )));
                }
                None => return Err(usage()),
            };
            let leader = AgentAddress::new(
                WorkspaceId::from_raw((*leader_workspace).to_string()),
                (*leader_conversation).to_string(),
            );
            state
                .create_agent_group(*name, leader, members)
                .await
                .map(|group| format!("Agent group {} created.", group.group_id))
                .map_err(|error| {
                    AgentTextCommandError::Operation(format!("Agent group create failed: {error}"))
                })
        }
        "update" => {
            let (Some(group_id), Some(name), Some(leader_workspace), Some(leader_conversation)) =
                (args.get(1), args.get(2), args.get(3), args.get(4))
            else {
                return Err(usage());
            };
            let members = match args.get(5..).map(parse_group_members) {
                Some(Ok(members)) => members,
                Some(Err(error)) => {
                    return Err(AgentTextCommandError::InvalidInput(format!(
                        "Agent group update failed: {error}"
                    )));
                }
                None => return Err(usage()),
            };
            let leader = AgentAddress::new(
                WorkspaceId::from_raw((*leader_workspace).to_string()),
                (*leader_conversation).to_string(),
            );
            state
                .update_agent_group(*group_id, *name, leader, members)
                .await
                .map(|group| format!("Agent group {} updated.", group.group_id))
                .map_err(|error| {
                    AgentTextCommandError::Operation(format!("Agent group update failed: {error}"))
                })
        }
        "delete" => {
            let group_id = args.get(1).ok_or_else(usage)?;
            state
                .delete_agent_group(group_id)
                .await
                .map(|deleted| {
                    if deleted {
                        format!("Agent group {group_id} deleted.")
                    } else {
                        format!("Agent group {group_id} does not exist.")
                    }
                })
                .map_err(|error| {
                    AgentTextCommandError::Operation(format!("Agent group delete failed: {error}"))
                })
        }
        _ => Err(usage()),
    }
}

fn format_delivery_records(records: Vec<AgentDeliveryRecord>, message_id: Option<&str>) -> String {
    let mut matching = records
        .into_iter()
        .filter(|record| message_id.is_none_or(|id| record.message_id == id))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return message_id.map_or_else(
            || "No Agent deliveries for this conversation.".to_string(),
            |id| format!("No Agent delivery found for message {id}."),
        );
    }
    matching.reverse();
    matching
        .into_iter()
        .map(|record| {
            let mut details = vec![format!(
                "{}  {}  attempt {}  drained {}",
                record.message_id,
                record.phase.as_str(),
                record.attempt,
                record.drained
            )];
            if let Some(outcome) = record.outcome {
                details.push(format!("outcome {}", outcome.as_str()));
            }
            if let Some(turn_id) = record.turn_id {
                details.push(format!("turn {turn_id}"));
            }
            if let Some(reply_message_id) = record.reply_message_id {
                details.push(format!("reply {reply_message_id}"));
            }
            if let Some(reason) = record.reason {
                details.push(format!("reason {reason}"));
            }
            details.join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn delivery_projection_filters_and_renders_canonical_terminal_metadata() -> Result<(), String> {
        let target = AgentAddress::new(WorkspaceId::from_raw("target".to_string()), "conv");
        let message = AgentMessage::user_text(None, target.clone(), "hello");
        let message_id = message.message_id.clone();
        let timestamp = Utc::now();
        let record: AgentDeliveryRecord = serde_json::from_value(serde_json::json!({
            "message_id": message_id.clone(),
            "route": target,
            "payload": message,
            "metadata": {},
            "correlation_id": null,
            "causation_id": null,
            "phase": "turn_settled",
            "outcome": "completed",
            "drained": true,
            "reason": null,
            "persisted_at": timestamp,
            "attempt_id": "attempt-1",
            "attempt": 1,
            "claimed_at": timestamp,
            "effect_started_at": timestamp,
            "mailbox_accepted_at": timestamp,
            "drained_at": timestamp,
            "turn_settled_at": timestamp,
            "turn_id": "turn-1",
            "reply_message_id": "reply-1",
            "next_attempt_at": null,
            "terminal": true,
            "retained_bytes": 0
        }))
        .map_err(|error| format!("canonical Agent delivery fixture is invalid: {error}"))?;
        let records = vec![record];
        let rendered = format_delivery_records(records, Some(&message_id));
        assert!(rendered.contains("turn_settled"));
        assert!(rendered.contains("outcome completed"));
        assert!(rendered.contains("drained true"));
        assert!(rendered.contains("turn turn-1"));
        assert!(rendered.contains("reply reply-1"));
        for legacy in ["injection_started", "injected", "delivered"] {
            assert!(!rendered.contains(legacy));
        }
        Ok(())
    }

    #[test]
    fn group_member_parser_preserves_dynamic_roles_and_addresses() -> Result<(), String> {
        let members = parse_group_members(&[
            "researcher",
            "workspace-a",
            "conversation-a",
            "reviewer",
            "workspace-b",
            "conversation-b",
        ])?;
        assert_eq!(members.len(), 2);
        assert_eq!(
            members.first().map(|member| member.subagent_role.as_str()),
            Some("researcher")
        );
        assert_eq!(
            members
                .get(1)
                .map(|member| member.address.workspace_id.as_str()),
            Some("workspace-b")
        );
        assert!(parse_group_members(&["role", "workspace-only"]).is_err());
        Ok(())
    }
}
