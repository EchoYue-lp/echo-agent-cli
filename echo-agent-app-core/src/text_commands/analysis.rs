//! Existing analysis text adapter over ScopedProductData.
use super::ManagementCommandError;
use crate::analysis::{
    AnalysisLanguage, SaveAnalysisRequest, create_analysis, format_analysis_document,
    format_analysis_list, list_analyses, load_analysis,
};
use crate::product_data_io::ScopedProductData;

const USAGE: &str = "Usage: /analysis list | create <python|r> <title> | show <analysis-id> | save <analysis-id> <request-json> | run <analysis-id> | wait <analysis-id> <owner-id> | cancel <analysis-id> | delete <analysis-id>";

pub async fn execute_analysis_command(product_data: &ScopedProductData, args: &[&str]) -> String {
    execute_analysis_command_result(product_data, args)
        .await
        .unwrap_or_else(|error| error.to_string())
}

/// The same original command executor supplies typed facts before display.
pub(crate) async fn execute_analysis_command_result(
    product_data: &ScopedProductData,
    args: &[&str],
) -> Result<String, ManagementCommandError> {
    Ok(match args.first().copied() {
        None | Some("list" | "ls") => match product_data.data("list analyses", list_analyses).await
        {
            Ok(Ok(summaries)) => format_analysis_list(&summaries),
            Ok(Err(error)) => {
                return Err(ManagementCommandError::native(error, false)
                    .context("Unable to list analyses: "));
            }
            Err(error) => {
                return Err(ManagementCommandError::product_io(error, false)
                    .context("Unable to list analyses: "));
            }
        },
        Some("create" | "new") => {
            let Some(language) = args.get(1).and_then(|value| parse_language(value)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let title = args.get(2..).unwrap_or(&[]).join(" ");
            if title.trim().is_empty() {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            }
            match product_data
                .data("create analysis", move |root| {
                    create_analysis(root, &title, language)
                })
                .await
            {
                Ok(Ok(document)) => format_analysis_document(&document),
                Ok(Err(error)) => {
                    return Err(ManagementCommandError::native(error, true)
                        .context("Unable to create analysis: "));
                }
                Err(error) => {
                    return Err(ManagementCommandError::product_io(error, true)
                        .context("Unable to create analysis: "));
                }
            }
        }
        Some("show" | "get") => {
            let Some(analysis_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let analysis_id = analysis_id.to_string();
            match product_data
                .data("load analysis", move |root| {
                    load_analysis(root, &analysis_id)
                })
                .await
            {
                Ok(Ok(document)) => format_analysis_document(&document),
                Ok(Err(error)) => {
                    return Err(ManagementCommandError::native(error, false)
                        .context("Unable to load analysis: "));
                }
                Err(error) => {
                    return Err(ManagementCommandError::product_io(error, false)
                        .context("Unable to load analysis: "));
                }
            }
        }
        Some("save") => {
            let (Some(analysis_id), Some(request_parts)) = (args.get(1), args.get(2..)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let analysis_id = analysis_id.to_string();
            let request: SaveAnalysisRequest = match serde_json::from_str(&request_parts.join(" "))
            {
                Ok(request) => request,
                Err(error) => {
                    return Err(ManagementCommandError::InvalidInput(format!(
                        "Invalid analysis save JSON: {error}"
                    )));
                }
            };
            match product_data.save_analysis(&analysis_id, request).await {
                Ok(document) => format_analysis_document(&document),
                Err(error) => {
                    return Err(ManagementCommandError::analysis_control(error)
                        .context("Unable to save analysis: "));
                }
            }
        }
        Some("run") => {
            let Some(analysis_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            match product_data.start_analysis(analysis_id) {
                Ok(receipt) => format!(
                    "Analysis started. Wait with `/analysis wait {} {}`.",
                    receipt.analysis_id, receipt.owner_id
                ),
                Err(error) => {
                    return Err(ManagementCommandError::analysis_control(error)
                        .context("Unable to run analysis: "));
                }
            }
        }
        Some("wait") => {
            let (Some(analysis_id), Some(owner_id)) = (args.get(1), args.get(2)) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let receipt = crate::product_data_io::AnalysisRunReceipt {
                workspace_id: product_data.workspace_id().to_string(),
                workspace_generation: product_data.generation(),
                analysis_id: analysis_id.to_string(),
                owner_id: owner_id.to_string(),
            };
            match product_data.poll_analysis(&receipt) {
                Ok(status) => serde_json::to_string(&status).unwrap_or_else(|error| {
                    format!("Unable to serialize analysis status: {error}")
                }),
                Err(error) => {
                    return Err(ManagementCommandError::analysis_control(error)
                        .context("Unable to inspect analysis: "));
                }
            }
        }
        Some("cancel") => {
            let Some(analysis_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            match product_data.cancel_analysis(analysis_id).await {
                Ok(receipt) => {
                    let joined = receipt.cleanup_joined();
                    let rendered = serde_json::to_string(&receipt).unwrap_or_else(|error| {
                        format!("Analysis joined; receipt failed: {error}")
                    });
                    if !joined {
                        return Err(ManagementCommandError::Unknown(rendered));
                    }
                    rendered
                }
                Err(error) => {
                    return Err(ManagementCommandError::analysis_control(error)
                        .context("Unable to cancel analysis: "));
                }
            }
        }
        Some("delete") => {
            let Some(analysis_id) = args.get(1) else {
                return Err(ManagementCommandError::InvalidInput(USAGE.to_string()));
            };
            let analysis_id = analysis_id.to_string();
            match product_data.delete_analysis(&analysis_id).await {
                Ok(()) => "Analysis deleted.".to_string(),
                Err(error) => {
                    return Err(ManagementCommandError::analysis_control(error)
                        .context("Unable to delete analysis: "));
                }
            }
        }
        Some("help" | "--help" | "-h") => USAGE.to_string(),
        Some(other) => {
            return Err(ManagementCommandError::InvalidInput(format!(
                "Unknown analysis subcommand: {other}\n{USAGE}"
            )));
        }
    })
}

fn parse_language(value: &str) -> Option<AnalysisLanguage> {
    match value.to_ascii_lowercase().as_str() {
        "python" | "py" => Some(AnalysisLanguage::Python),
        "r" => Some(AnalysisLanguage::R),
        _ => None,
    }
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
