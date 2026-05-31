//! Pipeline module
//!
//! Provides predefined workflow implementations: research, data analysis,
//! writing, and research-to-writing continuous workflow.

pub mod data;
pub mod loader;
pub mod research;
pub mod research_to_writing;
pub mod template_engine;
pub mod writing;

pub use data::{build_data_graph, run_data_pipeline, run_data_pipeline_with_config, DataPipelineConfig};
pub use loader::{default_pipeline_dir, load_builtin_pipelines, PipelineDefinition, PipelineLoader};
pub use research::{build_research_graph, extract_quality_score, extract_quality_assessment, quality_assessment_prompt, run_research, run_research_with_config, QualityAssessment, ResearchConfig};
pub use research_to_writing::{run_research_to_writing, extract_r2w_writing_quality_assessment, R2WWritingQualityAssessment, ResearchToWritingConfig};
pub use template_engine::{PromptTemplateEngine, paths as template_paths};
pub use writing::{build_writing_graph, extract_writing_quality_score, extract_writing_quality_assessment, writing_quality_assessment_prompt, run_writing_pipeline, run_writing_pipeline_with_config, WritingQualityAssessment, WritingPipelineConfig};