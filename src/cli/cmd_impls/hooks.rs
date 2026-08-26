//! Thin interactive CLI adapter for Hook Extension commands.

use std::sync::Arc;

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};

async fn cmd_hooks(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    let receipt = crate::cli::extension_surface::dispatch_extension_command(
        ctx.app_state.as_ref(),
        ctx.conversation_id.as_deref(),
        "hooks",
        &args.join(" "),
    )
    .await;
    println!("{}", receipt.display_message());
    CommandOutcome::Continue
}

cmd!(
    HooksCommand,
    "hooks",
    ["hk"],
    CommandCategory::Config,
    "Manage hooks (list/reload/test)",
    cmd_hooks
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(Arc::new(HooksCommand));
}
