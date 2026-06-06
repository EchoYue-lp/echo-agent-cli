//! Writing pipeline — Graph workflow for document writing with revision loop.
//!
//! Graph topology:
//! ```text
//! init -> outline_prompt -> outline -> draft_prompt -> draft
//!   -> review_prompt -> review -> evaluate_quality ─┬─> finalize (quality >= threshold or max_revisions)
//!                                                    └─> revise_prompt -> revise -> increment_revision -> review_prompt (loop)
//! ```
//!
//! Uses the canonical `add_shared_agent_node_with_mode` pattern:
//! each agent stage is a PAIR of (prompt-construction fn node, agent node).
//! The `init` node stores prompt templates in state; prompt-construction nodes
//! enrich templates with dynamic content from previous stages.

use crate::agent_handle::AgentHandle;
use echo_agent::workflow::{Graph, GraphBuilder, SharedAgent, SharedState};
use futures::future::BoxFuture;

// Use shared quality extraction
use super::quality::extract_quality_score;

/// Configuration for the writing pipeline.
#[derive(Debug, Clone)]
pub struct WritingPipelineConfig {
    /// Topic or subject of the document.
    pub topic: String,
    /// Target audience.
    pub audience: String,
    /// Output format (markdown, latex, plain text, etc.).
    pub format: String,
    /// Maximum revision iterations (review -> revise loops).
    pub max_revisions: u32,
    /// Quality score threshold (0-100). If below this, loop back to revise.
    pub quality_threshold: u32,
}

impl Default for WritingPipelineConfig {
    fn default() -> Self {
        Self {
            topic: String::new(),
            audience: "general audience".into(),
            format: "markdown".into(),
            max_revisions: 3,
            quality_threshold: 70,
        }
    }
}

impl WritingPipelineConfig {
    /// Create a config with the topic and default values.
    pub fn new(topic: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            ..Self::default()
        }
    }

    /// Set the target audience.
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = audience.into();
        self
    }

    /// Set the output format.
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = format.into();
        self
    }

    /// Set the maximum number of revision iterations.
    pub fn with_max_revisions(mut self, max: u32) -> Self {
        self.max_revisions = max;
        self
    }

    /// Set the quality threshold score (0-100).
    pub fn with_quality_threshold(mut self, threshold: u32) -> Self {
        self.quality_threshold = threshold;
        self
    }
}

// ── Build the Writing Graph ────────────────────────────────────────────────────

/// Build the writing pipeline as a Graph workflow.
///
/// Constructs a pipeline with a conditional revision loop using the canonical
/// `add_shared_agent_node_with_mode` pattern:
///
/// ```text
/// init -> outline_prompt -> outline -> draft_prompt -> draft
///   -> review_prompt -> review -> evaluate_quality ─┬─> finalize
///                                                    └─> revise_prompt -> revise -> increment_revision -> review_prompt (loop)
/// ```
///
/// Each agent stage is a PAIR: a prompt-construction `add_function_node`
/// followed by `add_shared_agent_node_with_mode`. The `init` node stores
/// prompt templates in state; prompt-construction nodes enrich templates
/// with dynamic content from previous stages.
pub fn build_writing_graph(agent: &SharedAgent) -> anyhow::Result<Graph> {
    let agent_clone = agent.clone();

    let graph = GraphBuilder::new("writing_pipeline")
        // ── Init: store config values and prompt templates in state ──
        .add_function_node("init", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                // Config values are pre-set in state by run_writing_pipeline_with_config().
                let topic: String = state.get("topic").unwrap_or_default();
                let audience: String = state.get("audience").unwrap_or_else(|| "general audience".to_string());
                let format: String = state.get("format").unwrap_or_else(|| "markdown".to_string());

                // Store prompt templates for downstream nodes
                let _ = state.set(
                    "tpl_outline",
                    format!(
                        "Create a detailed outline for a document on the topic '{}'.\n\
                         Target audience: {}\n\
                         Format: {}\n\n\
                         The outline should include:\n\
                         1. Title suggestion\n\
                         2. Main sections with subsections\n\
                         3. Key points to cover in each section\n\
                         4. Logical flow and transitions\n\
                         5. Estimated length per section",
                        topic, audience, format,
                    ),
                );
                let _ = state.set(
                    "tpl_draft",
                    format!(
                        "Write a full draft based on the outline provided.\n\
                         Topic: {}\n\
                         Audience: {}\n\
                         Format: {}\n\n\
                         Write the complete document following the outline structure. \
                         Be thorough, coherent, and well-organized.",
                        topic, audience, format,
                    ),
                );
                let _ = state.set(
                    "tpl_review",
                    format!(
                        "You are an editor reviewing a document on: {}\n\n\
                         Review the draft and provide:\n\
                         1. Overall quality score (0-100) — at the very beginning of your response, \
                         output exactly: QUALITY_SCORE: <number>\n\
                         2. Strengths (what works well)\n\
                         3. Weaknesses (what needs improvement)\n\
                         4. Specific, actionable suggestions for improvement",
                        topic,
                    ),
                );
                let _ = state.set(
                    "tpl_revise",
                    "You are a revision specialist. Revise the following document based on the \
                     reviewer feedback. Address every point raised by the reviewer. Improve \
                     clarity, coherence, and quality. Provide the complete revised document.",
                );
                Ok(())
            })
        })
        // ── Stage 1: Outline ──
        .add_function_node("outline_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_outline").unwrap_or_default();
                let _ = state.set("outline_prompt", tpl);
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "outline",
            agent_clone.clone(),
            "outline_prompt",
            "outline",
            false, // chat mode
        )
        // ── Stage 2: Draft ──
        .add_function_node("draft_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_draft").unwrap_or_default();
                let outline_text: String = state.get("outline").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the outline to follow:\n{}",
                    tpl, outline_text,
                );
                let _ = state.set("draft_prompt", prompt);
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "draft",
            agent_clone.clone(),
            "draft_prompt",
            "draft",
            false,
        )
        // ── Stage 3: Review ──
        .add_function_node("review_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_review").unwrap_or_default();
                let draft_text: String = state.get("draft").unwrap_or_default();
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is revision round {}.\n\nHere is the draft to review:\n{}",
                    tpl, revision_count, draft_text,
                );
                let _ = state.set("review_prompt", prompt);
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "review",
            agent_clone.clone(),
            "review_prompt",
            "review",
            false,
        )
        // ── Evaluate quality from review ──
        .add_function_node("evaluate_quality", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let review_text: String = state.get("review").unwrap_or_default();
                let score = extract_quality_score(&review_text);
                let _ = state.set("quality_score", score as i64);

                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                tracing::info!(
                    pipeline = "writing",
                    quality_score = score,
                    revision_count = revision_count,
                    "Review quality evaluated"
                );
                Ok(())
            })
        })
        // ── Stage 4: Revise (conditional loop) ──
        .add_function_node("revise_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_revise").unwrap_or_default();
                let draft_text: String = state.get("draft").unwrap_or_default();
                let review_text: String = state.get("review").unwrap_or_default();
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is revision round {}.\n\nHere is the current draft:\n{}\n\nHere is the reviewer feedback:\n{}",
                    tpl, revision_count, draft_text, review_text,
                );
                let _ = state.set("revise_prompt", prompt);
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "revise",
            agent_clone.clone(),
            "revise_prompt",
            "draft", // overwrite draft with revised version
            false,
        )
        // ── Increment revision counter ──
        .add_function_node("increment_revision", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let count: i64 = state.get("revision_count").unwrap_or(0);
                let new_count = count + 1;
                let _ = state.set("revision_count", new_count);
                tracing::info!(
                    pipeline = "writing",
                    revision = new_count,
                    "Revision iteration completed"
                );
                Ok(())
            })
        })
        // ── Finalize: format output (no agent call needed) ──
        .add_function_node("finalize", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let draft: String = state.get("draft").unwrap_or_default();
                let quality_score: i64 = state.get("quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);

                tracing::info!(
                    pipeline = "writing",
                    quality_score = quality_score,
                    revision_count = revision_count,
                    "Finalizing writing pipeline output"
                );

                let final_output = format!(
                    "{draft}\n\n---\n\
                     **Quality Score**: {quality_score}/100\n\
                     **Revision Rounds**: {revision_count}"
                );
                let _ = state.set("final_output", final_output);
                Ok(())
            })
        })
        // ── Edges ──
        // Linear path: init -> outline_prompt -> outline -> draft_prompt -> draft -> review_prompt -> review -> evaluate_quality
        .set_entry("init")
        .add_edge("init", "outline_prompt")
        .add_edge("outline_prompt", "outline")
        .add_edge("outline", "draft_prompt")
        .add_edge("draft_prompt", "draft")
        .add_edge("draft", "review_prompt")
        .add_edge("review_prompt", "review")
        .add_edge("review", "evaluate_quality")
        // Conditional branch: evaluate_quality -> finalize or revise_prompt
        .add_conditional_edge("evaluate_quality", |state: &SharedState| {
            Box::pin(async move {
                let quality_score: i64 = state.get("quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let threshold: i64 = state.get("quality_threshold").unwrap_or(70);
                let max_revs: i64 = state.get("max_revisions").unwrap_or(3);

                if quality_score >= threshold {
                    tracing::info!(
                        pipeline = "writing",
                        quality_score = quality_score,
                        threshold = threshold,
                        "Quality threshold met — proceeding to finalize"
                    );
                    "finalize".to_string()
                } else if revision_count < max_revs {
                    tracing::info!(
                        pipeline = "writing",
                        quality_score = quality_score,
                        revision_count = revision_count,
                        "Quality below threshold — looping to revise"
                    );
                    "revise_prompt".to_string()
                } else {
                    tracing::info!(
                        pipeline = "writing",
                        revision_count = revision_count,
                        max_revisions = max_revs,
                        "Max revisions reached — proceeding to finalize"
                    );
                    "finalize".to_string()
                }
            })
        })
        // Revision loop: revise_prompt -> revise -> increment_revision -> review_prompt
        .add_edge("revise_prompt", "revise")
        .add_edge("revise", "increment_revision")
        .add_edge("increment_revision", "review_prompt")
        .set_finish("finalize")
        .build()?;

    Ok(graph)
}

// ── Pipeline Execution ─────────────────────────────────────────────────────────

/// Execute the writing pipeline for a given topic with default config.
pub async fn run_writing_pipeline(agent: AgentHandle, topic: &str) -> anyhow::Result<String> {
    let config = WritingPipelineConfig::new(topic);
    run_writing_pipeline_with_config(agent, config).await
}

/// Execute the writing pipeline with full configuration.
///
/// Returns the final output string containing the revised document along with
/// quality score and revision count metadata.
pub async fn run_writing_pipeline_with_config(
    agent: AgentHandle,
    config: WritingPipelineConfig,
) -> anyhow::Result<String> {
    // Convert AgentHandle -> SharedAgent for the canonical pattern
    let shared_agent = agent.as_shared_agent().await;
    let graph = build_writing_graph(&shared_agent)?;
    let state = SharedState::new();

    state.set("topic", config.topic.clone())?;
    state.set("audience", config.audience.clone())?;
    state.set("format", config.format.clone())?;
    state.set("revision_count", 0i64)?;
    state.set("max_revisions", config.max_revisions as i64)?;
    state.set("quality_threshold", config.quality_threshold as i64)?;

    tracing::info!(
        pipeline = "writing",
        topic = %config.topic,
        audience = %config.audience,
        format = %config.format,
        max_revisions = config.max_revisions,
        quality_threshold = config.quality_threshold,
        "Starting writing pipeline"
    );

    let result = graph.run(state).await?;

    tracing::info!(
        pipeline = "writing",
        steps = result.steps,
        path = ?result.path,
        "Writing pipeline completed"
    );

    let final_output: String = result.state.get("final_output").unwrap_or_else(|| {
        result
            .state
            .get("draft")
            .unwrap_or_else(|| "Writing pipeline completed but no draft was generated.".to_string())
    });

    Ok(final_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_writing_pipeline_config_defaults() {
        let config = WritingPipelineConfig::default();
        assert!(config.topic.is_empty());
        assert_eq!(config.audience, "general audience");
        assert_eq!(config.format, "markdown");
        assert_eq!(config.max_revisions, 3);
        assert_eq!(config.quality_threshold, 70);
    }

    #[test]
    fn test_writing_pipeline_config_builder() {
        let config = WritingPipelineConfig::new("Rust async")
            .with_audience("developers")
            .with_format("latex")
            .with_max_revisions(5)
            .with_quality_threshold(85);
        assert_eq!(config.topic, "Rust async");
        assert_eq!(config.audience, "developers");
        assert_eq!(config.format, "latex");
        assert_eq!(config.max_revisions, 5);
        assert_eq!(config.quality_threshold, 85);
    }

    #[test]
    fn test_writing_pipeline_config_clone() {
        let config = WritingPipelineConfig::new("test");
        let cloned = config.clone();
        assert_eq!(cloned.topic, "test");
        assert_eq!(cloned.format, "markdown");
    }
}
