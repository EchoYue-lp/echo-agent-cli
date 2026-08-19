use std::sync::Arc;

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, CommandRegistry, SlashCommand, SubCommandDef,
};

pub struct WorkflowCommand;

impl SlashCommand for WorkflowCommand {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn description(&self) -> &'static str {
        "Manage and execute durable Graph workflows"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Advanced
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "list",
                aliases: &["ls"],
                description: "List workflows",
            },
            SubCommandDef {
                name: "show",
                aliases: &["get"],
                description: "Show one workflow",
            },
            SubCommandDef {
                name: "create",
                aliases: &[],
                description: "Create a validated workflow from a definition or @path",
            },
            SubCommandDef {
                name: "delete",
                aliases: &["rm"],
                description: "Delete a workflow",
            },
            SubCommandDef {
                name: "run",
                aliases: &["execute"],
                description: "Execute a workflow with optional JSON input",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            let output = match ctx.app_state.as_ref() {
                Some(state) => state
                    .history
                    .workflows
                    .execute_command(&args.join(" "))
                    .await
                    .unwrap_or_else(|error| format!("Workflow command failed: {error}")),
                None => "Workflow service is unavailable.".to_string(),
            };
            println!("{output}");
            CommandOutcome::Continue
        })
    }
}

pub fn register_all(registry: &mut CommandRegistry) {
    registry.register(Arc::new(WorkflowCommand));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_command_is_registered_with_product_crud_actions() {
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let command = registry.get("workflow");
        assert!(command.is_some());
        let actions = command
            .map(|command| command.subcommands())
            .unwrap_or_default();
        assert!(actions.iter().any(|action| action.name == "create"));
        assert!(actions.iter().any(|action| action.name == "run"));
    }
}
