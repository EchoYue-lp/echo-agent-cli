//! Agent-facing access to the file-backed research library.

use std::path::Path;

use echo_agent::error::Result;
use echo_agent::tools::permission::ToolPermission;
use echo_agent::tools::research::ZoteroLibraryKind;
use echo_agent::tools::{Tool, ToolParameters, ToolResult, ToolRiskLevel};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::research::{
    CreateReviewRequest, CreateSourceRequest, ReviewExportFormat, ReviewRecord,
    UpsertEvidenceRequest, audit_review, create_review, create_source, export_all_review_formats,
    export_review, get_review, get_source, list_evidence, list_reviews, list_sources, save_review,
    upsert_evidence,
};
use crate::research_connectors::{
    ScholarlySearchRequest, ZoteroSyncRequest, enrich_from_europe_pmc, export_zotero,
    import_zotero, search_and_ingest,
};

pub struct ResearchLibraryTool {
    product_data_io: crate::product_data_io::ProductDataIoService,
    workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
}

impl ResearchLibraryTool {
    pub(crate) fn new(
        product_data_io: crate::product_data_io::ProductDataIoService,
        workspace_io_identity: crate::workspace::WorkspaceIoIdentity,
    ) -> Self {
        Self {
            product_data_io,
            workspace_io_identity,
        }
    }
}

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
                        "list_reviews", "get_review", "create_review", "save_review",
                        "search_sources", "import_zotero", "export_zotero",
                        "enrich_europe_pmc", "audit_review", "export_review"
                    ]
                },
                "source_id": { "type": "string" },
                "review_id": { "type": "string" },
                "tag": { "type": "string" },
                "search": { "type": "string" },
                "source": { "type": "object" },
                "evidence": { "type": "object" },
                "review": { "type": "object" },
                "search_request": { "type": "object" },
                "zotero_request": { "type": "object" },
                "format": { "type": "string", "enum": ["markdown", "pdf", "docx", "json", "csv", "bibtex", "ris", "all"] },
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
        context: &'a echo_agent::tools::ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let action = match parameters.get("action").and_then(Value::as_str) {
                Some(action) => action.to_string(),
                None => return Ok(ToolResult::invalid_arguments("action is required")),
            };
            let Some(workspace_io) =
                crate::state::WorkspaceIoInvocation::from_tool_context_for_identity(
                    context,
                    &self.workspace_io_identity,
                )
            else {
                return Ok(ToolResult::error(
                    "research_library requires the exact EKO workspace lifetime receipt",
                ));
            };
            let workspace_root = workspace_io.data_root().to_path_buf();
            let resource_guards = workspace_io.resource_guards();
            let flow = self
                .product_data_io
                .begin_owned_flow("execute research library operation")
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
            let result = match action.as_str() {
                "search_sources" => {
                    match parse_object::<ScholarlySearchRequest>(&parameters, "search_request") {
                        Ok(request) => {
                            search_and_ingest(&flow, &resource_guards, &workspace_root, request)
                                .await
                                .and_then(success_json)
                        }
                        Err(error) => Err(error),
                    }
                }
                "import_zotero" => {
                    match parse_object::<ZoteroToolRequest>(&parameters, "zotero_request") {
                        Ok(request) => match request.into_sync_request() {
                            Ok(request) => {
                                import_zotero(&flow, &resource_guards, &workspace_root, request)
                                    .await
                                    .and_then(success_json)
                            }
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    }
                }
                "export_zotero" => {
                    match parse_object::<ZoteroToolRequest>(&parameters, "zotero_request") {
                        Ok(request) => match request.into_sync_request() {
                            Ok(request) => {
                                export_zotero(&flow, &resource_guards, &workspace_root, request)
                                    .await
                                    .and_then(success_json)
                            }
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    }
                }
                "enrich_europe_pmc" => match required_string(&parameters, "source_id") {
                    Ok(source_id) => {
                        enrich_from_europe_pmc(&flow, &resource_guards, &workspace_root, source_id)
                            .await
                            .and_then(success_json)
                    }
                    Err(error) => Err(error),
                },
                _ => {
                    let action = action.clone();
                    let resource_guards = resource_guards.clone();
                    match flow
                        .run("execute research library file action", move || {
                            let _resource_guards = resource_guards;
                            execute_file_action(&workspace_root, &action, &parameters)
                        })
                        .await
                    {
                        Ok(result) => result,
                        Err(error) => {
                            Err(crate::research::ResearchError::External(error.to_string()))
                        }
                    }
                }
            };
            let failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_durable_settlement_debt())
                .map(ToString::to_string);
            let result = result.unwrap_or_else(|error| ToolResult::error(error.to_string()));
            flow.settle(failure);
            Ok(result)
        })
    }
}

fn execute_file_action(
    workspace_root: &Path,
    action: &str,
    parameters: &ToolParameters,
) -> crate::research::ResearchResult<ToolResult> {
    match action {
        "list_sources" => list_sources(
            workspace_root,
            parameters.get("tag").and_then(Value::as_str),
            parameters.get("search").and_then(Value::as_str),
        )
        .and_then(success_json),
        "get_source" => required_string(parameters, "source_id")
            .and_then(|source_id| get_source(workspace_root, source_id))
            .and_then(success_json),
        "create_source" => parse_object::<CreateSourceRequest>(parameters, "source")
            .and_then(|request| create_source(workspace_root, request))
            .and_then(success_json),
        "list_evidence" => list_evidence(
            workspace_root,
            parameters.get("source_id").and_then(Value::as_str),
            parameters.get("review_id").and_then(Value::as_str),
        )
        .and_then(success_json),
        "upsert_evidence" => parse_object::<UpsertEvidenceRequest>(parameters, "evidence")
            .and_then(|request| upsert_evidence(workspace_root, request))
            .and_then(success_json),
        "list_reviews" => list_reviews(workspace_root).and_then(success_json),
        "get_review" => required_string(parameters, "review_id")
            .and_then(|review_id| get_review(workspace_root, review_id))
            .and_then(success_json),
        "create_review" => parse_object::<CreateReviewRequest>(parameters, "review")
            .and_then(|request| create_review(workspace_root, request))
            .and_then(success_json),
        "save_review" => {
            let request = parse_object::<ReviewRecord>(parameters, "review");
            let review_id = required_string(parameters, "review_id");
            let revision = required_string(parameters, "expected_revision");
            match (request, review_id, revision) {
                (Ok(record), Ok(review_id), Ok(revision)) => {
                    save_review(workspace_root, review_id, record, revision).and_then(success_json)
                }
                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
            }
        }
        "audit_review" => required_string(parameters, "review_id")
            .and_then(|review_id| audit_review(workspace_root, review_id))
            .and_then(success_json),
        "export_review" => {
            let review_id = required_string(parameters, "review_id");
            let format = parameters
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("markdown");
            review_id.and_then(|review_id| {
                if format == "all" {
                    export_all_review_formats(workspace_root, review_id).and_then(success_json)
                } else {
                    parse_export_format(format)
                        .and_then(|format| export_review(workspace_root, review_id, format))
                        .and_then(success_json)
                }
            })
        }
        _ => Ok(ToolResult::invalid_arguments(format!(
            "unsupported research_library action: {action}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct ZoteroToolRequest {
    library_kind: ZoteroLibraryKind,
    library_id: String,
    limit: Option<usize>,
    #[serde(default)]
    source_ids: Vec<String>,
}

impl ZoteroToolRequest {
    fn into_sync_request(self) -> crate::research::ResearchResult<ZoteroSyncRequest> {
        let api_key = std::env::var("ZOTERO_API_KEY").map_err(|_| {
            crate::research::ResearchError::Invalid(
                "ZOTERO_API_KEY is required for Agent-driven Zotero sync".to_string(),
            )
        })?;
        Ok(ZoteroSyncRequest {
            library_kind: self.library_kind,
            library_id: self.library_id,
            api_key,
            limit: self.limit,
            source_ids: self.source_ids,
        })
    }
}

fn parse_export_format(value: &str) -> crate::research::ResearchResult<ReviewExportFormat> {
    match value {
        "markdown" => Ok(ReviewExportFormat::Markdown),
        "pdf" => Ok(ReviewExportFormat::Pdf),
        "docx" => Ok(ReviewExportFormat::Docx),
        "json" => Ok(ReviewExportFormat::Json),
        "csv" => Ok(ReviewExportFormat::Csv),
        "bibtex" => Ok(ReviewExportFormat::Bibtex),
        "ris" => Ok(ReviewExportFormat::Ris),
        _ => Err(crate::research::ResearchError::Invalid(format!(
            "unsupported review export format: {value}"
        ))),
    }
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
