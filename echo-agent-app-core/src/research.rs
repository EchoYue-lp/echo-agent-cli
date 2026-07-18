//! File-backed research library and systematic-review workbench.
//!
//! Research artifacts are ordinary workspace files under `research/` so they
//! remain inspectable, versionable, and usable by GUI, TUI, CLI, and agents.

use std::collections::BTreeSet;
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
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub url: Option<String>,
    pub pdf_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub provenance: Vec<SourceProvenance>,
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
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub url: Option<String>,
    pub pdf_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub provenance: Vec<SourceProvenance>,
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
    request.pmcid = normalize_identifier(request.pmcid, &["pmcid:"]);
    request.arxiv_id = normalize_identifier(request.arxiv_id, &["arxiv:"]);
    request.openalex_id = normalize_identifier(
        request.openalex_id,
        &["https://openalex.org/", "http://openalex.org/"],
    );
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
        year: request.year,
        venue: clean_optional(request.venue),
        url: clean_optional(request.url),
        pdf_path: clean_optional(request.pdf_path),
        tags: normalized_values(request.tags),
        notes: clean_optional(request.notes),
        provenance: request.provenance,
        added_at: now,
        updated_at: now,
    };
    write_json(&source_path(workspace_root, &source.id)?, &source)?;
    Ok(source)
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
        },
        source_ids: Vec::new(),
        screening: Vec::new(),
        risk_of_bias: Vec::new(),
        grade: Vec::new(),
        prisma: PrismaSupplement::default(),
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
    if record.domain == ReviewDomain::Medical && record.protocol.pico.is_none() {
        return Err(ResearchError::Invalid(
            "medical reviews require a PICO framework".to_string(),
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
    fn medical_review_requires_pico_and_derives_prisma() -> ResearchResult<()> {
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
}
