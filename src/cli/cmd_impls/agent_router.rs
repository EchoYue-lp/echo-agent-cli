//! Cross-workspace Agent discovery, durable send, and receipt commands.

use std::sync::Arc;

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::agent_router::{
    AgentAddress, AgentDeliveryRecord, AgentDeliveryStatus, AgentGroupMember, AgentMessage,
};
use echo_agent_app_core::state::AppState;
use echo_agent_app_core::workspace::WorkspaceId;

pub async fn list_agent_endpoints(state: Option<&Arc<AppState>>) -> String {
    let Some(state) = state else {
        return "Agent routing is not initialized.".to_string();
    };
    match state.discover_agent_endpoints().await {
        Ok(endpoints) if endpoints.is_empty() => "No persisted Agent conversations.".to_string(),
        Ok(endpoints) => endpoints
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
            .join("\n"),
        Err(error) => format!("Agent discovery failed: {error}"),
    }
}

pub async fn send_agent_text(
    state: Option<&Arc<AppState>>,
    from: Option<AgentAddress>,
    workspace_id: &str,
    conversation_id: &str,
    text: &str,
) -> String {
    let Some(state) = state else {
        return "Agent routing is not initialized.".to_string();
    };
    let target = AgentAddress::new(
        WorkspaceId::from_raw(workspace_id.to_string()),
        conversation_id.to_string(),
    );
    match state
        .send_agent_message_owned(AgentMessage::user_text(from, target, text))
        .await
    {
        Ok(receipt) => format!(
            "Message {} queued for {}/{}.",
            receipt.message_id, receipt.target.workspace_id, receipt.target.conversation_id
        ),
        Err(error) => format!("Agent send failed: {error}"),
    }
}

pub async fn agent_delivery_status(
    state: Option<&Arc<AppState>>,
    workspace_id: &str,
    conversation_id: &str,
    message_id: Option<&str>,
) -> String {
    let Some(state) = state else {
        return "Agent routing is not initialized.".to_string();
    };
    let target = AgentAddress::new(
        WorkspaceId::from_raw(workspace_id.to_string()),
        conversation_id.to_string(),
    );
    match state.agent_delivery_records(&target).await {
        Ok(records) => format_delivery_records(records, message_id),
        Err(error) => format!("Agent status failed: {error}"),
    }
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

fn format_agent_groups(groups: Vec<echo_agent_app_core::agent_router::AgentGroup>) -> String {
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
    let Some(state) = state else {
        return "Agent routing is not initialized.".to_string();
    };
    let Some(operation) = args.first().copied() else {
        return AGENT_GROUP_USAGE.to_string();
    };
    match operation {
        "list" => match state.list_agent_groups().await {
            Ok(groups) => format_agent_groups(groups),
            Err(error) => format!("Agent group list failed: {error}"),
        },
        "create" => {
            let (Some(name), Some(leader_workspace), Some(leader_conversation)) =
                (args.get(1), args.get(2), args.get(3))
            else {
                return AGENT_GROUP_USAGE.to_string();
            };
            let members = match args.get(4..).map(parse_group_members) {
                Some(Ok(members)) => members,
                Some(Err(error)) => return format!("Agent group create failed: {error}"),
                None => return AGENT_GROUP_USAGE.to_string(),
            };
            let leader = AgentAddress::new(
                WorkspaceId::from_raw((*leader_workspace).to_string()),
                (*leader_conversation).to_string(),
            );
            match state.create_agent_group(*name, leader, members).await {
                Ok(group) => format!("Agent group {} created.", group.group_id),
                Err(error) => format!("Agent group create failed: {error}"),
            }
        }
        "update" => {
            let (Some(group_id), Some(name), Some(leader_workspace), Some(leader_conversation)) =
                (args.get(1), args.get(2), args.get(3), args.get(4))
            else {
                return AGENT_GROUP_USAGE.to_string();
            };
            let members = match args.get(5..).map(parse_group_members) {
                Some(Ok(members)) => members,
                Some(Err(error)) => return format!("Agent group update failed: {error}"),
                None => return AGENT_GROUP_USAGE.to_string(),
            };
            let leader = AgentAddress::new(
                WorkspaceId::from_raw((*leader_workspace).to_string()),
                (*leader_conversation).to_string(),
            );
            match state
                .update_agent_group(*group_id, *name, leader, members)
                .await
            {
                Ok(group) => format!("Agent group {} updated.", group.group_id),
                Err(error) => format!("Agent group update failed: {error}"),
            }
        }
        "delete" => {
            let Some(group_id) = args.get(1) else {
                return AGENT_GROUP_USAGE.to_string();
            };
            match state.delete_agent_group(group_id).await {
                Ok(true) => format!("Agent group {group_id} deleted."),
                Ok(false) => format!("Agent group {group_id} does not exist."),
                Err(error) => format!("Agent group delete failed: {error}"),
            }
        }
        _ => AGENT_GROUP_USAGE.to_string(),
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
            let status = match record.status {
                AgentDeliveryStatus::Queued => "queued",
                AgentDeliveryStatus::Claimed => "claimed",
                AgentDeliveryStatus::InjectionStarted => "injection_started",
                AgentDeliveryStatus::Injected => "injected",
                AgentDeliveryStatus::Delivered => "delivered",
                AgentDeliveryStatus::Failed => "failed",
            };
            let mut details = vec![format!(
                "{}  {}  attempt {}",
                record.message_id, status, record.attempt
            )];
            if let Some(turn_id) = record.turn_id {
                details.push(format!("turn {turn_id}"));
            }
            if let Some(reply_message_id) = record.reply_message_id {
                details.push(format!("reply {reply_message_id}"));
            }
            if let Some(error) = record.error {
                details.push(format!("error {error}"));
            }
            details.join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn list_agents(ctx: &CommandContext, _: &[&str]) -> CommandOutcome {
    println!("{}", list_agent_endpoints(ctx.app_state.as_ref()).await);
    CommandOutcome::Continue
}

async fn send_agent(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let (Some(workspace_id), Some(conversation_id)) = (args.first(), args.get(1)) else {
        println!("Usage: /agent-send <workspace-id> <conversation-id> <message>");
        return CommandOutcome::Continue;
    };
    let text = args.get(2..).unwrap_or(&[]).join(" ");
    if text.trim().is_empty() {
        println!("Usage: /agent-send <workspace-id> <conversation-id> <message>");
        return CommandOutcome::Continue;
    }
    let from = match ctx.app_state.as_ref() {
        Some(state) => match state
            .current_agent_address(ctx.conversation_id.as_deref())
            .await
        {
            Ok(address) => address,
            Err(error) => {
                println!("Agent source resolution failed: {error}");
                return CommandOutcome::Continue;
            }
        },
        None => None,
    };
    println!(
        "{}",
        send_agent_text(
            ctx.app_state.as_ref(),
            from,
            workspace_id,
            conversation_id,
            &text,
        )
        .await
    );
    CommandOutcome::Continue
}

async fn agent_status(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let (Some(workspace_id), Some(conversation_id)) = (args.first(), args.get(1)) else {
        println!("Usage: /agent-status <workspace-id> <conversation-id> [message-id]");
        return CommandOutcome::Continue;
    };
    println!(
        "{}",
        agent_delivery_status(
            ctx.app_state.as_ref(),
            workspace_id,
            conversation_id,
            args.get(2).copied(),
        )
        .await
    );
    CommandOutcome::Continue
}

async fn agent_group(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    println!(
        "{}",
        execute_agent_group_command(ctx.app_state.as_ref(), args).await
    );
    CommandOutcome::Continue
}

cmd!(
    AgentListCommand,
    "agent-list",
    CommandCategory::Sessions,
    "List addressable Agent conversations",
    list_agents
);
cmd!(
    AgentSendCommand,
    "agent-send",
    CommandCategory::Sessions,
    "Queue a message for another Agent conversation",
    send_agent
);
cmd!(
    AgentStatusCommand,
    "agent-status",
    CommandCategory::Sessions,
    "Show durable Agent delivery status",
    agent_status
);
cmd!(
    AgentGroupCommand,
    "agent-group",
    CommandCategory::Sessions,
    "Manage persistent cross-workspace Agent groups",
    agent_group
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(AgentListCommand));
    registry.register(Arc::new(AgentSendCommand));
    registry.register(Arc::new(AgentStatusCommand));
    registry.register(Arc::new(AgentGroupCommand));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn delivery_projection_filters_and_renders_terminal_metadata() {
        let target = AgentAddress::new(WorkspaceId::from_raw("target".to_string()), "conv");
        let message = AgentMessage::user_text(None, target.clone(), "hello");
        let message_id = message.message_id.clone();
        let records = vec![AgentDeliveryRecord {
            message,
            message_id: message_id.clone(),
            target,
            status: AgentDeliveryStatus::Delivered,
            accepted_at: Utc::now(),
            attempt_id: Some("attempt-1".to_string()),
            attempt: 1,
            settled_at: Some(Utc::now()),
            turn_id: Some("turn-1".to_string()),
            reply_message_id: Some("reply-1".to_string()),
            error: None,
            next_attempt_at: None,
        }];
        let rendered = format_delivery_records(records, Some(&message_id));
        assert!(rendered.contains("delivered"));
        assert!(rendered.contains("turn turn-1"));
        assert!(rendered.contains("reply reply-1"));
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
