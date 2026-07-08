//! Pipeline module
//!
//! Provides predefined workflow implementations: research, data analysis,
//! writing, and research-to-writing continuous workflow.

pub mod data;
pub mod quality;
pub mod research;
pub mod research_to_writing;
pub mod writing;

pub use data::{
    DataPipelineConfig, build_data_graph, run_data_pipeline, run_data_pipeline_with_config,
};
pub use quality::{
    QualityAssessment, extract_json_block, extract_quality_assessment, extract_quality_score,
    extract_quality_score_legacy, quality_assessment_prompt,
};
pub use research::{ResearchConfig, build_research_graph, run_research, run_research_with_config};
pub use research_to_writing::{ResearchToWritingConfig, run_research_to_writing};
pub use writing::{
    WritingPipelineConfig, build_writing_graph, run_writing_pipeline,
    run_writing_pipeline_with_config,
};
