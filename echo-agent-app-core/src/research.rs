//! File-backed research library and systematic-review workbench.
//!
//! Research artifacts are ordinary workspace files under `research/` so they
//! remain inspectable, versionable, and usable by GUI, TUI, CLI, and agents.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const RESEARCH_ROOT: &str = "research";
const SOURCES_DIR: &str = "sources";
const EVIDENCE_DIR: &str = "evidence";
const REVIEWS_DIR: &str = "reviews";
const FULL_TEXT_DIR: &str = "fulltext";
const REPORTS_DIR: &str = "reports";
const REVIEW_FILE: &str = "review.json";
const CONTRACT_VERSION: u32 = 1;
const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ResearchError {
    #[error("invalid research input: {0}")]
    Invalid(String),
    #[error("research record not found: {0}")]
    NotFound(String),
    #[error("research record conflict: {0}")]
    Conflict(String),
    #[error("research I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("research JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("research integration failed: {0}")]
    External(String),
}

impl ResearchError {
    pub(crate) fn is_durable_settlement_debt(&self) -> bool {
        matches!(self, Self::Io(_) | Self::Json(_) | Self::External(_))
    }
}

pub type ResearchResult<T> = Result<T, ResearchError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    JournalArticle,
    Preprint,
    ConferencePaper,
    Book,
    Dataset,
    Guideline,
    TrialRegistration,
    Web,
    #[default]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub provider: String,
    pub query: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub record_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub contract_version: u32,
    pub id: String,
    pub source_kind: SourceKind,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
    pub pmid: Option<String>,
    pub pmcid: Option<String>,
    pub arxiv_id: Option<String>,
    pub openalex_id: Option<String>,
    #[serde(default)]
    pub zotero_key: Option<String>,
    #[serde(default)]
    pub clinical_trial_id: Option<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub url: Option<String>,
    pub pdf_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub provenance: Vec<SourceProvenance>,
    #[serde(default)]
    pub europe_pmc: Option<EuropePmcSupplement>,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateSourceRequest {
    pub source_kind: Option<SourceKind>,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
    pub pmid: Option<String>,
    pub pmcid: Option<String>,
    pub arxiv_id: Option<String>,
    pub openalex_id: Option<String>,
    pub zotero_key: Option<String>,
    pub clinical_trial_id: Option<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub url: Option<String>,
    pub pdf_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub provenance: Vec<SourceProvenance>,
    pub europe_pmc: Option<EuropePmcSupplement>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EuropePmcSupplement {
    #[serde(default)]
    pub citation_ids: Vec<String>,
    #[serde(default)]
    pub reference_ids: Vec<String>,
    #[serde(default)]
    pub biomedical_entities: Vec<BiomedicalEntity>,
    pub full_text_path: Option<String>,
    pub enriched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub enrichment_attempts: Vec<EuropePmcEnrichmentAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EuropePmcEnrichmentAttempt {
    pub attempt_id: String,
    pub provider: String,
    pub attempted_at: DateTime<Utc>,
    #[serde(default)]
    pub successful_fields: Vec<String>,
    #[serde(default)]
    pub failed_fields: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EuropePmcSupplementUpdate {
    pub citation_ids: Option<Vec<String>>,
    pub reference_ids: Option<Vec<String>>,
    pub biomedical_entities: Option<Vec<BiomedicalEntity>>,
    pub full_text_path: Option<Option<String>>,
    pub attempt: Option<EuropePmcEnrichmentAttempt>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BiomedicalEntity {
    pub name: String,
    pub semantic_type: Option<String>,
    pub frequency: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceIngestResult {
    pub source: SourceRecord,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub contract_version: u32,
    pub id: String,
    pub source_id: String,
    pub review_id: Option<String>,
    pub dimension: String,
    pub claim: String,
    pub excerpt: Option<String>,
    pub locator: Option<String>,
    pub evidence_type: Option<String>,
    pub population: Option<String>,
    pub intervention: Option<String>,
    pub comparator: Option<String>,
    pub outcome: Option<String>,
    pub effect: Option<String>,
    pub limitations: Option<String>,
    pub certainty: Option<String>,
    #[serde(default)]
    pub harms: Vec<String>,
    #[serde(default)]
    pub contraindications: Vec<String>,
    #[serde(default)]
    pub conflicts_of_interest: Vec<String>,
    #[serde(default)]
    pub guideline_conflicts: Vec<String>,
    #[serde(default)]
    pub extrapolation_limits: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpsertEvidenceRequest {
    pub id: Option<String>,
    pub source_id: String,
    pub review_id: Option<String>,
    pub dimension: String,
    pub claim: String,
    pub excerpt: Option<String>,
    pub locator: Option<String>,
    pub evidence_type: Option<String>,
    pub population: Option<String>,
    pub intervention: Option<String>,
    pub comparator: Option<String>,
    pub outcome: Option<String>,
    pub effect: Option<String>,
    pub limitations: Option<String>,
    pub certainty: Option<String>,
    #[serde(default)]
    pub harms: Vec<String>,
    #[serde(default)]
    pub contraindications: Vec<String>,
    #[serde(default)]
    pub conflicts_of_interest: Vec<String>,
    #[serde(default)]
    pub guideline_conflicts: Vec<String>,
    #[serde(default)]
    pub extrapolation_limits: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDomain {
    #[default]
    Academic,
    Medical,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PicoFramework {
    pub population: String,
    pub intervention: String,
    pub comparator: String,
    #[serde(default)]
    pub outcomes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PecoFramework {
    pub population: String,
    pub exposure: String,
    pub comparator: String,
    #[serde(default)]
    pub outcomes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EligibilityCriteria {
    #[serde(default)]
    pub inclusion: Vec<String>,
    #[serde(default)]
    pub exclusion: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchStrategy {
    pub database: String,
    pub query: String,
    pub searched_at: Option<DateTime<Utc>>,
    pub result_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewProtocol {
    pub objective: String,
    pub question: String,
    pub registration: Option<String>,
    pub date_range: Option<String>,
    #[serde(default)]
    pub databases: Vec<String>,
    #[serde(default)]
    pub search_strategies: Vec<SearchStrategy>,
    #[serde(default)]
    pub eligibility: EligibilityCriteria,
    pub pico: Option<PicoFramework>,
    #[serde(default)]
    pub peco: Option<PecoFramework>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MedicalReviewContext {
    #[serde(default)]
    pub harms: Vec<String>,
    #[serde(default)]
    pub contraindications: Vec<String>,
    #[serde(default)]
    pub conflicts_of_interest: Vec<String>,
    #[serde(default)]
    pub guideline_conflicts: Vec<String>,
    #[serde(default)]
    pub extrapolation_limits: Vec<String>,
    #[serde(default)]
    pub guideline_source_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreeningStage {
    TitleAbstract,
    FullText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreeningDecisionValue {
    Pending,
    Include,
    Exclude,
    Maybe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreeningDecision {
    pub source_id: String,
    pub stage: ScreeningStage,
    pub decision: ScreeningDecisionValue,
    pub reason: Option<String>,
    pub reviewer: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskOfBiasTool {
    Rob2,
    RobinsI,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskJudgment {
    Low,
    SomeConcerns,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskOfBiasDomain {
    pub domain: String,
    pub judgment: RiskJudgment,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskOfBiasAssessment {
    pub id: String,
    pub source_id: String,
    pub result_id: Option<String>,
    pub tool: RiskOfBiasTool,
    #[serde(default)]
    pub domains: Vec<RiskOfBiasDomain>,
    pub overall: RiskJudgment,
    pub rationale: String,
    pub assessed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeCertainty {
    High,
    Moderate,
    Low,
    VeryLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeConcern {
    NotSerious,
    Serious,
    VerySerious,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeDomainAssessment {
    pub concern: GradeConcern,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeAssessment {
    pub id: String,
    pub outcome: String,
    pub relative_effect: Option<String>,
    pub absolute_effect: Option<String>,
    pub participants: Option<u64>,
    pub studies: Option<u64>,
    pub certainty: GradeCertainty,
    pub risk_of_bias: GradeDomainAssessment,
    pub inconsistency: GradeDomainAssessment,
    pub indirectness: GradeDomainAssessment,
    pub imprecision: GradeDomainAssessment,
    pub publication_bias: GradeDomainAssessment,
    pub applicability: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrismaSupplement {
    pub additional_identified: u64,
    pub duplicates_removed: u64,
    pub reports_not_retrieved: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrismaFlow {
    pub records_identified: u64,
    pub duplicates_removed: u64,
    pub records_screened: u64,
    pub records_excluded: u64,
    pub reports_sought: u64,
    pub reports_not_retrieved: u64,
    pub reports_assessed: u64,
    pub reports_excluded: u64,
    pub studies_included: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub contract_version: u32,
    pub id: String,
    pub title: String,
    pub domain: ReviewDomain,
    pub status: String,
    pub protocol: ReviewProtocol,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub screening: Vec<ScreeningDecision>,
    #[serde(default)]
    pub risk_of_bias: Vec<RiskOfBiasAssessment>,
    #[serde(default)]
    pub grade: Vec<GradeAssessment>,
    #[serde(default)]
    pub prisma: PrismaSupplement,
    #[serde(default)]
    pub medical: Option<MedicalReviewContext>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewDocument {
    pub record: ReviewRecord,
    pub revision: String,
    pub prisma_flow: PrismaFlow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub id: String,
    pub title: String,
    pub domain: ReviewDomain,
    pub status: String,
    pub source_count: usize,
    pub included_count: usize,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateReviewRequest {
    pub title: String,
    pub question: String,
    pub domain: ReviewDomain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationAuditSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationAuditIssue {
    pub severity: CitationAuditSeverity,
    pub code: String,
    pub message: String,
    pub source_id: Option<String>,
    pub evidence_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationAuditReport {
    pub review_id: String,
    pub checked_at: DateTime<Utc>,
    pub source_count: usize,
    pub evidence_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    #[serde(default)]
    pub issues: Vec<CitationAuditIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewExportFormat {
    Markdown,
    Pdf,
    Docx,
    Json,
    Csv,
    Bibtex,
    Ris,
}

impl ReviewExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Bibtex => "bib",
            Self::Ris => "ris",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewExportArtifact {
    pub review_id: String,
    pub format: ReviewExportFormat,
    pub path: String,
    pub bytes: u64,
    pub citation_audit: CitationAuditReport,
}

pub fn list_sources(
    workspace_root: &Path,
    tag: Option<&str>,
    search: Option<&str>,
) -> ResearchResult<Vec<SourceRecord>> {
    let mut sources =
        read_record_dir::<SourceRecord>(&research_dir(workspace_root).join(SOURCES_DIR))?;
    let normalized_tag = tag.map(normalize_token);
    let normalized_search = search.map(|value| value.trim().to_lowercase());
    sources.retain(|source| {
        let tag_matches = normalized_tag.as_ref().is_none_or(|expected| {
            source
                .tags
                .iter()
                .any(|value| normalize_token(value) == *expected)
        });
        let search_matches = normalized_search.as_ref().is_none_or(|expected| {
            source.title.to_lowercase().contains(expected)
                || source
                    .authors
                    .iter()
                    .any(|author| author.to_lowercase().contains(expected))
        });
        tag_matches && search_matches
    });
    sources.sort_by_key(|source| std::cmp::Reverse(source.updated_at));
    Ok(sources)
}

pub fn get_source(workspace_root: &Path, source_id: &str) -> ResearchResult<SourceRecord> {
    read_json(&source_path(workspace_root, source_id)?)
}

pub fn create_source(
    workspace_root: &Path,
    mut request: CreateSourceRequest,
) -> ResearchResult<SourceRecord> {
    request.title = request.title.trim().to_string();
    if request.title.is_empty() {
        return Err(ResearchError::Invalid(
            "source title cannot be empty".to_string(),
        ));
    }
    request.doi = normalize_identifier(
        request.doi,
        &["https://doi.org/", "http://doi.org/", "doi:"],
    );
    request.pmid = normalize_identifier(request.pmid, &["pmid:"]);
    request.pmcid = normalize_pmcid(request.pmcid);
    request.arxiv_id = normalize_identifier(request.arxiv_id, &["arxiv:"]);
    request.openalex_id = normalize_openalex_id(request.openalex_id);
    request.zotero_key = normalize_reference_key(request.zotero_key, "zotero:");
    request.clinical_trial_id = normalize_clinical_trial_id(request.clinical_trial_id);
    ensure_source_is_unique(workspace_root, &request)?;
    let now = Utc::now();
    let source = SourceRecord {
        contract_version: CONTRACT_VERSION,
        id: new_record_id("src"),
        source_kind: request.source_kind.unwrap_or_default(),
        title: request.title,
        authors: normalized_values(request.authors),
        abstract_text: clean_optional(request.abstract_text),
        doi: request.doi,
        pmid: request.pmid,
        pmcid: request.pmcid,
        arxiv_id: request.arxiv_id,
        openalex_id: request.openalex_id,
        zotero_key: request.zotero_key,
        clinical_trial_id: request.clinical_trial_id,
        year: request.year,
        venue: clean_optional(request.venue),
        url: clean_optional(request.url),
        pdf_path: clean_optional(request.pdf_path),
        tags: normalized_values(request.tags),
        notes: clean_optional(request.notes),
        provenance: request.provenance,
        europe_pmc: request.europe_pmc,
        added_at: now,
        updated_at: now,
    };
    write_json(&source_path(workspace_root, &source.id)?, &source)?;
    Ok(source)
}

pub fn ingest_source(
    workspace_root: &Path,
    request: CreateSourceRequest,
) -> ResearchResult<SourceIngestResult> {
    if let Some(mut existing) = find_matching_source(workspace_root, &request)? {
        merge_source(&mut existing, request);
        existing.updated_at = Utc::now();
        write_json(&source_path(workspace_root, &existing.id)?, &existing)?;
        return Ok(SourceIngestResult {
            source: existing,
            created: false,
        });
    }
    create_source(workspace_root, request).map(|source| SourceIngestResult {
        source,
        created: true,
    })
}

pub fn save_europe_pmc_supplement(
    workspace_root: &Path,
    source_id: &str,
    update: EuropePmcSupplementUpdate,
) -> ResearchResult<SourceRecord> {
    let mut source = get_source(workspace_root, source_id)?;
    let mut supplement = source.europe_pmc.take().unwrap_or_default();
    if let Some(citation_ids) = update.citation_ids {
        supplement.citation_ids = normalized_values(citation_ids);
    }
    if let Some(reference_ids) = update.reference_ids {
        supplement.reference_ids = normalized_values(reference_ids);
    }
    if let Some(mut biomedical_entities) = update.biomedical_entities {
        biomedical_entities.sort_by(|left, right| left.name.cmp(&right.name));
        biomedical_entities.dedup_by(|left, right| {
            left.name == right.name && left.semantic_type == right.semantic_type
        });
        supplement.biomedical_entities = biomedical_entities;
    }
    if let Some(full_text_path) = update.full_text_path {
        supplement.full_text_path = clean_optional(full_text_path);
    }
    if let Some(mut attempt) = update.attempt {
        attempt.successful_fields = normalized_values(attempt.successful_fields);
        attempt.failed_fields = normalized_values(attempt.failed_fields);
        if !attempt.successful_fields.is_empty() {
            supplement.enriched_at = Some(attempt.attempted_at);
        }
        supplement.enrichment_attempts.push(attempt);
    }
    source.europe_pmc = Some(supplement);
    source.updated_at = Utc::now();
    write_json(&source_path(workspace_root, source_id)?, &source)?;
    Ok(source)
}

pub fn write_full_text_xml(
    workspace_root: &Path,
    source_id: &str,
    xml: &str,
) -> ResearchResult<String> {
    validate_record_id(source_id)?;
    if xml.trim().is_empty() {
        return Err(ResearchError::Invalid(
            "Europe PMC full text is empty".to_string(),
        ));
    }
    let path = research_dir(workspace_root)
        .join(FULL_TEXT_DIR)
        .join(format!("{source_id}.xml"));
    atomic_write(&path, xml.as_bytes())?;
    path.strip_prefix(workspace_root)
        .map(|relative| relative.to_string_lossy().to_string())
        .map_err(|_| ResearchError::Invalid("full-text path is outside workspace".to_string()))
}

pub fn update_source_notes(
    workspace_root: &Path,
    source_id: &str,
    notes: String,
) -> ResearchResult<SourceRecord> {
    let mut source = get_source(workspace_root, source_id)?;
    source.notes = clean_optional(Some(notes));
    source.updated_at = Utc::now();
    write_json(&source_path(workspace_root, source_id)?, &source)?;
    Ok(source)
}

pub fn add_source_tags(
    workspace_root: &Path,
    source_id: &str,
    tags: Vec<String>,
) -> ResearchResult<SourceRecord> {
    let mut source = get_source(workspace_root, source_id)?;
    source.tags.extend(tags);
    source.tags = normalized_values(source.tags);
    source.updated_at = Utc::now();
    write_json(&source_path(workspace_root, source_id)?, &source)?;
    Ok(source)
}

pub fn delete_source(workspace_root: &Path, source_id: &str) -> ResearchResult<()> {
    let path = source_path(workspace_root, source_id)?;
    if !path.is_file() {
        return Err(ResearchError::NotFound(source_id.to_string()));
    }
    fs::remove_file(path)?;
    for evidence in list_evidence(workspace_root, Some(source_id), None)? {
        let evidence_path = evidence_path(workspace_root, &evidence.id)?;
        if evidence_path.is_file() {
            fs::remove_file(evidence_path)?;
        }
    }
    for summary in list_reviews(workspace_root)? {
        let mut document = get_review(workspace_root, &summary.id)?;
        let original_len = document.record.source_ids.len();
        document.record.source_ids.retain(|id| id != source_id);
        document
            .record
            .screening
            .retain(|item| item.source_id != source_id);
        document
            .record
            .risk_of_bias
            .retain(|item| item.source_id != source_id);
        if original_len != document.record.source_ids.len() {
            save_review(
                workspace_root,
                &summary.id,
                document.record,
                &document.revision,
            )?;
        }
    }
    Ok(())
}

pub fn list_evidence(
    workspace_root: &Path,
    source_id: Option<&str>,
    review_id: Option<&str>,
) -> ResearchResult<Vec<EvidenceRecord>> {
    let mut records =
        read_record_dir::<EvidenceRecord>(&research_dir(workspace_root).join(EVIDENCE_DIR))?;
    records.retain(|record| {
        source_id.is_none_or(|expected| record.source_id == expected)
            && review_id.is_none_or(|expected| record.review_id.as_deref() == Some(expected))
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    Ok(records)
}

pub fn upsert_evidence(
    workspace_root: &Path,
    request: UpsertEvidenceRequest,
) -> ResearchResult<EvidenceRecord> {
    get_source(workspace_root, &request.source_id)?;
    if let Some(review_id) = request.review_id.as_deref() {
        get_review(workspace_root, review_id)?;
    }
    let dimension = request.dimension.trim().to_string();
    if dimension.is_empty() {
        return Err(ResearchError::Invalid(
            "evidence dimension cannot be empty".to_string(),
        ));
    }
    let existing = if let Some(id) = request.id.as_deref() {
        Some(read_json::<EvidenceRecord>(&evidence_path(
            workspace_root,
            id,
        )?)?)
    } else {
        list_evidence(
            workspace_root,
            Some(&request.source_id),
            request.review_id.as_deref(),
        )?
        .into_iter()
        .find(|record| record.dimension == dimension)
    };
    let now = Utc::now();
    let record = EvidenceRecord {
        contract_version: CONTRACT_VERSION,
        id: existing
            .as_ref()
            .map(|record| record.id.clone())
            .unwrap_or_else(|| new_record_id("ev")),
        source_id: request.source_id,
        review_id: request.review_id,
        dimension,
        claim: request.claim.trim().to_string(),
        excerpt: clean_optional(request.excerpt),
        locator: clean_optional(request.locator),
        evidence_type: clean_optional(request.evidence_type),
        population: clean_optional(request.population),
        intervention: clean_optional(request.intervention),
        comparator: clean_optional(request.comparator),
        outcome: clean_optional(request.outcome),
        effect: clean_optional(request.effect),
        limitations: clean_optional(request.limitations),
        certainty: clean_optional(request.certainty),
        harms: normalized_values(request.harms),
        contraindications: normalized_values(request.contraindications),
        conflicts_of_interest: normalized_values(request.conflicts_of_interest),
        guideline_conflicts: normalized_values(request.guideline_conflicts),
        extrapolation_limits: normalized_values(request.extrapolation_limits),
        tags: normalized_values(request.tags),
        created_at: existing
            .as_ref()
            .map(|record| record.created_at)
            .unwrap_or(now),
        updated_at: now,
    };
    write_json(&evidence_path(workspace_root, &record.id)?, &record)?;
    Ok(record)
}

pub fn delete_evidence(workspace_root: &Path, evidence_id: &str) -> ResearchResult<()> {
    let path = evidence_path(workspace_root, evidence_id)?;
    if !path.is_file() {
        return Err(ResearchError::NotFound(evidence_id.to_string()));
    }
    fs::remove_file(path)?;
    Ok(())
}

pub fn list_reviews(workspace_root: &Path) -> ResearchResult<Vec<ReviewSummary>> {
    let root = research_dir(workspace_root).join(REVIEWS_DIR);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut summaries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "skipping unreadable review entry");
                continue;
            }
        };
        let review_path = entry.path().join(REVIEW_FILE);
        if !review_path.is_file() {
            continue;
        }
        match read_json::<ReviewRecord>(&review_path) {
            Ok(record) => summaries.push(review_summary(&record)),
            Err(error) => {
                tracing::warn!(path = %review_path.display(), %error, "skipping invalid review")
            }
        }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    Ok(summaries)
}

pub fn create_review(
    workspace_root: &Path,
    request: CreateReviewRequest,
) -> ResearchResult<ReviewDocument> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(ResearchError::Invalid(
            "review title cannot be empty".to_string(),
        ));
    }
    let now = Utc::now();
    let record = ReviewRecord {
        contract_version: CONTRACT_VERSION,
        id: new_record_id("review"),
        title: title.to_string(),
        domain: request.domain,
        status: "protocol".to_string(),
        protocol: ReviewProtocol {
            objective: String::new(),
            question: request.question.trim().to_string(),
            registration: None,
            date_range: None,
            databases: Vec::new(),
            search_strategies: Vec::new(),
            eligibility: EligibilityCriteria::default(),
            pico: (request.domain == ReviewDomain::Medical).then(PicoFramework::default),
            peco: None,
        },
        source_ids: Vec::new(),
        screening: Vec::new(),
        risk_of_bias: Vec::new(),
        grade: Vec::new(),
        prisma: PrismaSupplement::default(),
        medical: (request.domain == ReviewDomain::Medical).then(MedicalReviewContext::default),
        created_at: now,
        updated_at: now,
    };
    let path = review_path(workspace_root, &record.id)?;
    write_json(&path, &record)?;
    get_review(workspace_root, &record.id)
}

pub fn get_review(workspace_root: &Path, review_id: &str) -> ResearchResult<ReviewDocument> {
    let path = review_path(workspace_root, review_id)?;
    if !path.is_file() {
        return Err(ResearchError::NotFound(review_id.to_string()));
    }
    let bytes = read_limited(&path)?;
    let record: ReviewRecord = serde_json::from_slice(&bytes)?;
    validate_review(&record, review_id)?;
    Ok(ReviewDocument {
        prisma_flow: prisma_flow(&record),
        record,
        revision: hash_bytes(&bytes),
    })
}

pub fn save_review(
    workspace_root: &Path,
    review_id: &str,
    mut record: ReviewRecord,
    expected_revision: &str,
) -> ResearchResult<ReviewDocument> {
    let current = get_review(workspace_root, review_id)?;
    if current.revision != expected_revision {
        return Err(ResearchError::Conflict(
            "review changed on disk; reload before saving".to_string(),
        ));
    }
    validate_review(&record, review_id)?;
    record.source_ids = normalized_values(record.source_ids);
    for source_id in &record.source_ids {
        get_source(workspace_root, source_id)?;
    }
    if let Some(medical) = record.medical.as_mut() {
        medical.harms = normalized_values(std::mem::take(&mut medical.harms));
        medical.contraindications =
            normalized_values(std::mem::take(&mut medical.contraindications));
        medical.conflicts_of_interest =
            normalized_values(std::mem::take(&mut medical.conflicts_of_interest));
        medical.guideline_conflicts =
            normalized_values(std::mem::take(&mut medical.guideline_conflicts));
        medical.extrapolation_limits =
            normalized_values(std::mem::take(&mut medical.extrapolation_limits));
        medical.guideline_source_ids =
            normalized_values(std::mem::take(&mut medical.guideline_source_ids));
        for source_id in &medical.guideline_source_ids {
            let source = get_source(workspace_root, source_id)?;
            if source.source_kind != SourceKind::Guideline {
                return Err(ResearchError::Invalid(format!(
                    "medical guideline source {source_id} is not marked as a guideline"
                )));
            }
        }
    }
    record.updated_at = Utc::now();
    write_json(&review_path(workspace_root, review_id)?, &record)?;
    get_review(workspace_root, review_id)
}

pub fn delete_review(workspace_root: &Path, review_id: &str) -> ResearchResult<()> {
    let path = review_dir(workspace_root, review_id)?;
    if !path.is_dir() {
        return Err(ResearchError::NotFound(review_id.to_string()));
    }
    fs::remove_dir_all(path)?;
    for evidence in list_evidence(workspace_root, None, Some(review_id))? {
        let path = evidence_path(workspace_root, &evidence.id)?;
        if path.is_file() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub fn prisma_flow(review: &ReviewRecord) -> PrismaFlow {
    let title_decisions: Vec<&ScreeningDecision> = review
        .screening
        .iter()
        .filter(|item| item.stage == ScreeningStage::TitleAbstract)
        .collect();
    let full_text_decisions: Vec<&ScreeningDecision> = review
        .screening
        .iter()
        .filter(|item| item.stage == ScreeningStage::FullText)
        .collect();
    PrismaFlow {
        records_identified: usize_to_u64(review.source_ids.len())
            .saturating_add(review.prisma.additional_identified),
        duplicates_removed: review.prisma.duplicates_removed,
        records_screened: usize_to_u64(title_decisions.len()),
        records_excluded: usize_to_u64(
            title_decisions
                .iter()
                .filter(|item| item.decision == ScreeningDecisionValue::Exclude)
                .count(),
        ),
        reports_sought: usize_to_u64(
            title_decisions
                .iter()
                .filter(|item| item.decision == ScreeningDecisionValue::Include)
                .count(),
        ),
        reports_not_retrieved: review.prisma.reports_not_retrieved,
        reports_assessed: usize_to_u64(full_text_decisions.len()),
        reports_excluded: usize_to_u64(
            full_text_decisions
                .iter()
                .filter(|item| item.decision == ScreeningDecisionValue::Exclude)
                .count(),
        ),
        studies_included: usize_to_u64(
            full_text_decisions
                .iter()
                .filter(|item| item.decision == ScreeningDecisionValue::Include)
                .count(),
        ),
    }
}

pub fn audit_review(workspace_root: &Path, review_id: &str) -> ResearchResult<CitationAuditReport> {
    let document = get_review(workspace_root, review_id)?;
    let evidence = list_evidence(workspace_root, None, Some(review_id))?;
    let sources = list_sources(workspace_root, None, None)?;
    let source_by_id: BTreeMap<&str, &SourceRecord> = sources
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut issues = Vec::new();

    for source_id in &document.record.source_ids {
        if !source_by_id.contains_key(source_id.as_str()) {
            issues.push(audit_issue(
                CitationAuditSeverity::Error,
                "missing_source",
                format!("Review references missing source {source_id}"),
                Some(source_id.clone()),
                None,
            ));
        }
    }

    for record in &evidence {
        if !source_by_id.contains_key(record.source_id.as_str()) {
            issues.push(audit_issue(
                CitationAuditSeverity::Error,
                "orphan_evidence",
                format!("Evidence {} references a missing source", record.id),
                Some(record.source_id.clone()),
                Some(record.id.clone()),
            ));
        }
        if !document.record.source_ids.contains(&record.source_id) {
            issues.push(audit_issue(
                CitationAuditSeverity::Warning,
                "evidence_outside_review",
                "Evidence source is not included in the review source set".to_string(),
                Some(record.source_id.clone()),
                Some(record.id.clone()),
            ));
        }
        if record.claim.trim().is_empty() {
            issues.push(audit_issue(
                CitationAuditSeverity::Error,
                "empty_claim",
                "Evidence claim is empty".to_string(),
                Some(record.source_id.clone()),
                Some(record.id.clone()),
            ));
        }
        if record.excerpt.as_deref().is_none_or(str::is_empty)
            && record.locator.as_deref().is_none_or(str::is_empty)
        {
            issues.push(audit_issue(
                CitationAuditSeverity::Warning,
                "missing_locator",
                "Evidence has neither an excerpt nor a source locator".to_string(),
                Some(record.source_id.clone()),
                Some(record.id.clone()),
            ));
        }
    }

    for source_id in &document.record.source_ids {
        let included = document.record.screening.iter().any(|decision| {
            decision.source_id == *source_id
                && decision.stage == ScreeningStage::FullText
                && decision.decision == ScreeningDecisionValue::Include
        });
        if included && !evidence.iter().any(|record| record.source_id == *source_id) {
            issues.push(audit_issue(
                CitationAuditSeverity::Warning,
                "included_without_evidence",
                "Included full-text source has no extracted evidence".to_string(),
                Some(source_id.clone()),
                None,
            ));
        }
    }

    for decision in &document.record.screening {
        if decision.decision == ScreeningDecisionValue::Exclude
            && decision.reason.as_deref().is_none_or(str::is_empty)
        {
            issues.push(audit_issue(
                CitationAuditSeverity::Warning,
                "exclusion_without_reason",
                "Excluded source has no exclusion reason".to_string(),
                Some(decision.source_id.clone()),
                None,
            ));
        }
    }

    let error_count = issues
        .iter()
        .filter(|issue| issue.severity == CitationAuditSeverity::Error)
        .count();
    let warning_count = issues
        .iter()
        .filter(|issue| issue.severity == CitationAuditSeverity::Warning)
        .count();
    Ok(CitationAuditReport {
        review_id: review_id.to_string(),
        checked_at: Utc::now(),
        source_count: document.record.source_ids.len(),
        evidence_count: evidence.len(),
        error_count,
        warning_count,
        issues,
    })
}

pub fn export_review(
    workspace_root: &Path,
    review_id: &str,
    format: ReviewExportFormat,
) -> ResearchResult<ReviewExportArtifact> {
    let document = get_review(workspace_root, review_id)?;
    let sources = document
        .record
        .source_ids
        .iter()
        .map(|source_id| get_source(workspace_root, source_id))
        .collect::<ResearchResult<Vec<_>>>()?;
    let evidence = list_evidence(workspace_root, None, Some(review_id))?;
    let audit = audit_review(workspace_root, review_id)?;
    let markdown = render_review_markdown(&document, &sources, &evidence, &audit);
    let bytes = match format {
        ReviewExportFormat::Markdown => markdown.into_bytes(),
        ReviewExportFormat::Pdf | ReviewExportFormat::Docx => {
            render_review_document(&markdown, format)?
        }
        ReviewExportFormat::Json => serde_json::to_vec_pretty(&serde_json::json!({
            "review": document,
            "sources": sources,
            "evidence": evidence,
            "citation_audit": audit,
        }))?,
        ReviewExportFormat::Csv => render_evidence_csv(&sources, &evidence).into_bytes(),
        ReviewExportFormat::Bibtex => render_bibtex(&sources).into_bytes(),
        ReviewExportFormat::Ris => render_ris(&sources).into_bytes(),
    };
    let path = review_dir(workspace_root, review_id)?
        .join(REPORTS_DIR)
        .join(format!("systematic-review.{}", format.extension()));
    atomic_write(&path, &bytes)?;
    let relative = path
        .strip_prefix(workspace_root)
        .map_err(|_| ResearchError::Invalid("report path is outside workspace".to_string()))?
        .to_string_lossy()
        .to_string();
    Ok(ReviewExportArtifact {
        review_id: review_id.to_string(),
        format,
        path: relative,
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        citation_audit: audit,
    })
}

pub fn export_all_review_formats(
    workspace_root: &Path,
    review_id: &str,
) -> ResearchResult<Vec<ReviewExportArtifact>> {
    let mut formats = vec![
        ReviewExportFormat::Markdown,
        ReviewExportFormat::Json,
        ReviewExportFormat::Csv,
        ReviewExportFormat::Bibtex,
        ReviewExportFormat::Ris,
    ];
    if document_renderer_available() {
        formats.push(ReviewExportFormat::Docx);
        if pdf_renderer_available() {
            formats.push(ReviewExportFormat::Pdf);
        }
    }
    formats
        .into_iter()
        .map(|format| export_review(workspace_root, review_id, format))
        .collect()
}

pub fn document_renderer_available() -> bool {
    resolve_document_renderer().is_some()
}

fn pdf_renderer_available() -> bool {
    match resolve_document_renderer() {
        Some(DocumentRenderer::Quarto(_)) => true,
        Some(DocumentRenderer::Pandoc { pdf_engine, .. }) => pdf_engine.is_some(),
        None => false,
    }
}

#[derive(Debug, Clone)]
enum DocumentRenderer {
    Pandoc {
        binary: PathBuf,
        pdf_engine: Option<String>,
    },
    Quarto(PathBuf),
}

fn resolve_document_renderer() -> Option<DocumentRenderer> {
    if let Some(path) = configured_executable("EKO_PANDOC") {
        return Some(DocumentRenderer::Pandoc {
            binary: path,
            pdf_engine: preferred_pdf_engine(),
        });
    }
    if executable_available(Path::new("pandoc")) {
        return Some(DocumentRenderer::Pandoc {
            binary: PathBuf::from("pandoc"),
            pdf_engine: preferred_pdf_engine(),
        });
    }
    if let Some(path) = configured_executable("EKO_QUARTO") {
        return Some(DocumentRenderer::Quarto(path));
    }
    executable_available(Path::new("quarto"))
        .then(|| DocumentRenderer::Quarto(PathBuf::from("quarto")))
}

fn configured_executable(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| executable_available(path))
}

fn executable_available(path: &Path) -> bool {
    std::process::Command::new(path)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn render_review_document(markdown: &str, format: ReviewExportFormat) -> ResearchResult<Vec<u8>> {
    let renderer = resolve_document_renderer().ok_or_else(|| {
        ResearchError::External(
            "PDF/DOCX export requires Pandoc or Quarto on PATH; EKO_PANDOC/EKO_QUARTO may point to a custom executable"
                .to_string(),
        )
    })?;
    render_review_document_with_renderer(markdown, format, renderer)
}

fn render_review_document_with_renderer(
    markdown: &str,
    format: ReviewExportFormat,
    renderer: DocumentRenderer,
) -> ResearchResult<Vec<u8>> {
    let temp = tempfile::Builder::new()
        .prefix("eko-systematic-review-")
        .tempdir()
        .map_err(ResearchError::Io)?;
    let input = temp.path().join("systematic-review.md");
    let output = temp
        .path()
        .join(format!("systematic-review.{}", format.extension()));
    fs::write(&input, markdown)?;

    let command_output = match renderer {
        DocumentRenderer::Pandoc { binary, pdf_engine } => {
            let mut command = std::process::Command::new(binary);
            command.arg(&input).arg("--from=gfm").arg("--standalone");
            if format == ReviewExportFormat::Docx {
                command.arg("--to=docx");
            }
            command.arg("--output").arg(&output);
            if format == ReviewExportFormat::Pdf {
                let engine = pdf_engine.ok_or_else(|| {
                    ResearchError::External(
                        "Pandoc PDF export requires typst, weasyprint, wkhtmltopdf, xelatex, lualatex, or pdflatex; EKO_PDF_ENGINE may select another supported engine"
                            .to_string(),
                    )
                })?;
                command.arg(format!("--pdf-engine={engine}"));
            }
            command.output()
        }
        DocumentRenderer::Quarto(binary) => {
            let mut command = std::process::Command::new(binary);
            command
                .arg("render")
                .arg(&input)
                .arg("--to")
                .arg(format.extension())
                .arg("--output")
                .arg(output.file_name().ok_or_else(|| {
                    ResearchError::Invalid("document output filename is unavailable".to_string())
                })?)
                .current_dir(temp.path());
            command.output()
        }
    }
    .map_err(|error| ResearchError::External(format!("document renderer failed: {error}")))?;

    if !command_output.status.success() {
        let stderr = String::from_utf8_lossy(&command_output.stderr)
            .chars()
            .take(2_000)
            .collect::<String>();
        return Err(ResearchError::External(format!(
            "document renderer exited with {}: {}",
            command_output.status,
            stderr.trim()
        )));
    }
    fs::read(&output).map_err(ResearchError::Io)
}

fn preferred_pdf_engine() -> Option<String> {
    if let Ok(engine) = std::env::var("EKO_PDF_ENGINE")
        && !engine.trim().is_empty()
    {
        return Some(engine);
    }
    [
        "typst",
        "weasyprint",
        "wkhtmltopdf",
        "xelatex",
        "lualatex",
        "pdflatex",
    ]
    .into_iter()
    .find(|engine| executable_available(Path::new(engine)))
    .map(str::to_string)
}

fn audit_issue(
    severity: CitationAuditSeverity,
    code: &str,
    message: String,
    source_id: Option<String>,
    evidence_id: Option<String>,
) -> CitationAuditIssue {
    CitationAuditIssue {
        severity,
        code: code.to_string(),
        message,
        source_id,
        evidence_id,
    }
}

fn render_review_markdown(
    document: &ReviewDocument,
    sources: &[SourceRecord],
    evidence: &[EvidenceRecord],
    audit: &CitationAuditReport,
) -> String {
    let record = &document.record;
    let mut output = format!(
        "# {}\n\n## Protocol\n\n**Question:** {}\n\n**Objective:** {}\n\n",
        record.title, record.protocol.question, record.protocol.objective
    );
    if let Some(pico) = &record.protocol.pico {
        output.push_str(&format!(
            "### PICO\n\n- Population: {}\n- Intervention: {}\n- Comparator: {}\n- Outcomes: {}\n\n",
            pico.population,
            pico.intervention,
            pico.comparator,
            pico.outcomes.join("; ")
        ));
    }
    if let Some(peco) = &record.protocol.peco {
        output.push_str(&format!(
            "### PECO\n\n- Population: {}\n- Exposure: {}\n- Comparator: {}\n- Outcomes: {}\n\n",
            peco.population,
            peco.exposure,
            peco.comparator,
            peco.outcomes.join("; ")
        ));
    }
    output.push_str("## Search Strategy\n\n");
    for search in &record.protocol.search_strategies {
        output.push_str(&format!(
            "- **{}**: `{}`; searched {}; results {}\n",
            search.database,
            search.query,
            search
                .searched_at
                .map(|date| date.to_rfc3339())
                .unwrap_or_else(|| "not recorded".to_string()),
            search
                .result_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "not recorded".to_string())
        ));
    }
    let flow = &document.prisma_flow;
    output.push_str(&format!(
        "\n## PRISMA Flow\n\n- Records identified: {}\n- Duplicates removed: {}\n- Records screened: {}\n- Reports assessed: {}\n- Studies included: {}\n\n",
        flow.records_identified,
        flow.duplicates_removed,
        flow.records_screened,
        flow.reports_assessed,
        flow.studies_included
    ));
    output.push_str("## Evidence\n\n| Source | Dimension | Claim | Locator | Certainty |\n|---|---|---|---|---|\n");
    for item in evidence {
        let title = sources
            .iter()
            .find(|source| source.id == item.source_id)
            .map(|source| source.title.as_str())
            .unwrap_or(item.source_id.as_str());
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            markdown_cell(title),
            markdown_cell(&item.dimension),
            markdown_cell(&item.claim),
            markdown_cell(item.locator.as_deref().unwrap_or("")),
            markdown_cell(item.certainty.as_deref().unwrap_or(""))
        ));
    }
    output.push_str("\n## Risk Of Bias\n\n");
    for assessment in &record.risk_of_bias {
        output.push_str(&format!(
            "- {}: {:?}. {}\n",
            assessment.source_id, assessment.overall, assessment.rationale
        ));
    }
    output.push_str("\n## GRADE Summary Of Findings\n\n");
    for grade in &record.grade {
        output.push_str(&format!(
            "- {}: {:?}; relative effect {}; absolute effect {}; participants {}; studies {}; applicability {}\n",
            grade.outcome,
            grade.certainty,
            grade.relative_effect.as_deref().unwrap_or("not reported"),
            grade.absolute_effect.as_deref().unwrap_or("not reported"),
            grade.participants.map(|value| value.to_string()).unwrap_or_else(|| "not reported".to_string()),
            grade.studies.map(|value| value.to_string()).unwrap_or_else(|| "not reported".to_string()),
            grade.applicability.as_deref().unwrap_or("not assessed")
        ));
    }
    if let Some(medical) = &record.medical {
        output.push_str(&format!(
            "\n## Medical Applicability\n\n- Harms: {}\n- Contraindications: {}\n- Conflicts of interest: {}\n- Guideline conflicts: {}\n- Extrapolation limits: {}\n\n",
            medical.harms.join("; "),
            medical.contraindications.join("; "),
            medical.conflicts_of_interest.join("; "),
            medical.guideline_conflicts.join("; "),
            medical.extrapolation_limits.join("; ")
        ));
    }
    output.push_str(&format!(
        "## Citation Audit\n\nErrors: {}; warnings: {}.\n\n",
        audit.error_count, audit.warning_count
    ));
    for issue in &audit.issues {
        output.push_str(&format!(
            "- [{:?}] {}: {}\n",
            issue.severity, issue.code, issue.message
        ));
    }
    output
}

fn render_evidence_csv(sources: &[SourceRecord], evidence: &[EvidenceRecord]) -> String {
    let mut rows = vec![
        [
            "source_id",
            "title",
            "dimension",
            "claim",
            "excerpt",
            "locator",
            "effect",
            "certainty",
            "limitations",
            "harms",
            "contraindications",
            "conflicts_of_interest",
            "guideline_conflicts",
            "extrapolation_limits",
        ]
        .into_iter()
        .map(csv_cell)
        .collect::<Vec<_>>()
        .join(","),
    ];
    for item in evidence {
        let title = sources
            .iter()
            .find(|source| source.id == item.source_id)
            .map(|source| source.title.as_str())
            .unwrap_or("");
        rows.push(
            [
                item.source_id.as_str(),
                title,
                item.dimension.as_str(),
                item.claim.as_str(),
                item.excerpt.as_deref().unwrap_or(""),
                item.locator.as_deref().unwrap_or(""),
                item.effect.as_deref().unwrap_or(""),
                item.certainty.as_deref().unwrap_or(""),
                item.limitations.as_deref().unwrap_or(""),
                &item.harms.join("; "),
                &item.contraindications.join("; "),
                &item.conflicts_of_interest.join("; "),
                &item.guideline_conflicts.join("; "),
                &item.extrapolation_limits.join("; "),
            ]
            .into_iter()
            .map(csv_cell)
            .collect::<Vec<_>>()
            .join(","),
        );
    }
    format!("{}\n", rows.join("\n"))
}

fn render_bibtex(sources: &[SourceRecord]) -> String {
    sources
        .iter()
        .map(|source| {
            let key = citation_key(source);
            format!(
                "@article{{{key},\n  title = {{{}}},\n  author = {{{}}},\n  year = {{{}}},\n  journal = {{{}}},\n  doi = {{{}}},\n  url = {{{}}}\n}}",
                source.title,
                source.authors.join(" and "),
                source.year.map(|year| year.to_string()).unwrap_or_default(),
                source.venue.as_deref().unwrap_or(""),
                source.doi.as_deref().unwrap_or(""),
                source.url.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_ris(sources: &[SourceRecord]) -> String {
    sources
        .iter()
        .map(|source| {
            let mut lines = vec!["TY  - JOUR".to_string(), format!("TI  - {}", source.title)];
            lines.extend(
                source
                    .authors
                    .iter()
                    .map(|author| format!("AU  - {author}")),
            );
            if let Some(year) = source.year {
                lines.push(format!("PY  - {year}"));
            }
            if let Some(venue) = &source.venue {
                lines.push(format!("JO  - {venue}"));
            }
            if let Some(doi) = &source.doi {
                lines.push(format!("DO  - {doi}"));
            }
            if let Some(url) = &source.url {
                lines.push(format!("UR  - {url}"));
            }
            lines.push("ER  -".to_string());
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn citation_key(source: &SourceRecord) -> String {
    let author = source
        .authors
        .first()
        .and_then(|name| name.split_whitespace().last())
        .unwrap_or("source");
    let year = source
        .year
        .map(|year| year.to_string())
        .unwrap_or_else(|| "nd".to_string());
    let raw = format!("{author}{year}");
    raw.chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect()
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn find_matching_source(
    workspace_root: &Path,
    request: &CreateSourceRequest,
) -> ResearchResult<Option<SourceRecord>> {
    let incoming_doi = normalize_identifier(
        request.doi.clone(),
        &["https://doi.org/", "http://doi.org/", "doi:"],
    );
    let incoming_pmid = normalize_identifier(request.pmid.clone(), &["pmid:"]);
    let incoming_pmcid = normalize_pmcid(request.pmcid.clone());
    let incoming_arxiv = normalize_identifier(request.arxiv_id.clone(), &["arxiv:"]);
    let incoming_openalex = normalize_openalex_id(request.openalex_id.clone());
    let incoming_zotero = normalize_reference_key(request.zotero_key.clone(), "zotero:");
    let incoming_trial = normalize_clinical_trial_id(request.clinical_trial_id.clone());
    let normalized_title = normalize_token(&request.title);
    Ok(list_sources(workspace_root, None, None)?
        .into_iter()
        .find(|source| {
            [
                (incoming_doi.as_ref(), source.doi.as_ref()),
                (incoming_pmid.as_ref(), source.pmid.as_ref()),
                (incoming_pmcid.as_ref(), source.pmcid.as_ref()),
                (incoming_arxiv.as_ref(), source.arxiv_id.as_ref()),
                (incoming_openalex.as_ref(), source.openalex_id.as_ref()),
                (incoming_zotero.as_ref(), source.zotero_key.as_ref()),
                (incoming_trial.as_ref(), source.clinical_trial_id.as_ref()),
            ]
            .iter()
            .any(|(incoming, existing)| incoming.is_some() && incoming == existing)
                || (!normalized_title.is_empty()
                    && normalize_token(&source.title) == normalized_title
                    && request.year.is_some()
                    && request.year == source.year)
        }))
}

fn merge_source(source: &mut SourceRecord, request: CreateSourceRequest) {
    if source.title.trim().is_empty() && !request.title.trim().is_empty() {
        source.title = request.title.trim().to_string();
    }
    source.authors.extend(request.authors);
    source.authors = normalized_values(std::mem::take(&mut source.authors));
    source.abstract_text = source
        .abstract_text
        .take()
        .or_else(|| clean_optional(request.abstract_text));
    source.doi = source.doi.take().or_else(|| {
        normalize_identifier(
            request.doi,
            &["https://doi.org/", "http://doi.org/", "doi:"],
        )
    });
    source.pmid = source
        .pmid
        .take()
        .or_else(|| normalize_identifier(request.pmid, &["pmid:"]));
    source.pmcid = source
        .pmcid
        .take()
        .or_else(|| normalize_pmcid(request.pmcid));
    source.arxiv_id = source
        .arxiv_id
        .take()
        .or_else(|| normalize_identifier(request.arxiv_id, &["arxiv:"]));
    source.openalex_id = source
        .openalex_id
        .take()
        .or_else(|| normalize_openalex_id(request.openalex_id));
    source.zotero_key = source
        .zotero_key
        .take()
        .or_else(|| normalize_reference_key(request.zotero_key, "zotero:"));
    source.clinical_trial_id = source
        .clinical_trial_id
        .take()
        .or_else(|| normalize_clinical_trial_id(request.clinical_trial_id));
    source.year = source.year.or(request.year);
    source.venue = source
        .venue
        .take()
        .or_else(|| clean_optional(request.venue));
    source.url = source.url.take().or_else(|| clean_optional(request.url));
    source.pdf_path = source
        .pdf_path
        .take()
        .or_else(|| clean_optional(request.pdf_path));
    source.tags.extend(request.tags);
    source.tags = normalized_values(std::mem::take(&mut source.tags));
    source.notes = source
        .notes
        .take()
        .or_else(|| clean_optional(request.notes));
    source.provenance.extend(request.provenance);
    source.provenance.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.record_url.cmp(&right.record_url))
    });
    source.provenance.dedup_by(|left, right| {
        left.provider == right.provider && left.record_url == right.record_url
    });
    if source.europe_pmc.is_none() {
        source.europe_pmc = request.europe_pmc;
    }
    if source.source_kind == SourceKind::Other {
        source.source_kind = request.source_kind.unwrap_or_default();
    }
}

fn ensure_source_is_unique(
    workspace_root: &Path,
    request: &CreateSourceRequest,
) -> ResearchResult<()> {
    for source in list_sources(workspace_root, None, None)? {
        for (label, incoming, existing) in [
            ("DOI", request.doi.as_ref(), source.doi.as_ref()),
            ("PMID", request.pmid.as_ref(), source.pmid.as_ref()),
            ("PMCID", request.pmcid.as_ref(), source.pmcid.as_ref()),
            ("arXiv", request.arxiv_id.as_ref(), source.arxiv_id.as_ref()),
            (
                "OpenAlex",
                request.openalex_id.as_ref(),
                source.openalex_id.as_ref(),
            ),
            (
                "Zotero",
                request.zotero_key.as_ref(),
                source.zotero_key.as_ref(),
            ),
            (
                "ClinicalTrials.gov",
                request.clinical_trial_id.as_ref(),
                source.clinical_trial_id.as_ref(),
            ),
        ] {
            if incoming.is_some() && incoming == existing {
                return Err(ResearchError::Conflict(format!(
                    "{label} already belongs to source {}",
                    source.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_review(record: &ReviewRecord, expected_id: &str) -> ResearchResult<()> {
    validate_record_id(expected_id)?;
    if record.contract_version != CONTRACT_VERSION || record.id != expected_id {
        return Err(ResearchError::Invalid(
            "review identity or contract version is invalid".to_string(),
        ));
    }
    if record.title.trim().is_empty() {
        return Err(ResearchError::Invalid(
            "review title cannot be empty".to_string(),
        ));
    }
    if record.domain == ReviewDomain::Medical
        && record.protocol.pico.is_none()
        && record.protocol.peco.is_none()
    {
        return Err(ResearchError::Invalid(
            "medical reviews require a PICO or PECO framework".to_string(),
        ));
    }
    if record.domain == ReviewDomain::Medical && record.medical.is_none() {
        return Err(ResearchError::Invalid(
            "medical reviews require a medical evidence context".to_string(),
        ));
    }
    Ok(())
}

fn review_summary(record: &ReviewRecord) -> ReviewSummary {
    ReviewSummary {
        id: record.id.clone(),
        title: record.title.clone(),
        domain: record.domain,
        status: record.status.clone(),
        source_count: record.source_ids.len(),
        included_count: record
            .screening
            .iter()
            .filter(|item| {
                item.stage == ScreeningStage::FullText
                    && item.decision == ScreeningDecisionValue::Include
            })
            .count(),
        updated_at: record.updated_at,
    }
}

fn research_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(RESEARCH_ROOT)
}

fn source_path(workspace_root: &Path, source_id: &str) -> ResearchResult<PathBuf> {
    validate_record_id(source_id)?;
    Ok(research_dir(workspace_root)
        .join(SOURCES_DIR)
        .join(format!("{source_id}.json")))
}

fn evidence_path(workspace_root: &Path, evidence_id: &str) -> ResearchResult<PathBuf> {
    validate_record_id(evidence_id)?;
    Ok(research_dir(workspace_root)
        .join(EVIDENCE_DIR)
        .join(format!("{evidence_id}.json")))
}

fn review_dir(workspace_root: &Path, review_id: &str) -> ResearchResult<PathBuf> {
    validate_record_id(review_id)?;
    Ok(research_dir(workspace_root)
        .join(REVIEWS_DIR)
        .join(review_id))
}

fn review_path(workspace_root: &Path, review_id: &str) -> ResearchResult<PathBuf> {
    Ok(review_dir(workspace_root, review_id)?.join(REVIEW_FILE))
}

fn validate_record_id(record_id: &str) -> ResearchResult<()> {
    if record_id.is_empty()
        || !record_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err(ResearchError::Invalid(
            "record id contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

fn new_record_id(prefix: &str) -> String {
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(16)
        .collect();
    format!("{prefix}-{suffix}")
}

fn normalize_identifier(value: Option<String>, prefixes: &[&str]) -> Option<String> {
    let mut normalized = value?.trim().to_string();
    if normalized.is_empty() {
        return None;
    }
    let lower = normalized.to_lowercase();
    for prefix in prefixes {
        if lower.starts_with(&prefix.to_lowercase()) {
            normalized = normalized.chars().skip(prefix.chars().count()).collect();
            break;
        }
    }
    let normalized = normalized.trim().to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_reference_key(value: Option<String>, prefix: &str) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    let normalized = if trimmed.to_lowercase().starts_with(prefix) {
        trimmed
            .chars()
            .skip(prefix.chars().count())
            .collect::<String>()
    } else {
        trimmed.to_string()
    };
    clean_optional(Some(normalized))
}

fn normalize_clinical_trial_id(value: Option<String>) -> Option<String> {
    normalize_reference_key(value, "nct:").map(|value| value.to_uppercase())
}

fn normalize_pmcid(value: Option<String>) -> Option<String> {
    normalize_reference_key(value, "pmcid:").map(|value| value.to_uppercase())
}

fn normalize_openalex_id(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    let lower = trimmed.to_lowercase();
    let normalized = if lower.starts_with("https://openalex.org/") {
        trimmed
            .chars()
            .skip("https://openalex.org/".chars().count())
            .collect::<String>()
    } else if lower.starts_with("http://openalex.org/") {
        trimmed
            .chars()
            .skip("http://openalex.org/".chars().count())
            .collect::<String>()
    } else {
        trimmed.to_string()
    };
    clean_optional(Some(normalized)).map(|value| value.to_uppercase())
}

fn normalize_token(value: &str) -> String {
    value.trim().to_lowercase()
}

fn normalized_values(values: Vec<String>) -> Vec<String> {
    let mut unique = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            unique.insert(trimmed.to_string());
        }
    }
    unique.into_iter().collect()
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn read_record_dir<T>(directory: &Path) -> ResearchResult<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "skipping unreadable research record");
                continue;
            }
        };
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        match read_json(&entry.path()) {
            Ok(record) => records.push(record),
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), %error, "skipping invalid research record")
            }
        }
    }
    Ok(records)
}

fn read_json<T>(path: &Path) -> ResearchResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.is_file() {
        return Err(ResearchError::NotFound(path.display().to_string()));
    }
    Ok(serde_json::from_slice(&read_limited(path)?)?)
}

fn read_limited(path: &Path) -> ResearchResult<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_JSON_BYTES {
        return Err(ResearchError::Invalid(format!(
            "research JSON exceeds {} bytes: {}",
            MAX_JSON_BYTES,
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> ResearchResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(ResearchError::Invalid(
            "research JSON exceeds the 4 MiB limit".to_string(),
        ));
    }
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> ResearchResult<()> {
    let parent = path.parent().ok_or_else(|| {
        ResearchError::Invalid(format!("research path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let temp_path = parent.join(format!(".{}.tmp", new_record_id("write")));
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> ResearchResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ResearchError::Io)
    }

    #[test]
    fn source_and_evidence_are_file_backed_and_deduplicated() -> ResearchResult<()> {
        let workspace = temp_workspace()?;
        let source = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "A trial".to_string(),
                doi: Some("https://doi.org/10.1000/Test".to_string()),
                ..CreateSourceRequest::default()
            },
        )?;
        assert_eq!(source.doi.as_deref(), Some("10.1000/test"));
        let duplicate = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "Duplicate".to_string(),
                doi: Some("10.1000/test".to_string()),
                ..CreateSourceRequest::default()
            },
        );
        assert!(matches!(duplicate, Err(ResearchError::Conflict(_))));

        let evidence = upsert_evidence(
            workspace.path(),
            UpsertEvidenceRequest {
                source_id: source.id.clone(),
                dimension: "method".to_string(),
                claim: "Randomized design".to_string(),
                ..UpsertEvidenceRequest::default()
            },
        )?;
        assert_eq!(
            list_evidence(workspace.path(), Some(&source.id), None)?.len(),
            1
        );
        assert!(evidence_path(workspace.path(), &evidence.id)?.is_file());
        Ok(())
    }

    #[test]
    fn europe_pmc_update_preserves_failed_dimensions() -> ResearchResult<()> {
        let workspace = temp_workspace()?;
        let source = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "Enriched source".to_string(),
                pmcid: Some("PMC123".to_string()),
                europe_pmc: Some(EuropePmcSupplement {
                    citation_ids: vec!["old-citation".to_string()],
                    reference_ids: vec!["old-reference".to_string()],
                    biomedical_entities: vec![BiomedicalEntity {
                        name: "old-entity".to_string(),
                        semantic_type: Some("gene".to_string()),
                        frequency: Some(1),
                    }],
                    full_text_path: Some("research/full-text/old.xml".to_string()),
                    enriched_at: Some(Utc::now()),
                    enrichment_attempts: Vec::new(),
                }),
                ..CreateSourceRequest::default()
            },
        )?;

        for failed_field in [
            "citation_ids",
            "reference_ids",
            "biomedical_entities",
            "full_text_path",
        ] {
            let before = get_source(workspace.path(), &source.id)?
                .europe_pmc
                .ok_or_else(|| ResearchError::Invalid("missing prior supplement".to_string()))?;
            let saved = save_europe_pmc_supplement(
                workspace.path(),
                &source.id,
                EuropePmcSupplementUpdate {
                    citation_ids: (failed_field != "citation_ids")
                        .then(|| vec!["new-citation".to_string()]),
                    reference_ids: (failed_field != "reference_ids")
                        .then(|| vec!["new-reference".to_string()]),
                    biomedical_entities: (failed_field != "biomedical_entities").then(|| {
                        vec![BiomedicalEntity {
                            name: "new-entity".to_string(),
                            semantic_type: Some("disease".to_string()),
                            frequency: Some(2),
                        }]
                    }),
                    full_text_path: (failed_field != "full_text_path")
                        .then(|| Some("research/full-text/new.xml".to_string())),
                    attempt: Some(EuropePmcEnrichmentAttempt {
                        attempt_id: failed_field.to_string(),
                        provider: "europe_pmc".to_string(),
                        attempted_at: Utc::now(),
                        successful_fields: Vec::new(),
                        failed_fields: vec![failed_field.to_string()],
                    }),
                },
            )?;
            let after = saved
                .europe_pmc
                .ok_or_else(|| ResearchError::Invalid("missing merged supplement".to_string()))?;

            match failed_field {
                "citation_ids" => assert_eq!(after.citation_ids, before.citation_ids),
                "reference_ids" => assert_eq!(after.reference_ids, before.reference_ids),
                "biomedical_entities" => assert_eq!(
                    after
                        .biomedical_entities
                        .first()
                        .map(|entity| entity.name.as_str()),
                    before
                        .biomedical_entities
                        .first()
                        .map(|entity| entity.name.as_str())
                ),
                "full_text_path" => assert_eq!(after.full_text_path, before.full_text_path),
                _ => return Err(ResearchError::Invalid("unknown test field".to_string())),
            }
            let attempt = after
                .enrichment_attempts
                .last()
                .ok_or_else(|| ResearchError::Invalid("missing enrichment attempt".to_string()))?;
            assert_eq!(attempt.failed_fields, vec![failed_field.to_string()]);
        }
        Ok(())
    }

    #[test]
    fn medical_review_accepts_peco_and_derives_prisma() -> ResearchResult<()> {
        let workspace = temp_workspace()?;
        let source = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "Medical trial".to_string(),
                ..CreateSourceRequest::default()
            },
        )?;
        let mut document = create_review(
            workspace.path(),
            CreateReviewRequest {
                title: "Treatment review".to_string(),
                question: "Does treatment help?".to_string(),
                domain: ReviewDomain::Medical,
            },
        )?;
        assert!(document.record.protocol.pico.is_some());
        document.record.protocol.pico = None;
        document.record.protocol.peco = Some(PecoFramework {
            population: "Adults".to_string(),
            exposure: "Exposure".to_string(),
            comparator: "No exposure".to_string(),
            outcomes: vec!["Mortality".to_string()],
        });
        document.record.source_ids.push(source.id.clone());
        document.record.screening.push(ScreeningDecision {
            source_id: source.id.clone(),
            stage: ScreeningStage::TitleAbstract,
            decision: ScreeningDecisionValue::Include,
            reason: None,
            reviewer: None,
            decided_at: Utc::now(),
        });
        document.record.screening.push(ScreeningDecision {
            source_id: source.id,
            stage: ScreeningStage::FullText,
            decision: ScreeningDecisionValue::Include,
            reason: None,
            reviewer: None,
            decided_at: Utc::now(),
        });
        let saved = save_review(
            workspace.path(),
            &document.record.id,
            document.record.clone(),
            &document.revision,
        )?;
        assert_eq!(saved.prisma_flow.records_identified, 1);
        assert_eq!(saved.prisma_flow.studies_included, 1);
        Ok(())
    }

    #[test]
    fn citation_audit_and_all_report_formats_are_file_backed() -> ResearchResult<()> {
        let workspace = temp_workspace()?;
        let source = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "Evidence paper".to_string(),
                authors: vec!["A. Author".to_string()],
                year: Some(2025),
                doi: Some("10.1000/evidence".to_string()),
                ..CreateSourceRequest::default()
            },
        )?;
        let mut document = create_review(
            workspace.path(),
            CreateReviewRequest {
                title: "Evidence review".to_string(),
                question: "What is supported?".to_string(),
                domain: ReviewDomain::Academic,
            },
        )?;
        document.record.source_ids.push(source.id.clone());
        document.record.screening.push(ScreeningDecision {
            source_id: source.id.clone(),
            stage: ScreeningStage::FullText,
            decision: ScreeningDecisionValue::Include,
            reason: Some("Meets criteria".to_string()),
            reviewer: Some("reviewer".to_string()),
            decided_at: Utc::now(),
        });
        let review_id = document.record.id.clone();
        let saved = save_review(
            workspace.path(),
            &review_id,
            document.record,
            &document.revision,
        )?;
        upsert_evidence(
            workspace.path(),
            UpsertEvidenceRequest {
                source_id: source.id,
                review_id: Some(saved.record.id.clone()),
                dimension: "outcome".to_string(),
                claim: "The outcome improved".to_string(),
                locator: Some("Results, table 2".to_string()),
                ..UpsertEvidenceRequest::default()
            },
        )?;
        let audit = audit_review(workspace.path(), &saved.record.id)?;
        assert_eq!(audit.error_count, 0);
        let artifacts = export_all_review_formats(workspace.path(), &saved.record.id)?;
        assert!(artifacts.len() >= 5);
        for format in [
            ReviewExportFormat::Markdown,
            ReviewExportFormat::Json,
            ReviewExportFormat::Csv,
            ReviewExportFormat::Bibtex,
            ReviewExportFormat::Ris,
        ] {
            assert!(artifacts.iter().any(|artifact| artifact.format == format));
        }
        assert!(
            artifacts
                .iter()
                .all(|artifact| workspace.path().join(&artifact.path).is_file())
        );
        Ok(())
    }

    #[test]
    fn audit_and_export_both_reject_missing_declared_source() -> ResearchResult<()> {
        let workspace = temp_workspace()?;
        let source = create_source(
            workspace.path(),
            CreateSourceRequest {
                title: "Missing later".to_string(),
                ..CreateSourceRequest::default()
            },
        )?;
        let mut review = create_review(
            workspace.path(),
            CreateReviewRequest {
                title: "Missing source review".to_string(),
                question: "Where did it go?".to_string(),
                domain: ReviewDomain::Academic,
            },
        )?;
        review.record.source_ids.push(source.id.clone());
        let review = save_review(
            workspace.path(),
            &review.record.id,
            review.record.clone(),
            &review.revision,
        )?;
        fs::remove_file(source_path(workspace.path(), &source.id)?)?;

        let audit = audit_review(workspace.path(), &review.record.id)?;
        assert!(audit.issues.iter().any(|issue| {
            issue.code == "missing_source" && issue.source_id.as_deref() == Some(source.id.as_str())
        }));
        assert!(matches!(
            export_review(
                workspace.path(),
                &review.record.id,
                ReviewExportFormat::Markdown
            ),
            Err(ResearchError::NotFound(_))
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn pandoc_renderer_produces_pdf_and_docx_bytes() -> ResearchResult<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = temp_workspace()?;
        let binary = temp.path().join("pandoc-fixture");
        fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nout=\"\"\nprev=\"\"\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"--output\" ]; then out=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nprintf 'rendered-document' > \"$out\"\n",
        )?;
        let mut permissions = fs::metadata(&binary)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions)?;

        for format in [ReviewExportFormat::Pdf, ReviewExportFormat::Docx] {
            let bytes = render_review_document_with_renderer(
                "# Review\n",
                format,
                DocumentRenderer::Pandoc {
                    binary: binary.clone(),
                    pdf_engine: Some("fixture-engine".to_string()),
                },
            )?;
            assert_eq!(bytes, b"rendered-document");
        }
        Ok(())
    }
}
