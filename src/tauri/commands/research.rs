//! File-backed research library and systematic-review IPC.

use std::path::PathBuf;

use echo_agent_app_core::research::{
    CitationAuditReport, CreateReviewRequest, CreateSourceRequest, EvidenceRecord, ResearchError,
    ReviewDocument, ReviewExportArtifact, ReviewExportFormat, ReviewRecord, ReviewSummary,
    SourceRecord, UpsertEvidenceRequest,
};
use echo_agent_app_core::research_connectors::{
    EuropePmcEnrichmentResult, ScholarlyIngestResult, ScholarlySearchRequest, ZoteroSyncRequest,
    ZoteroSyncResult,
};

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

async fn workspace_root(state: &TauriState) -> PathBuf {
    match state.app_state.current_workspace().await {
        Some(workspace) => workspace.root,
        None => {
            let agent = state.app_state.connection.primary_agent();
            echo_agent_app_core::analysis::workspace_root_for_agent(&agent).await
        }
    }
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
    tag: Option<String>,
    search: Option<String>,
) -> Result<Vec<SourceRecord>, IpcError> {
    echo_agent_app_core::research::list_sources(
        &workspace_root(&state).await,
        tag.as_deref(),
        search.as_deref(),
    )
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn get_paper(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<SourceRecord, IpcError> {
    echo_agent_app_core::research::get_source(&workspace_root(&state).await, &id).map_err(ipc_error)
}

#[tauri::command]
pub async fn create_paper(
    state: tauri::State<'_, TauriState>,
    request: CreateSourceRequest,
) -> Result<SourceRecord, IpcError> {
    echo_agent_app_core::research::create_source(&workspace_root(&state).await, request)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn delete_paper(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    echo_agent_app_core::research::delete_source(&workspace_root(&state).await, &id)
        .map_err(ipc_error)?;
    Ok(serde_json::json!({ "deleted": id }))
}

#[tauri::command]
pub async fn update_paper_notes(
    state: tauri::State<'_, TauriState>,
    id: String,
    notes: String,
) -> Result<SourceRecord, IpcError> {
    echo_agent_app_core::research::update_source_notes(&workspace_root(&state).await, &id, notes)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn add_paper_tags(
    state: tauri::State<'_, TauriState>,
    id: String,
    tags: Vec<String>,
) -> Result<SourceRecord, IpcError> {
    echo_agent_app_core::research::add_source_tags(&workspace_root(&state).await, &id, tags)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn list_research_evidence(
    state: tauri::State<'_, TauriState>,
    source_id: Option<String>,
    review_id: Option<String>,
) -> Result<Vec<EvidenceRecord>, IpcError> {
    echo_agent_app_core::research::list_evidence(
        &workspace_root(&state).await,
        source_id.as_deref(),
        review_id.as_deref(),
    )
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn upsert_research_evidence(
    state: tauri::State<'_, TauriState>,
    request: UpsertEvidenceRequest,
) -> Result<EvidenceRecord, IpcError> {
    echo_agent_app_core::research::upsert_evidence(&workspace_root(&state).await, request)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn delete_research_evidence(
    state: tauri::State<'_, TauriState>,
    evidence_id: String,
) -> Result<bool, IpcError> {
    echo_agent_app_core::research::delete_evidence(&workspace_root(&state).await, &evidence_id)
        .map_err(ipc_error)?;
    Ok(true)
}

#[tauri::command]
pub async fn list_systematic_reviews(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<ReviewSummary>, IpcError> {
    echo_agent_app_core::research::list_reviews(&workspace_root(&state).await).map_err(ipc_error)
}

#[tauri::command]
pub async fn create_systematic_review(
    state: tauri::State<'_, TauriState>,
    request: CreateReviewRequest,
) -> Result<ReviewDocument, IpcError> {
    echo_agent_app_core::research::create_review(&workspace_root(&state).await, request)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn get_systematic_review(
    state: tauri::State<'_, TauriState>,
    review_id: String,
) -> Result<ReviewDocument, IpcError> {
    echo_agent_app_core::research::get_review(&workspace_root(&state).await, &review_id)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn save_systematic_review(
    state: tauri::State<'_, TauriState>,
    review_id: String,
    record: ReviewRecord,
    expected_revision: String,
) -> Result<ReviewDocument, IpcError> {
    echo_agent_app_core::research::save_review(
        &workspace_root(&state).await,
        &review_id,
        record,
        &expected_revision,
    )
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn delete_systematic_review(
    state: tauri::State<'_, TauriState>,
    review_id: String,
) -> Result<bool, IpcError> {
    echo_agent_app_core::research::delete_review(&workspace_root(&state).await, &review_id)
        .map_err(ipc_error)?;
    Ok(true)
}

#[tauri::command]
pub async fn search_scholarly_sources(
    state: tauri::State<'_, TauriState>,
    request: ScholarlySearchRequest,
) -> Result<ScholarlyIngestResult, IpcError> {
    echo_agent_app_core::research_connectors::search_and_ingest(
        &workspace_root(&state).await,
        request,
    )
    .await
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn import_zotero_library(
    state: tauri::State<'_, TauriState>,
    request: ZoteroSyncRequest,
) -> Result<ZoteroSyncResult, IpcError> {
    echo_agent_app_core::research_connectors::import_zotero(&workspace_root(&state).await, request)
        .await
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn export_zotero_library(
    state: tauri::State<'_, TauriState>,
    request: ZoteroSyncRequest,
) -> Result<ZoteroSyncResult, IpcError> {
    echo_agent_app_core::research_connectors::export_zotero(&workspace_root(&state).await, request)
        .await
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn enrich_paper_europe_pmc(
    state: tauri::State<'_, TauriState>,
    source_id: String,
) -> Result<EuropePmcEnrichmentResult, IpcError> {
    echo_agent_app_core::research_connectors::enrich_from_europe_pmc(
        &workspace_root(&state).await,
        &source_id,
    )
    .await
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn audit_systematic_review(
    state: tauri::State<'_, TauriState>,
    review_id: String,
) -> Result<CitationAuditReport, IpcError> {
    echo_agent_app_core::research::audit_review(&workspace_root(&state).await, &review_id)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn export_systematic_review(
    state: tauri::State<'_, TauriState>,
    review_id: String,
    format: String,
) -> Result<Vec<ReviewExportArtifact>, IpcError> {
    let root = workspace_root(&state).await;
    if format == "all" {
        return echo_agent_app_core::research::export_all_review_formats(&root, &review_id)
            .map_err(ipc_error);
    }
    let format = parse_export_format(&format)?;
    echo_agent_app_core::research::export_review(&root, &review_id, format)
        .map(|artifact| vec![artifact])
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
