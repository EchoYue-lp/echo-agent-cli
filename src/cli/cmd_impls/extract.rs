use std::sync::Arc;

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, CommandRegistry, SlashCommand, SubCommandDef,
};

pub struct ExtractCommand;

impl SlashCommand for ExtractCommand {
    fn name(&self) -> &'static str {
        "extract"
    }

    fn description(&self) -> &'static str {
        "Extract typed JSON through the current conversation Agent"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Advanced
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "examples",
                aliases: &["example"],
                description: "List structured extraction examples",
            },
            SubCommandDef {
                name: "validate",
                aliases: &[],
                description: "Validate a JSON schema or @path",
            },
            SubCommandDef {
                name: "run",
                aliases: &["extract"],
                description: "Extract typed JSON from input",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            let output = match (ctx.app_state.as_ref(), ctx.conversation_id.as_deref()) {
                (Some(state), Some(conversation_id)) => {
                    let scope = state.current_execution_scope().await;
                    state
                        .execute_structured_extraction_command_for_scope(
                            scope.workspace_id(),
                            conversation_id,
                            echo_agent_app_core::api::foreground_turn::ForegroundTurnSurface::Cli,
                            &args.join(" "),
                        )
                        .await
                        .unwrap_or_else(|error| {
                            format!("Structured extraction command failed: {error}")
                        })
                }
                _ => "Structured extraction service is unavailable.".to_string(),
            };
            println!("{output}");
            CommandOutcome::Continue
        })
    }
}

pub fn register_all(registry: &mut CommandRegistry) {
    registry.register(Arc::new(ExtractCommand));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_command_is_registered_with_shared_actions() {
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let command = registry.get("extract");
        assert!(command.is_some());
        let actions = command
            .map(|command| command.subcommands())
            .unwrap_or_default();
        assert!(actions.iter().any(|action| action.name == "examples"));
        assert!(actions.iter().any(|action| action.name == "validate"));
        assert!(actions.iter().any(|action| action.name == "run"));
    }
}
