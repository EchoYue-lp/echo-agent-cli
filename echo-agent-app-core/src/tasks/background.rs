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
}

/// Workspace write policy for parallel background tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWritePolicy {
    /// Rely on existing tool permission, sandbox, and read-before-edit guards.
    #[default]
    Guarded,
    /// Task should avoid writing workspace files.
    ReadOnly,
    /// Caller explicitly allows workspace writes.
    AllowWrites,
}

/// Shell/sandbox execution policy for parallel background tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxExecutionPolicy {
    /// Use existing shared sandbox/tool execution limits and permissions.
    #[default]
    SharedLimited,
    /// Task should avoid shell/sandbox execution.
    Disabled,
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
    /// Task priority (0-10, higher = more urgent). Default: 5.
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// List of task IDs this task depends on. Task will not start until
    /// all dependencies reach `Completed` status.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Workspace write policy for parallel execution safety.
    #[serde(default)]
    pub workspace_write_policy: WorkspaceWritePolicy,
    /// Shell/sandbox policy for parallel execution safety.
    #[serde(default)]
    pub sandbox_execution_policy: SandboxExecutionPolicy,
}

fn default_priority() -> u8 {
    5
}

impl BackgroundTaskKind {
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

    /// Parse a kind from a tag string.
    pub fn from_tag(tag: &str) -> Option<&str> {
        tag.strip_prefix(BG_KIND_TAG_PREFIX)
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

    /// Return the Agent mode name best suited for this task kind.
    ///
    /// Used by the unified dispatch to configure the task agent's system
    /// prompt and available tools.
    pub fn mode_name(&self) -> &str {
        match self {
            Self::Research { .. } | Self::ResearchToWriting { .. } => "research",
            Self::DataPipeline { .. } => "data",
            Self::WritingPipeline { .. } => "writing",
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
                "Research the following topic thoroughly. Find up to {max_papers} relevant \
                 academic papers using arxiv_search and semantic_scholar_search, fetch and \
                 read the most important ones, then write a comprehensive literature review \
                 with proper citations.\n\nTopic: {topic}"
            ),

            Self::ResearchToWriting {
                topic,
                max_papers,
                audience,
                format,
                ..
            } => format!(
                "First, research the topic below by finding up to {max_papers} academic \
                 papers. Then, write a {format} about it for {audience}. The final document \
                 should be well-structured with proper citations and a reference list.\n\n\
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
                    "Analyze the dataset at '{dataset_path}'. {obj}. Generate up to \
                     {max_charts} charts to visualize the key findings. Use read_data to \
                     load the dataset, profile_data for statistics, and generate_chart for \
                     visualizations. End with an executive summary."
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

impl BackgroundTaskMeta {
    pub fn new(kind: BackgroundTaskKind, submitted_via: Option<String>) -> Self {
        Self {
            kind,
            progress: 0,
            progress_message: None,
            submitted_at: chrono::Utc::now().to_rfc3339(),
            submitted_via,
            priority: default_priority(),
            depends_on: Vec::new(),
            workspace_write_policy: WorkspaceWritePolicy::default(),
            sandbox_execution_policy: SandboxExecutionPolicy::default(),
        }
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority.min(10);
        self
    }

    pub fn with_dependencies(mut self, depends_on: Vec<String>) -> Self {
        self.depends_on = depends_on;
        self
    }

    pub fn with_workspace_write_policy(mut self, policy: WorkspaceWritePolicy) -> Self {
        self.workspace_write_policy = policy;
        self
    }

    pub fn with_sandbox_execution_policy(mut self, policy: SandboxExecutionPolicy) -> Self {
        self.sandbox_execution_policy = policy;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_task_meta_defaults_to_guarded_parallel_resource_policies() {
        let meta = BackgroundTaskMeta::new(
            BackgroundTaskKind::Research {
                topic: "hello".to_string(),
                max_papers: 1,
                output_format: ResearchOutputFormat::default(),
            },
            Some("test".to_string()),
        );

        assert_eq!(meta.workspace_write_policy, WorkspaceWritePolicy::Guarded);
        assert_eq!(
            meta.sandbox_execution_policy,
            SandboxExecutionPolicy::SharedLimited
        );
    }
}
