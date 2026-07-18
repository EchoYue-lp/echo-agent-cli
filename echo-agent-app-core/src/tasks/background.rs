//! Background trigger kinds converted into TaskRuntime prompts.
//!
//! **No default timeout**: tasks run until completion unless the caller
//! explicitly sets one. Long-running research pipelines or multi-hour data
//! analysis jobs are first-class citizens.

use serde::{Deserialize, Serialize};

use super::task_runtime::DomainProfile;

/// Tag prefix persisted in TaskRuntime trigger metadata.
pub const BG_KIND_TAG_PREFIX: &str = "bg:kind:";

/// Discriminator for what kind of work a background task performs.
///
/// Persisted in the TaskRuntime run route and trigger event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params")]
pub enum BackgroundTaskKind {
    /// Research pipeline: paper search -> fetch -> synthesize -> write (with revision loop).
    Research {
        topic: String,
        #[serde(default = "default_max_papers")]
        max_papers: usize,
        #[serde(default)]
        output_format: ResearchOutputFormat,
    },

    /// Research-to-Writing continuous workflow: research pipeline output
    /// feeds directly into a writing pipeline for end-to-end document production.
    ResearchToWriting {
        topic: String,
        #[serde(default = "default_max_papers")]
        max_papers: usize,
        #[serde(default = "default_audience")]
        audience: String,
        #[serde(default = "default_format")]
        format: String,
        #[serde(default = "default_research_max_revisions")]
        research_max_revisions: u32,
        #[serde(default = "default_research_quality_threshold")]
        research_quality_threshold: u32,
        #[serde(default = "default_rtw_writing_max_revisions")]
        writing_max_revisions: u32,
        #[serde(default = "default_rtw_writing_quality")]
        writing_quality_threshold: u32,
    },

    /// File-backed analysis pipeline: contract -> script -> execute -> lineage -> report.
    DataPipeline {
        /// Path to the dataset (CSV, JSON, etc.)
        dataset_path: String,
        /// Optional analysis objective
        #[serde(skip_serializing_if = "Option::is_none")]
        objective: Option<String>,
        #[serde(default = "default_max_charts")]
        max_charts: usize,
    },

    /// Writing pipeline: outline -> draft -> review-revise loop -> finalize.
    WritingPipeline {
        /// Topic or subject of the document
        topic: String,
        #[serde(default = "default_audience")]
        audience: String,
        #[serde(default = "default_format")]
        format: String,
        #[serde(default = "default_wp_max_revisions")]
        max_revisions: u32,
        #[serde(default = "default_wp_quality_threshold")]
        quality_threshold: u32,
    },
}

fn default_max_papers() -> usize {
    20
}

fn default_audience() -> String {
    "academic peers".to_string()
}

fn default_format() -> String {
    "academic paper".to_string()
}

fn default_research_max_revisions() -> u32 {
    3
}

fn default_research_quality_threshold() -> u32 {
    70
}

fn default_rtw_writing_max_revisions() -> u32 {
    2
}

fn default_rtw_writing_quality() -> u32 {
    80
}

fn default_wp_max_revisions() -> u32 {
    3
}

fn default_wp_quality_threshold() -> u32 {
    70
}

fn default_max_charts() -> usize {
    3
}

/// Output format for research/paper writing pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResearchOutputFormat {
    #[default]
    Markdown,
    Latex,
}

impl BackgroundTaskKind {
    pub fn domain_profile(&self) -> DomainProfile {
        match self {
            Self::Research { .. } | Self::ResearchToWriting { .. } => {
                DomainProfile::AcademicResearch
            }
            Self::DataPipeline { .. } => DomainProfile::DataAnalysis,
            Self::WritingPipeline { .. } => DomainProfile::General,
        }
    }

    /// Return the tag string for this kind (used in `Task.tags`).
    pub fn tag(&self) -> String {
        let kind_name = match self {
            BackgroundTaskKind::Research { .. } => "research",
            BackgroundTaskKind::ResearchToWriting { .. } => "research_to_writing",
            BackgroundTaskKind::DataPipeline { .. } => "data_pipeline",
            BackgroundTaskKind::WritingPipeline { .. } => "writing_pipeline",
        };
        format!("{BG_KIND_TAG_PREFIX}{kind_name}")
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            BackgroundTaskKind::Research { .. } => "Research",
            BackgroundTaskKind::ResearchToWriting { .. } => "Research to Writing",
            BackgroundTaskKind::DataPipeline { .. } => "Data Analysis",
            BackgroundTaskKind::WritingPipeline { .. } => "Writing",
        }
    }

    /// Build a natural-language prompt from this task kind's parameters.
    ///
    /// Converts structured task parameters (topic, max_papers, etc.) into
    /// a descriptive prompt that the Agent can execute autonomously.
    pub fn to_prompt(&self) -> String {
        match self {
            Self::Research {
                topic, max_papers, ..
            } => format!(
                "Conduct a reproducible literature review on the topic below. Create an academic \
                 systematic-review record with research_library before synthesis. Record the \
                 question, eligibility criteria, databases, exact search queries, search dates, \
                 and result counts. Find up to {max_papers} relevant sources, normalize DOI, \
                 arXiv, PMID/PMCID, or OpenAlex identifiers, and persist every source with \
                 research_library. Screen each source with explicit include/exclude reasons. \
                 Persist claim-level evidence with quotations or section/page locators, then \
                 write a cited review from those evidence records. Do not cite a source that is \
                 absent from the library or make a claim without linked evidence.\n\nTopic: {topic}"
            ),

            Self::ResearchToWriting {
                topic,
                max_papers,
                audience,
                format,
                ..
            } => format!(
                "First build a reproducible academic review in research_library: define the \
                 protocol, find and normalize up to {max_papers} sources, record searches and \
                 screening reasons, and persist claim-level evidence with locators. Then write a \
                 {format} for {audience} using only the persisted evidence. The final document \
                 must have audited citations and a reference list.\n\n\
                 Topic: {topic}"
            ),

            Self::DataPipeline {
                dataset_path,
                objective,
                max_charts,
            } => {
                let obj = objective.as_deref().unwrap_or(
                    "Provide a comprehensive overview with key statistics, \
                                trends, and insights",
                );
                format!(
                    "Create a reproducible file-backed analysis for the workspace-relative \
                     dataset '{dataset_path}'. Objective: {obj}. Use the analysis contract under \
                     analysis/<analysis-id>/: manifest.json, a persisted analysis.py or \
                     analysis.R script, outputs/, environment.json, result.json, runs/, and \
                     latest-run.json. Set contract_version to 1, put '{dataset_path}' in manifest \
                     input_paths, record structured parameters and a random seed, and resolve \
                     generated paths from the script directory. Perform all transformations, \
                     statistical methods, diagnostics, and up to {max_charts} charts in the \
                     reviewable script. Execute the persisted file with run_code using \
                     script_path, not inline code. Record package/runtime versions, script/input/\
                     output SHA-256 hashes, exit status, assumptions, missing-data handling, \
                     warnings, and diagnostics. Treat exploratory_statistics as exploratory only; \
                     formal inference must use a mature pinned library. If execution or a \
                     dependency fails, preserve a structured failed run instead of fabricating \
                     results. End with an executive summary that points to the saved artifacts."
                )
            }

            Self::WritingPipeline {
                topic,
                audience,
                format,
                max_revisions,
                quality_threshold,
            } => format!(
                "Write a {format} about '{topic}' for {audience}. Follow this workflow:\n\
                 1. Create a detailed outline\n2. Write a complete draft following the outline\n\
                 3. Review the draft critically (score quality 0-100)\n\
                 4. If quality < {quality_threshold}, revise and re-review (up to \
                 {max_revisions} revisions)\n\
                 5. Finalize the document"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_kinds_select_domain_profiles() {
        let data = BackgroundTaskKind::DataPipeline {
            dataset_path: "data.csv".to_string(),
            objective: None,
            max_charts: 2,
        };
        let research = BackgroundTaskKind::Research {
            topic: "agents".to_string(),
            max_papers: 5,
            output_format: ResearchOutputFormat::Markdown,
        };
        assert_eq!(data.domain_profile(), DomainProfile::DataAnalysis);
        assert_eq!(research.domain_profile(), DomainProfile::AcademicResearch);
        let data_prompt = data.to_prompt();
        assert!(data_prompt.contains("run_code using script_path"));
        assert!(data_prompt.contains("contract_version to 1"));
        assert!(data_prompt.contains("script/input/output SHA-256 hashes"));
        assert!(data_prompt.contains("structured failed run"));
        assert!(research.to_prompt().contains("research_library"));
    }
}
