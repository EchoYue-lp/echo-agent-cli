//! CLI registration for shared Agent routing commands.
use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
pub use echo_agent_app_core::api::text_commands::agent_router::{
    agent_delivery_status, execute_agent_group_command, list_agent_endpoints, send_agent_text,
};
use std::sync::Arc;

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
