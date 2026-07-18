//! Agent-facing access to the file-backed research library.

use std::path::{Path, PathBuf};

use echo_core::error::Result;
use echo_core::tools::permission::ToolPermission;
use echo_core::tools::{Tool, ToolParameters, ToolResult, ToolRiskLevel};
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{Value, json};

use crate::research::{
    CreateReviewRequest, CreateSourceRequest, ReviewRecord, UpsertEvidenceRequest, create_review,
    create_source, get_review, get_source, list_evidence, list_reviews, list_sources, save_review,
    upsert_evidence,
};

pub struct ResearchLibraryTool;

impl Tool for ResearchLibraryTool {
    fn name(&self) -> &str {
        "research_library"
    }

    fn description(&self) -> &str {
        "Read and update EKO's file-backed research library. Use it to persist normalized sources, citable evidence, systematic-review protocols, screening decisions, risk-of-bias assessments, GRADE outcomes, and PRISMA state."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_sources", "get_source", "create_source",
                        "list_evidence", "upsert_evidence",
                        "list_reviews", "get_review", "create_review", "save_review"
                    ]
                },
                "source_id": { "type": "string" },
                "review_id": { "type": "string" },
                "tag": { "type": "string" },
                "search": { "type": "string" },
                "source": { "type": "object" },
                "evidence": { "type": "object" },
                "review": { "type": "object" },
                "expected_revision": { "type": "string" }
            },
            "required": ["action"]
        })
    }

    fn risk_level(&self) -> ToolRiskLevel {
        ToolRiskLevel::Standard
    }

    fn permissions(&self) -> Vec<ToolPermission> {
        vec![ToolPermission::Read, ToolPermission::Write]
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a echo_core::tools::ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let action = match parameters.get("action").and_then(Value::as_str) {
                Some(action) => action,
                None => return Ok(ToolResult::invalid_arguments("action is required")),
            };
            let workspace_root = workspace_root(context);
            let result = match action {
                "list_sources" => list_sources(
                    &workspace_root,
                    parameters.get("tag").and_then(Value::as_str),
                    parameters.get("search").and_then(Value::as_str),
                )
                .and_then(success_json),
                "get_source" => required_string(&parameters, "source_id")
                    .and_then(|source_id| get_source(&workspace_root, source_id))
                    .and_then(success_json),
                "create_source" => parse_object::<CreateSourceRequest>(&parameters, "source")
                    .and_then(|request| create_source(&workspace_root, request))
                    .and_then(success_json),
                "list_evidence" => list_evidence(
                    &workspace_root,
                    parameters.get("source_id").and_then(Value::as_str),
                    parameters.get("review_id").and_then(Value::as_str),
                )
                .and_then(success_json),
                "upsert_evidence" => parse_object::<UpsertEvidenceRequest>(&parameters, "evidence")
                    .and_then(|request| upsert_evidence(&workspace_root, request))
                    .and_then(success_json),
                "list_reviews" => list_reviews(&workspace_root).and_then(success_json),
                "get_review" => required_string(&parameters, "review_id")
                    .and_then(|review_id| get_review(&workspace_root, review_id))
                    .and_then(success_json),
                "create_review" => parse_object::<CreateReviewRequest>(&parameters, "review")
                    .and_then(|request| create_review(&workspace_root, request))
                    .and_then(success_json),
                "save_review" => {
                    let request = parse_object::<ReviewRecord>(&parameters, "review");
                    let review_id = required_string(&parameters, "review_id");
                    let revision = required_string(&parameters, "expected_revision");
                    match (request, review_id, revision) {
                        (Ok(record), Ok(review_id), Ok(revision)) => {
                            save_review(&workspace_root, review_id, record, revision)
                                .and_then(success_json)
                        }
                        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
                    }
                }
                _ => {
                    return Ok(ToolResult::invalid_arguments(format!(
                        "unsupported research_library action: {action}"
                    )));
                }
            };
            Ok(result.unwrap_or_else(|error| ToolResult::error(error.to_string())))
        })
    }
}

fn workspace_root(context: &echo_core::tools::ToolContext) -> PathBuf {
    context
        .working_dir
        .as_ref()
        .map(|path| path.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| Path::new(".").to_path_buf())
}

fn required_string<'a>(
    parameters: &'a ToolParameters,
    field: &str,
) -> crate::research::ResearchResult<&'a str> {
    parameters
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| crate::research::ResearchError::Invalid(format!("{field} is required")))
}

fn parse_object<T>(parameters: &ToolParameters, field: &str) -> crate::research::ResearchResult<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let value = parameters.get(field).cloned().ok_or_else(|| {
        crate::research::ResearchError::Invalid(format!("{field} object is required"))
    })?;
    serde_json::from_value(value).map_err(crate::research::ResearchError::Json)
}

fn success_json<T: Serialize>(value: T) -> crate::research::ResearchResult<ToolResult> {
    serde_json::to_string_pretty(&value)
        .map(ToolResult::success)
        .map_err(crate::research::ResearchError::Json)
}
