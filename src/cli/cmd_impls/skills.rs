//! Thin interactive CLI adapters for Skill and MCP Extension commands.

use std::sync::Arc;

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};

async fn execute(ctx: &CommandContext, root: &str, args: &[&str]) -> CommandOutcome {
    let receipt = crate::cli::extension_surface::dispatch_extension_command(
        ctx.app_state.as_ref(),
        ctx.conversation_id.as_deref(),
        root,
        &args.join(" "),
    )
    .await;
    println!("{}", receipt.display_message());
    CommandOutcome::Continue
}

async fn cmd_skills(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    execute(ctx, "skills", args).await
}

cmd!(
    SkillsCommand,
    "skills",
    ["sk", "skill"],
    CommandCategory::Info,
    "List and manage skills, including explicit upstream checks and sync",
    cmd_skills
);

async fn cmd_mcp(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    execute(ctx, "mcp", args).await
}

cmd!(
    McpCommand,
    "mcp",
    ["m"],
    CommandCategory::Info,
    "Manage MCP server configuration and connections",
    cmd_mcp
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(SkillsCommand));
    registry.register(Arc::new(McpCommand));
}
