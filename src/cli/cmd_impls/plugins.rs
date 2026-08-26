//! Thin interactive CLI adapter for Plugin Extension commands.

use std::sync::Arc;

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};

async fn cmd_plugins(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let receipt = crate::cli::extension_surface::dispatch_extension_command(
        ctx.app_state.as_ref(),
        ctx.conversation_id.as_deref(),
        "plugins",
        &args.join(" "),
    )
    .await;
    println!("{}", receipt.display_message());
    CommandOutcome::Continue
}

cmd!(
    PluginsCommand,
    "plugins",
    ["plugin"],
    CommandCategory::Config,
    "Manage plugins and live plugin components",
    cmd_plugins
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(PluginsCommand));
}
