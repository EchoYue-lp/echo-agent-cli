//! File-backed analysis commands shared by CLI, TUI, and channels.

use std::sync::Arc;

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, CommandRegistry, SlashCommand, SubCommandDef,
};
pub use echo_agent_app_core::api::text_commands::analysis::execute_analysis_command;

pub struct AnalysisCommand;

impl SlashCommand for AnalysisCommand {
    fn name(&self) -> &'static str {
        "analysis"
    }

    fn description(&self) -> &'static str {
        "Create, inspect, and run file-backed analyses"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Coding
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "list",
                aliases: &["ls"],
                description: "List analyses in the current workspace",
            },
            SubCommandDef {
                name: "create",
                aliases: &["new"],
                description: "Create a Python or R analysis",
            },
            SubCommandDef {
                name: "show",
                aliases: &["get"],
                description: "Show analysis lineage and latest result",
            },
            SubCommandDef {
                name: "save",
                aliases: &[],
                description: "Save a script with optimistic concurrency",
            },
            SubCommandDef {
                name: "run",
                aliases: &[],
                description: "Run the persisted analysis script",
            },
            SubCommandDef {
                name: "wait",
                aliases: &[],
                description: "Join the exact started analysis receipt",
            },
            SubCommandDef {
                name: "cancel",
                aliases: &[],
                description: "Cancel a running analysis",
            },
            SubCommandDef {
                name: "delete",
                aliases: &[],
                description: "Delete a file-backed analysis",
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
                Some(state) => match state.current_product_data().await {
                    Ok(product_data) => execute_analysis_command(&product_data, args).await,
                    Err(error) => format!("Analysis workspace is unavailable: {error}"),
                },
                None => "Analysis workspace is unavailable.".to_string(),
            };
            println!("{output}");
            CommandOutcome::Continue
        })
    }
}

pub fn register_all(registry: &mut CommandRegistry) {
    registry.register(Arc::new(AnalysisCommand));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_analysis_catalog_exposes_save_cancel_and_delete() {
        let names = AnalysisCommand
            .subcommands()
            .into_iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"save"));
        assert!(names.contains(&"wait"));
        assert!(names.contains(&"cancel"));
        assert!(names.contains(&"delete"));
    }
}
