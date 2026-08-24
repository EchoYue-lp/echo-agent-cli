//! File-backed analysis commands shared by CLI, TUI, and channels.

use std::sync::Arc;

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, CommandRegistry, SlashCommand, SubCommandDef,
};
use echo_agent_app_core::analysis::{
    AnalysisLanguage, SaveAnalysisRequest, create_analysis, format_analysis_document,
    format_analysis_list, list_analyses, load_analysis,
};
use echo_agent_app_core::product_data_io::ScopedProductData;

const USAGE: &str = "Usage: /analysis list | create <python|r> <title> | show <analysis-id> | save <analysis-id> <request-json> | run <analysis-id> | wait <analysis-id> <owner-id> | cancel <analysis-id> | delete <analysis-id>";

pub async fn execute_analysis_command(product_data: &ScopedProductData, args: &[&str]) -> String {
    match args.first().copied() {
        None | Some("list" | "ls") => match product_data.data("list analyses", list_analyses).await
        {
            Ok(Ok(summaries)) => format_analysis_list(&summaries),
            Ok(Err(error)) => format!("Unable to list analyses: {error}"),
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
            match product_data
                .data("create analysis", move |root| {
                    create_analysis(root, &title, language)
                })
                .await
            {
                Ok(Ok(document)) => format_analysis_document(&document),
                Ok(Err(error)) => format!("Unable to create analysis: {error}"),
                Err(error) => format!("Unable to create analysis: {error}"),
            }
        }
        Some("show" | "get") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            let analysis_id = analysis_id.to_string();
            match product_data
                .data("load analysis", move |root| {
                    load_analysis(root, &analysis_id)
                })
                .await
            {
                Ok(Ok(document)) => format_analysis_document(&document),
                Ok(Err(error)) => format!("Unable to load analysis: {error}"),
                Err(error) => format!("Unable to load analysis: {error}"),
            }
        }
        Some("save") => {
            let (Some(analysis_id), Some(request_parts)) = (args.get(1), args.get(2..)) else {
                return USAGE.to_string();
            };
            let analysis_id = analysis_id.to_string();
            let request: SaveAnalysisRequest = match serde_json::from_str(&request_parts.join(" "))
            {
                Ok(request) => request,
                Err(error) => return format!("Invalid analysis save JSON: {error}"),
            };
            match product_data.save_analysis(&analysis_id, request).await {
                Ok(document) => format_analysis_document(&document),
                Err(error) => format!("Unable to save analysis: {error}"),
            }
        }
        Some("run") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            match product_data.start_analysis(analysis_id) {
                Ok(receipt) => format!(
                    "Analysis started. Wait with `/analysis wait {} {}`.",
                    receipt.analysis_id, receipt.owner_id
                ),
                Err(error) => format!("Unable to run analysis: {error}"),
            }
        }
        Some("wait") => {
            let (Some(analysis_id), Some(owner_id)) = (args.get(1), args.get(2)) else {
                return USAGE.to_string();
            };
            let receipt = echo_agent_app_core::product_data_io::AnalysisRunReceipt {
                workspace_id: product_data.workspace_id().to_string(),
                workspace_generation: product_data.generation(),
                analysis_id: analysis_id.to_string(),
                owner_id: owner_id.to_string(),
            };
            match product_data.poll_analysis(&receipt) {
                Ok(status) => serde_json::to_string(&status).unwrap_or_else(|error| {
                    format!("Unable to serialize analysis status: {error}")
                }),
                Err(error) => format!("Unable to inspect analysis: {error}"),
            }
        }
        Some("cancel") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            match product_data.cancel_analysis(analysis_id).await {
                Ok(receipt) => serde_json::to_string(&receipt)
                    .unwrap_or_else(|error| format!("Analysis joined; receipt failed: {error}")),
                Err(error) => format!("Unable to cancel analysis: {error}"),
            }
        }
        Some("delete") => {
            let Some(analysis_id) = args.get(1) else {
                return USAGE.to_string();
            };
            let analysis_id = analysis_id.to_string();
            match product_data.delete_analysis(&analysis_id).await {
                Ok(()) => "Analysis deleted.".to_string(),
                Err(error) => format!("Unable to delete analysis: {error}"),
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
    fn parses_supported_analysis_languages() {
        assert_eq!(parse_language("python"), Some(AnalysisLanguage::Python));
        assert_eq!(parse_language("PY"), Some(AnalysisLanguage::Python));
        assert_eq!(parse_language("r"), Some(AnalysisLanguage::R));
        assert_eq!(parse_language("julia"), None);
    }

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
