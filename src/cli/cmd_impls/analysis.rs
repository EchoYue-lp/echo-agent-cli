//! File-backed analysis commands shared by CLI, TUI, and channels.

use std::sync::Arc;

use crate::agent_handle::AgentHandle;
use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, CommandRegistry, SlashCommand, SubCommandDef,
};
use echo_agent_app_core::analysis::{
    AnalysisLanguage, create_analysis, format_analysis_document, format_analysis_list,
    list_analyses, load_analysis, run_analysis_with_agent, workspace_root_for_agent,
};

const USAGE: &str =
    "Usage: /analysis list | create <python|r> <title> | show <analysis-id> | run <analysis-id>";

pub async fn execute_analysis_command(agent: &AgentHandle, args: &[&str]) -> String {
    let workspace_root = workspace_root_for_agent(agent).await;
    match args.first().copied() {
        None | Some("list" | "ls") => match list_analyses(&workspace_root) {
            Ok(summaries) => format_analysis_list(&summaries),
            Err(error) => format!("Unable to list analyses: {error}"),
        },
        Some("create" | "new") => {
            let Some(language) = args.get(1).and_then(|value| parse_language(value)) else {
                return USAGE.to_string();
            };
            let title = args.get(2..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return USAGE.to_string();
            }
            match create_analysis(&workspace_root, &title, language) {
                Ok(document) => format_analysis_document(&document),
                Err(error) => format!("Unable to create analysis: {error}"),
            }
        }
        Some("show" | "get") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            match load_analysis(&workspace_root, analysis_id) {
                Ok(document) => format_analysis_document(&document),
                Err(error) => format!("Unable to load analysis: {error}"),
            }
        }
        Some("run") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            match run_analysis_with_agent(agent, &workspace_root, analysis_id, None).await {
                Ok(document) => format_analysis_document(&document),
                Err(error) => format!("Unable to run analysis: {error}"),
            }
        }
        Some("help" | "--help" | "-h") => USAGE.to_string(),
        Some(other) => format!("Unknown analysis subcommand: {other}\n{USAGE}"),
    }
}

fn parse_language(value: &str) -> Option<AnalysisLanguage> {
    match value.to_ascii_lowercase().as_str() {
        "python" | "py" => Some(AnalysisLanguage::Python),
        "r" => Some(AnalysisLanguage::R),
        _ => None,
    }
}

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
                name: "run",
                aliases: &[],
                description: "Run the persisted analysis script",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            println!("{}", execute_analysis_command(&ctx.agent, args).await);
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
    fn parses_supported_analysis_languages() {
        assert_eq!(parse_language("python"), Some(AnalysisLanguage::Python));
        assert_eq!(parse_language("PY"), Some(AnalysisLanguage::Python));
        assert_eq!(parse_language("r"), Some(AnalysisLanguage::R));
        assert_eq!(parse_language("julia"), None);
    }
}
