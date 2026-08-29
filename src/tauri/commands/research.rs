//! File-backed research library and systematic-review IPC.

use std::path::Path;

use echo_agent_app_core::api::research::{
    CitationAuditReport, CreateReviewRequest, CreateSourceRequest, EvidenceRecord, ResearchError,
    ReviewDocument, ReviewExportArtifact, ReviewExportFormat, ReviewRecord, ReviewSummary,
    SourceRecord, UpsertEvidenceRequest,
};
use echo_agent_app_core::api::research_connectors::{
    EuropePmcEnrichmentResult, ScholarlyIngestResult, ScholarlySearchRequest, ZoteroSyncRequest,
    ZoteroSyncResult,
};

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

async fn with_research_io<T, F>(
    state: &TauriState,
    workspace_id: &str,
    workspace_generation: &str,
    operation: &'static str,
    function: F,
) -> Result<T, IpcError>
where
    T: Send + 'static,
    F: FnOnce(&Path) -> Result<T, ResearchError> + Send + 'static,
{
    let control =
        super::product_data::scoped_control(state, workspace_id, workspace_generation).await?;
    control
        .data(operation, function)
        .await
        .map_err(super::product_data::blocking_error)?
        .map_err(ipc_error)
}

fn ipc_error(error: ResearchError) -> IpcError {
    match error {
        ResearchError::NotFound(message) => IpcError::NotFound(message),
        ResearchError::Invalid(message) | ResearchError::Conflict(message) => {
            IpcError::Validation(message)
        }
        ResearchError::Io(_) | ResearchError::Json(_) | ResearchError::External(_) => {
            IpcError::Internal(error.to_string())
        }
    }
}

#[tauri::command]
pub async fn list_papers(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    tag: Option<String>,
    search: Option<String>,
) -> Result<Vec<SourceRecord>, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "list research sources",
        move |root| {
            echo_agent_app_core::api::research::list_sources(
                root,
                tag.as_deref(),
                search.as_deref(),
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn get_paper(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
) -> Result<SourceRecord, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "load research source",
        move |root| echo_agent_app_core::api::research::get_source(root, &id),
    )
    .await
}

#[tauri::command]
pub async fn create_paper(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: CreateSourceRequest,
) -> Result<SourceRecord, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "create research source",
        move |root| echo_agent_app_core::api::research::create_source(root, request),
    )
    .await
}

#[tauri::command]
pub async fn delete_paper(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let deleted = id.clone();
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "delete research source",
        move |root| echo_agent_app_core::api::research::delete_source(root, &deleted),
    )
    .await?;
    Ok(serde_json::json!({ "deleted": id }))
}

#[tauri::command]
pub async fn update_paper_notes(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
    notes: String,
) -> Result<SourceRecord, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "update research source notes",
        move |root| echo_agent_app_core::api::research::update_source_notes(root, &id, notes),
    )
    .await
}

#[tauri::command]
pub async fn add_paper_tags(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    id: String,
    tags: Vec<String>,
) -> Result<SourceRecord, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "update research source tags",
        move |root| echo_agent_app_core::api::research::add_source_tags(root, &id, tags),
    )
    .await
}

#[tauri::command]
pub async fn list_research_evidence(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    source_id: Option<String>,
    review_id: Option<String>,
) -> Result<Vec<EvidenceRecord>, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "list research evidence",
        move |root| {
            echo_agent_app_core::api::research::list_evidence(
                root,
                source_id.as_deref(),
                review_id.as_deref(),
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn upsert_research_evidence(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: UpsertEvidenceRequest,
) -> Result<EvidenceRecord, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "upsert research evidence",
        move |root| echo_agent_app_core::api::research::upsert_evidence(root, request),
    )
    .await
}

#[tauri::command]
pub async fn delete_research_evidence(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    evidence_id: String,
) -> Result<bool, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "delete research evidence",
        move |root| echo_agent_app_core::api::research::delete_evidence(root, &evidence_id),
    )
    .await?;
    Ok(true)
}

#[tauri::command]
pub async fn list_systematic_reviews(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<Vec<ReviewSummary>, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "list systematic reviews",
        echo_agent_app_core::api::research::list_reviews,
    )
    .await
}

#[tauri::command]
pub async fn create_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: CreateReviewRequest,
) -> Result<ReviewDocument, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "create systematic review",
        move |root| echo_agent_app_core::api::research::create_review(root, request),
    )
    .await
}

#[tauri::command]
pub async fn get_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    review_id: String,
) -> Result<ReviewDocument, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "load systematic review",
        move |root| echo_agent_app_core::api::research::get_review(root, &review_id),
    )
    .await
}

#[tauri::command]
pub async fn save_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    review_id: String,
    record: ReviewRecord,
    expected_revision: String,
) -> Result<ReviewDocument, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "save systematic review",
        move |root| {
            echo_agent_app_core::api::research::save_review(
                root,
                &review_id,
                record,
                &expected_revision,
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn delete_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    review_id: String,
) -> Result<bool, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "delete systematic review",
        move |root| echo_agent_app_core::api::research::delete_review(root, &review_id),
    )
    .await?;
    Ok(true)
}

#[tauri::command]
pub async fn search_scholarly_sources(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: ScholarlySearchRequest,
) -> Result<ScholarlyIngestResult, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let result =
        echo_agent_app_core::api::research_connectors::search_and_ingest_scoped(&control, request)
            .await
            .map_err(ipc_error);
    drop(control);
    result
}

#[tauri::command]
pub async fn import_zotero_library(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: ZoteroSyncRequest,
) -> Result<ZoteroSyncResult, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let result =
        echo_agent_app_core::api::research_connectors::import_zotero_scoped(&control, request)
            .await
            .map_err(ipc_error);
    drop(control);
    result
}

#[tauri::command]
pub async fn export_zotero_library(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    request: ZoteroSyncRequest,
) -> Result<ZoteroSyncResult, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let result =
        echo_agent_app_core::api::research_connectors::export_zotero_scoped(&control, request)
            .await
            .map_err(ipc_error);
    drop(control);
    result
}

#[tauri::command]
pub async fn enrich_paper_europe_pmc(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    source_id: String,
) -> Result<EuropePmcEnrichmentResult, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let result = echo_agent_app_core::api::research_connectors::enrich_from_europe_pmc_scoped(
        &control, &source_id,
    )
    .await
    .map_err(ipc_error);
    drop(control);
    result
}

#[tauri::command]
pub async fn audit_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    review_id: String,
) -> Result<CitationAuditReport, IpcError> {
    with_research_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "audit systematic review",
        move |root| echo_agent_app_core::api::research::audit_review(root, &review_id),
    )
    .await
}

#[tauri::command]
pub async fn export_systematic_review(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    review_id: String,
    format: String,
) -> Result<Vec<ReviewExportArtifact>, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    if format == "all" {
        return state
            .app_state
            .session
            .product_data_io
            .run("export all systematic review formats", move || {
                echo_agent_app_core::api::research::export_all_review_formats(
                    control.data_root(),
                    &review_id,
                )
            })
            .await
            .map_err(super::product_data::blocking_error)?
            .map_err(ipc_error);
    }
    let format = parse_export_format(&format)?;
    state
        .app_state
        .session
        .product_data_io
        .run("export systematic review", move || {
            echo_agent_app_core::api::research::export_review(
                control.data_root(),
                &review_id,
                format,
            )
            .map(|artifact| vec![artifact])
        })
        .await
        .map_err(super::product_data::blocking_error)?
        .map_err(ipc_error)
}

fn parse_export_format(value: &str) -> Result<ReviewExportFormat, IpcError> {
    match value {
        "markdown" => Ok(ReviewExportFormat::Markdown),
        "pdf" => Ok(ReviewExportFormat::Pdf),
        "docx" => Ok(ReviewExportFormat::Docx),
        "json" => Ok(ReviewExportFormat::Json),
        "csv" => Ok(ReviewExportFormat::Csv),
        "bibtex" => Ok(ReviewExportFormat::Bibtex),
        "ris" => Ok(ReviewExportFormat::Ris),
        _ => Err(IpcError::Validation(format!(
            "unsupported review export format: {value}"
        ))),
    }
}
