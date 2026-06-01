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

pub use data::{
    DataPipelineConfig, build_data_graph, run_data_pipeline, run_data_pipeline_with_config,
};
pub use loader::{
    PipelineDefinition, PipelineLoader, default_pipeline_dir, load_builtin_pipelines,
};
pub use research::{
    QualityAssessment, ResearchConfig, build_research_graph, extract_quality_assessment,
    extract_quality_score, quality_assessment_prompt, run_research, run_research_with_config,
};
pub use research_to_writing::{
    R2WWritingQualityAssessment, ResearchToWritingConfig, extract_r2w_writing_quality_assessment,
    run_research_to_writing,
};
pub use template_engine::{PromptTemplateEngine, paths as template_paths};
pub use writing::{
    WritingPipelineConfig, WritingQualityAssessment, build_writing_graph,
    extract_writing_quality_assessment, extract_writing_quality_score, run_writing_pipeline,
    run_writing_pipeline_with_config, writing_quality_assessment_prompt,
};
