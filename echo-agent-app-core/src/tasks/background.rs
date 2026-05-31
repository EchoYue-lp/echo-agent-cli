//! Background task kind discriminator and metadata.
//!
//! The framework's `Task` type handles state machine, persistence, events,
//! DAG scheduling, and retry. This module adds the "what to execute" dimension
//! via `BackgroundTaskKind` — a tagged enum that maps to different execution
//! strategies in `BackgroundTaskService`.
//!
//! **No default timeout**: tasks run until completion unless the caller
//! explicitly sets one. Long-running research pipelines or multi-hour data
//! analysis jobs are first-class citizens.

use serde::{Deserialize, Serialize};

/// Tag prefix used to identify background task kinds on the framework's `Task.tags`.
pub const BG_KIND_TAG_PREFIX: &str = "bg:kind:";

/// Discriminator for what kind of work a background task performs.
///
/// Stored as a tag on the framework's `Task` (e.g. `"bg:kind:agent_chat"`)
/// and as the `kind` field in `BackgroundTaskMeta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params")]
pub enum BackgroundTaskKind {
    /// One-shot agent chat: submit a prompt, stream the response.
    AgentChat {
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Recurring cron job: fires on a schedule.
    /// Each firing creates a child `AgentChat` task for tracking.
    Cron {
        cron_expr: String,
        prompt: String,
    },

    /// Multi-step workflow: executes a Graph workflow definition.
    Workflow {
        workflow_id: String,
        #[serde(default)]
        input: serde_json::Value,
    },

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

    /// Data analysis pipeline: load -> profile -> analyze -> visualize -> summarize.
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

    /// Composite task: chains multiple tasks together with dependencies.
    /// Tasks execute in the specified order, with output from one task
    /// available as input to the next.
    Composite {
        /// List of sub-tasks to execute in sequence
        steps: Vec<CompositeStep>,
        /// Execution strategy: "sequential" (default) or "parallel"
        #[serde(default = "default_strategy")]
        strategy: CompositeStrategy,
    },
}

/// A single step in a composite task chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositeStep {
    /// Task kind for this step
    pub kind: BackgroundTaskKind,
    /// Optional description override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Input mapping: which output keys from previous steps to use
    #[serde(default)]
    pub input_from: Vec<String>,
}

/// Execution strategy for composite tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompositeStrategy {
    #[default]
    Sequential,
    Parallel,
}

fn default_strategy() -> CompositeStrategy {
    CompositeStrategy::Sequential
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
    7
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
    7
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

/// Serializable metadata stored alongside the framework's `Task`.
///
/// Stored as JSON in a companion SQLite namespace keyed by task ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskMeta {
    /// What kind of work this task performs.
    pub kind: BackgroundTaskKind,
    /// Progress percentage (0-100), updated by the executor.
    pub progress: u8,
    /// Human-readable progress message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_message: Option<String>,
    /// When this task was submitted (ISO 8601).
    pub submitted_at: String,
    /// Which interface submitted this (web, cli, tui, tauri).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_via: Option<String>,
}

impl BackgroundTaskKind {
    /// Return the tag string for this kind (used in `Task.tags`).
    pub fn tag(&self) -> String {
        let kind_name = match self {
            BackgroundTaskKind::AgentChat { .. } => "agent_chat",
            BackgroundTaskKind::Cron { .. } => "cron",
            BackgroundTaskKind::Workflow { .. } => "workflow",
            BackgroundTaskKind::Research { .. } => "research",
            BackgroundTaskKind::ResearchToWriting { .. } => "research_to_writing",
            BackgroundTaskKind::DataPipeline { .. } => "data_pipeline",
            BackgroundTaskKind::WritingPipeline { .. } => "writing_pipeline",
            BackgroundTaskKind::Composite { .. } => "composite",
        };
        format!("{BG_KIND_TAG_PREFIX}{kind_name}")
    }

    /// Parse a kind from a tag string.
    pub fn from_tag(tag: &str) -> Option<&str> {
        tag.strip_prefix(BG_KIND_TAG_PREFIX)
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            BackgroundTaskKind::AgentChat { .. } => "Agent Chat",
            BackgroundTaskKind::Cron { .. } => "Cron Job",
            BackgroundTaskKind::Workflow { .. } => "Workflow",
            BackgroundTaskKind::Research { .. } => "Research",
            BackgroundTaskKind::ResearchToWriting { .. } => "Research to Writing",
            BackgroundTaskKind::DataPipeline { .. } => "Data Analysis",
            BackgroundTaskKind::WritingPipeline { .. } => "Writing",
            BackgroundTaskKind::Composite { .. } => "Composite Task",
        }
    }
}

impl BackgroundTaskMeta {
    pub fn new(kind: BackgroundTaskKind, submitted_via: Option<String>) -> Self {
        Self {
            kind,
            progress: 0,
            progress_message: None,
            submitted_at: chrono::Utc::now().to_rfc3339(),
            submitted_via,
        }
    }
}
