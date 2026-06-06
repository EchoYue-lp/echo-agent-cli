//! Data analysis pipeline — Graph workflow for dataset analysis.
//!
//! Graph topology (canonical prompt + shared-agent-node pattern):
//! ```text
//! init -> load_prompt -> load_data -> profile_prompt -> profile
//!   -> analyze_prompt -> analyze -> visualize_prompt -> visualize
//!   -> summarize_prompt -> summarize
//! ```
//!
//! Each agent stage is a PAIR: a prompt-construction `add_function_node`
//! followed by `add_shared_agent_node_with_mode` that reads the prompt
//! from state and writes the agent output to another state key.

use crate::agent_handle::AgentHandle;
use echo_agent::workflow::{Graph, GraphBuilder, SharedAgent, SharedState};
use futures::future::BoxFuture;

// ── Configuration ──────────────────────────────────────────────────────────────

/// Configuration for the data analysis pipeline.
#[derive(Debug, Clone)]
pub struct DataPipelineConfig {
    /// Path to the dataset (CSV, JSON, etc.)
    pub dataset_path: String,
    /// Optional analysis objective / question to focus on.
    pub objective: Option<String>,
    /// Maximum number of charts/visualizations to suggest.
    pub max_charts: usize,
}

impl Default for DataPipelineConfig {
    fn default() -> Self {
        Self {
            dataset_path: String::new(),
            objective: None,
            max_charts: 3,
        }
    }
}

impl DataPipelineConfig {
    /// Create a config with the dataset path and default values.
    pub fn new(dataset_path: impl Into<String>) -> Self {
        Self {
            dataset_path: dataset_path.into(),
            ..Self::default()
        }
    }

    /// Set an analysis objective.
    pub fn with_objective(mut self, objective: impl Into<String>) -> Self {
        self.objective = Some(objective.into());
        self
    }

    /// Set the maximum number of charts.
    pub fn with_max_charts(mut self, max: usize) -> Self {
        self.max_charts = max;
        self
    }
}

// ── Build the Data Analysis Graph ──────────────────────────────────────────────

/// Build the data analysis pipeline as a Graph workflow.
///
/// Constructs a 5-stage pipeline using the canonical prompt-construction +
/// shared-agent-node pattern:
///
/// ```text
/// init -> load_prompt -> load_data -> profile_prompt -> profile
///   -> analyze_prompt -> analyze -> visualize_prompt -> visualize
///   -> summarize_prompt -> summarize
/// ```
///
/// Each stage is a pair: a `add_function_node` that composes the prompt and
/// stores it at a state key, followed by `add_shared_agent_node_with_mode`
/// that reads the prompt from that key and writes the agent output to another
/// key.
pub fn build_data_graph(shared: SharedAgent, max_charts: usize) -> anyhow::Result<Graph> {
    let shared_load = shared.clone();
    let shared_profile = shared.clone();
    let shared_analyze = shared.clone();
    let shared_visualize = shared.clone();
    let shared_summarize = shared.clone();
    let max_charts_i64 = max_charts as i64;

    let graph = GraphBuilder::new("data_analysis_pipeline")
        // ── Init: store config values in state ──
        .add_function_node("init", move |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                state.set("max_charts", max_charts_i64)?;
                Ok(())
            })
        })
        // ── Stage 1: Load and describe data ──
        .add_function_node("load_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let dataset_path: String = state.get("dataset_path").unwrap_or_default();
                let objective: String = state.get("objective").unwrap_or_default();
                let prompt = format!(
                    "You are a data analyst. Describe the dataset at '{}'.\n\
                     What are the columns, data types, shape, and any obvious issues?\n\
                     {objective_section}",
                    dataset_path,
                    objective_section = if objective.is_empty() { String::new() } else {
                        format!("Focus especially on aspects relevant to: {}", objective)
                    }
                );
                state.set("load_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode("load_data", shared_load, "load_prompt", "data_description", false)
        // ── Stage 2: Statistical profiling ──
        .add_function_node("profile_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let description: String = state.get("data_description").unwrap_or_default();
                let prompt = format!(
                    "Given this data description:\n{description}\n\n\
                     Compute a statistical profile:\n\
                     - Column distributions (mean, median, std dev for numeric; frequency counts for categorical)\n\
                     - Missing value counts per column\n\
                     - Correlations between numeric columns\n\
                     - Data quality issues and anomalies\n\
                     Present the profile in a structured format."
                );
                state.set("profile_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode("profile", shared_profile, "profile_prompt", "data_profile", false)
        // ── Stage 3: Analysis ──
        .add_function_node("analyze_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let description: String = state.get("data_description").unwrap_or_default();
                let profile: String = state.get("data_profile").unwrap_or_default();
                let objective: String = state.get("objective").unwrap_or_default();
                let prompt = format!(
                    "Analyze this data and identify patterns, outliers, and insights:\n\n\
                     Data Description:\n{description}\n\n\
                     Statistical Profile:\n{profile}\n\
                     {objective_section}",
                    objective_section = if objective.is_empty() { String::new() } else {
                        format!("\nFocus the analysis on: {}", objective)
                    }
                );
                state.set("analyze_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode("analyze", shared_analyze, "analyze_prompt", "analysis_result", false)
        // ── Stage 4: Visualization suggestions ──
        .add_function_node("visualize_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let analysis: String = state.get("analysis_result").unwrap_or_default();
                let max_charts: i64 = state.get("max_charts").unwrap_or(3);
                let prompt = format!(
                    "Given this analysis:\n{analysis}\n\n\
                     Suggest {max_charts} visualizations that best illustrate these findings.\n\
                     For each visualization, specify:\n\
                     - Chart type (bar, line, scatter, heatmap, etc.)\n\
                     - Which columns to use\n\
                     - What insight it reveals\n\
                     - Suggested title and axis labels"
                );
                state.set("visualize_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode("visualize", shared_visualize, "visualize_prompt", "visualization_suggestions", false)
        // ── Stage 5: Executive summary ──
        .add_function_node("summarize_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let analysis: String = state.get("analysis_result").unwrap_or_default();
                let viz: String = state.get("visualization_suggestions").unwrap_or_default();
                let prompt = format!(
                    "Create a concise executive summary of these data findings:\n\n\
                     Analysis:\n{analysis}\n\n\
                     Recommended Visualizations:\n{viz}\n\n\
                     The summary should include:\n\
                     1. Key findings (top 3-5 insights)\n\
                     2. Data quality assessment\n\
                     3. Recommendations for further investigation\n\
                     4. Suggested next steps"
                );
                state.set("summarize_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode("summarize", shared_summarize, "summarize_prompt", "final_output", false)
        // ── Edges ──
        .set_entry("init")
        .add_edge("init", "load_prompt")
        .add_edge("load_prompt", "load_data")
        .add_edge("load_data", "profile_prompt")
        .add_edge("profile_prompt", "profile")
        .add_edge("profile", "analyze_prompt")
        .add_edge("analyze_prompt", "analyze")
        .add_edge("analyze", "visualize_prompt")
        .add_edge("visualize_prompt", "visualize")
        .add_edge("visualize", "summarize_prompt")
        .add_edge("summarize_prompt", "summarize")
        .set_finish("summarize")
        .build()?;

    Ok(graph)
}

// ── Pipeline Execution ─────────────────────────────────────────────────────────

/// Execute the data analysis pipeline with default config.
pub async fn run_data_pipeline(
    agent: AgentHandle,
    dataset_path: &str,
    max_charts: usize,
) -> anyhow::Result<String> {
    let config = DataPipelineConfig::new(dataset_path).with_max_charts(max_charts);
    run_data_pipeline_with_config(agent, config).await
}

/// Execute the data analysis pipeline with full configuration.
///
/// Returns the final output string containing the executive summary
/// and insights.
///
/// Converts the `AgentHandle` to a `SharedAgent` internally for use
/// with `add_shared_agent_node_with_mode`.
pub async fn run_data_pipeline_with_config(
    agent: AgentHandle,
    config: DataPipelineConfig,
) -> anyhow::Result<String> {
    let shared = agent.as_shared_agent().await;
    let graph = build_data_graph(shared, config.max_charts)?;
    let state = SharedState::new();

    state.set("dataset_path", config.dataset_path.clone())?;
    if let Some(ref objective) = config.objective {
        state.set("objective", objective.clone())?;
    } else {
        state.set("objective", String::new())?;
    }

    tracing::info!(
        pipeline = "data_analysis",
        dataset_path = %config.dataset_path,
        objective = ?config.objective,
        max_charts = config.max_charts,
        "Starting data analysis pipeline"
    );

    let result = graph.run(state).await?;

    tracing::info!(
        pipeline = "data_analysis",
        steps = result.steps,
        path = ?result.path,
        "Data analysis pipeline completed"
    );

    let final_output: String = result.state.get("final_output").unwrap_or_else(|| {
        "Data analysis pipeline completed but no summary was generated.".to_string()
    });

    Ok(final_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_pipeline_config_defaults() {
        let config = DataPipelineConfig::default();
        assert!(config.dataset_path.is_empty());
        assert!(config.objective.is_none());
        assert_eq!(config.max_charts, 3);
    }

    #[test]
    fn test_data_pipeline_config_builder() {
        let config = DataPipelineConfig::new("data.csv")
            .with_objective("find trends")
            .with_max_charts(5);
        assert_eq!(config.dataset_path, "data.csv");
        assert_eq!(config.objective, Some("find trends".to_string()));
        assert_eq!(config.max_charts, 5);
    }

    #[test]
    fn test_data_pipeline_config_clone() {
        let config = DataPipelineConfig::new("test.json").with_objective("analysis");
        let cloned = config.clone();
        assert_eq!(cloned.dataset_path, "test.json");
        assert_eq!(cloned.objective, Some("analysis".to_string()));
    }
}
