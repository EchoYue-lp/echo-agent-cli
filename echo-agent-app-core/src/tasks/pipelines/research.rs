//! Research pipeline -- Graph workflow for academic research with revision loop.
//!
//! Graph topology:
//! ```text
//! init -> search_prompt -> search -> merge_prompt -> merge -> fetch_prompt -> fetch
//!   -> synthesize_prompt -> synthesize -> write_prompt -> write_paper
//!   -> review_prompt -> review_paper -> evaluate_quality ─┬─> finalize
//!                                                          └─> revise_prompt -> revise_paper
//!                                                              -> increment_revision -> review_prompt (loop)
//! ```
//!
//! Uses the framework's GraphBuilder + SharedState for checkpoint/resume support.
//! Agent stages use `add_shared_agent_node_with_mode` for `SharedAgent` integration.
//! Each agent stage is a PAIR: a prompt-construction function node followed by
//! a shared-agent node that reads the prompt and writes the output.

use crate::agent_handle::AgentHandle;
use echo_agent::workflow::{Graph, GraphBuilder, SharedAgent, SharedState};
use futures::future::BoxFuture;

// Re-export quality utilities from shared module
use super::quality::extract_quality_score;

/// Configuration for the research pipeline.
#[derive(Debug, Clone)]
pub struct ResearchConfig {
    /// Research topic to search and synthesize.
    pub topic: String,
    /// Maximum number of papers to search and analyze.
    pub max_papers: usize,
    /// Maximum number of revision iterations (review -> revise loops).
    pub max_revisions: u32,
    /// Quality score threshold (0-100). If below this, loop back to revise.
    pub quality_threshold: u32,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            topic: String::new(),
            max_papers: 20,
            max_revisions: 3,
            quality_threshold: 70,
        }
    }
}

impl ResearchConfig {
    /// Create a config with the topic and default values.
    pub fn new(topic: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            ..Self::default()
        }
    }

    /// Set the maximum number of papers.
    pub fn with_max_papers(mut self, max: usize) -> Self {
        self.max_papers = max;
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

// ── Build the Research Graph ───────────────────────────────────────────────────

/// Build the research pipeline as a Graph workflow.
///
/// Constructs a pipeline with a conditional revision loop using the canonical
/// prompt-construction + `add_shared_agent_node_with_mode` pattern:
///
/// ```text
/// init -> search_prompt -> search -> merge_prompt -> merge -> fetch_prompt -> fetch
///   -> synthesize_prompt -> synthesize -> write_prompt -> write_paper
///   -> review_prompt -> review_paper -> evaluate_quality ─┬─> finalize
///                                                          └─> revise_prompt -> revise_paper
///                                                              -> increment_revision -> review_prompt (loop)
/// ```
pub fn build_research_graph(agent: SharedAgent) -> anyhow::Result<Graph> {
    let agent_search = agent.clone();
    let agent_merge = agent.clone();
    let agent_fetch = agent.clone();
    let agent_synthesize = agent.clone();
    let agent_write = agent.clone();
    let agent_review = agent.clone();
    let agent_revise = agent.clone();

    let graph = GraphBuilder::new("research_pipeline")
        // ── Init: store config values and prompt templates in state ──
        .add_function_node("init", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let topic: String = state.get("topic").unwrap_or_default();
                let max_papers: i64 = state.get("max_papers").unwrap_or(20);

                state.set(
                    "tpl_search",
                    format!(
                        "Search for the top {max_papers} academic papers about: {topic}\n\
                         For each paper found, extract:\n\
                         - Title\n\
                         - Authors\n\
                         - Year of publication\n\
                         - Abstract or key findings\n\
                         - Paper ID (arxiv ID or DOI if available)\n\
                         - Citation count if available\n\n\
                         Cover both ArXiv and Semantic Scholar sources.\n\
                         Return results as structured text organized by source."
                    ),
                )?;

                state.set(
                    "tpl_merge",
                    "Organize and merge the following search results. Remove duplicates and \
                     present as a unified, well-structured summary organized by relevance and topic.",
                )?;

                state.set(
                    "tpl_fetch",
                    format!(
                        "You have the following merged search results about: {topic}\n\n\
                         Your task: DOWNLOAD AND READ the actual papers, not just the abstracts.\n\n\
                         Instructions:\n\
                         1. From the search results below, identify the top {top_n} most relevant \
                         and impactful papers based on citation count and relevance to the topic.\n\
                         2. For each selected paper, use the pdf_fetch tool to download and read \
                         the full text. Use the PDF URL provided in the search results \
                         (for ArXiv papers, the URL format is typically https://arxiv.org/pdf/<arxiv_id>).\n\
                         3. After reading each paper, extract:\n\
                            - Key findings and main contributions\n\
                            - Methodology and approach details\n\
                            - Specific results, metrics, or evidence\n\
                            - Limitations and gaps identified by the authors\n\
                            - How it relates to the research topic: {topic}\n\
                         4. If a PDF URL is not available or download fails, use the abstract \
                         and note that full text was not accessible.\n\
                         5. Prioritize depth over breadth: it is better to thoroughly analyze \
                         {top_n} papers than superficially skim {max_papers}.\n\n\
                         Use pdf_fetch when it is available and a full-text URL can be verified. Do not claim a \
                         full-text reading when only an abstract or snippet was accessible; label the evidence level \
                         for each paper.",
                        top_n = (max_papers / 2).clamp(3, 10),
                    ),
                )?;

                state.set(
                    "tpl_synthesize",
                    format!(
                        "Synthesize the analyzed sources into a literature review on: {topic}\n\n\
                         Treat each upstream analysis according to its stated access level; do not assume full text. \
                         Use only verifiable bibliographic details and evidence present in the inputs. Include:\n\
                         1. Introduction and background context\n\
                         2. Key themes and approaches in the field\n\
                         3. Comparison of methodologies across papers (use specific details from the full texts)\n\
                         4. Major findings and contributions (cite specific results and metrics)\n\
                         5. Identified gaps, contradictions, and future directions\n\n\
                         Use consistent numbered citations [1], [2], etc. Tie material claims to specific studies, \
                         distinguish source findings from synthesis, preserve disagreement, and state search/scope limitations."
                    ),
                )?;

                state.set(
                    "tpl_write",
                    format!(
                        "Draft an academic paper on {topic} using the supplied literature review as the evidence base. \
                         Do not invent a novel method, experiment, result, or citation when none is supplied. Include:\n\
                         1. Title\n\
                         2. Abstract (150-250 words)\n\
                         3. Introduction\n\
                         4. Related Work (based on the literature review)\n\
                         5. Methodology / Approach\n\
                         6. Analysis / Discussion\n\
                         7. Conclusion and Future Work\n\
                         8. References\n\n\
                         Maintain an academic tone and logical argument. Every citation must map to an actual source in \
                         the review; label proposed methodology or future work as proposed rather than completed."
                    ),
                )?;

                state.set(
                    "tpl_review",
                    format!(
                        "Peer-review the supplied draft on {topic}. Check contribution, factual/citation support, \
                         methodology, internal consistency, evidence strength, limitations, and whether claims exceed \
                         results. Begin exactly with QUALITY_SCORE: <0-100>, then provide:\n\
                         - strengths worth preserving\n\
                         - concrete defects ordered by impact\n\
                         - section-specific revisions and any citation requiring verification."
                    ),
                )?;

                state.set(
                    "tpl_revise",
                    "Revise the complete paper using the review as a set of claims to evaluate. Correct valid issues, \
                     preserve supported material, remove or qualify unsupported claims, and never invent evidence or \
                     citations to satisfy feedback. Return the full revised paper.",
                )?;

                Ok(())
            })
        })
        // ── Stage 1: Search for papers ──
        .add_function_node("search_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_search").unwrap_or_default();
                state.set("search_prompt", tpl)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "search",
            agent_search,
            "search_prompt",
            "search_results",
            false, // chat mode
        )
        // ── Stage 2: Merge and organize search results ──
        .add_function_node("merge_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_merge").unwrap_or_default();
                let results: String = state.get("search_results").unwrap_or_default();
                let prompt = format!("{}\n\nSearch Results:\n{}", tpl, results);
                state.set("merge_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "merge",
            agent_merge,
            "merge_prompt",
            "merged_results",
            false,
        )
        // ── Stage 3: Fetch and analyze paper content ──
        .add_function_node("fetch_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_fetch").unwrap_or_default();
                let merged: String = state.get("merged_results").unwrap_or_default();
                let prompt = format!("{}\n\n{}", tpl, merged);
                state.set("fetch_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "fetch",
            agent_fetch,
            "fetch_prompt",
            "fetched_papers",
            false,
        )
        // ── Stage 4: Synthesize literature review ──
        .add_function_node("synthesize_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_synthesize").unwrap_or_default();
                let papers: String = state.get("fetched_papers").unwrap_or_default();
                let prompt = format!("{}\n\nPapers analyzed:\n{}", tpl, papers);
                state.set("synthesize_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "synthesize",
            agent_synthesize,
            "synthesize_prompt",
            "literature_review",
            false,
        )
        // ── Stage 5: Write initial paper draft ──
        .add_function_node("write_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_write").unwrap_or_default();
                let review: String = state.get("literature_review").unwrap_or_default();
                let prompt = format!("{}\n\nLiterature Review:\n{}", tpl, review);
                state.set("write_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "write_paper",
            agent_write,
            "write_prompt",
            "paper_draft",
            false,
        )
        // ── Stage 6: Review paper ──
        .add_function_node("review_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_review").unwrap_or_default();
                let draft: String = state.get("paper_draft").unwrap_or_default();
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is revision round {revision_count}.\n\nPaper Draft:\n{draft}",
                    tpl,
                );
                state.set("review_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "review_paper",
            agent_review,
            "review_prompt",
            "review_feedback",
            false,
        )
        // ── Stage 7: Evaluate quality from review ──
        .add_function_node("evaluate_quality", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let review_text: String = state.get("review_feedback").unwrap_or_default();
                let score = extract_quality_score(&review_text);
                state.set("quality_score", score as i64)?;

                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                tracing::info!(
                    pipeline = "research",
                    quality_score = score,
                    revision_count = revision_count,
                    "Review quality evaluated"
                );
                Ok(())
            })
        })
        // ── Stage 8: Revise paper (conditional loop) ──
        .add_function_node("revise_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_revise").unwrap_or_default();
                let draft: String = state.get("paper_draft").unwrap_or_default();
                let review: String = state.get("review_feedback").unwrap_or_default();
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is revision round {revision_count}.\n\nOriginal Draft:\n{draft}\n\nReviewer Feedback:\n{review}",
                    tpl,
                );
                state.set("revise_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "revise_paper",
            agent_revise,
            "revise_prompt",
            "paper_draft", // overwrite draft with revised version
            false,
        )
        // ── Increment revision counter ──
        .add_function_node("increment_revision", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let count: i64 = state.get("revision_count").unwrap_or(0);
                let new_count = count + 1;
                state.set("revision_count", new_count)?;
                tracing::info!(
                    pipeline = "research",
                    revision = new_count,
                    "Revision iteration completed"
                );
                Ok(())
            })
        })
        // ── Stage 9: Finalize ──
        .add_function_node("finalize", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let draft: String = state.get("paper_draft").unwrap_or_default();
                let quality_score: i64 = state.get("quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);

                tracing::info!(
                    pipeline = "research",
                    quality_score = quality_score,
                    revision_count = revision_count,
                    "Finalizing research pipeline output"
                );

                let final_output = format!(
                    "{draft}\n\n---\n\
                     **Quality Score**: {quality_score}/100\n\
                     **Revision Rounds**: {revision_count}"
                );
                state.set("final_output", final_output)?;
                Ok(())
            })
        })
        // ── Edges ──
        .set_entry("init")
        .add_edge("init", "search_prompt")
        .add_edge("search_prompt", "search")
        .add_edge("search", "merge_prompt")
        .add_edge("merge_prompt", "merge")
        .add_edge("merge", "fetch_prompt")
        .add_edge("fetch_prompt", "fetch")
        .add_edge("fetch", "synthesize_prompt")
        .add_edge("synthesize_prompt", "synthesize")
        .add_edge("synthesize", "write_prompt")
        .add_edge("write_prompt", "write_paper")
        .add_edge("write_paper", "review_prompt")
        .add_edge("review_prompt", "review_paper")
        .add_edge("review_paper", "evaluate_quality")
        // Conditional branch: evaluate_quality -> finalize or revise
        .add_conditional_edge("evaluate_quality", |state: &SharedState| {
            Box::pin(async move {
                let quality_score: i64 = state.get("quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("revision_count").unwrap_or(0);
                let threshold: i64 = state.get("quality_threshold").unwrap_or(70);
                let max_revs: i64 = state.get("max_revisions").unwrap_or(3);

                if quality_score >= threshold {
                    tracing::info!(
                        pipeline = "research",
                        quality_score = quality_score,
                        threshold = threshold,
                        "Quality threshold met -- proceeding to finalize"
                    );
                    "finalize".to_string()
                } else if revision_count < max_revs {
                    tracing::info!(
                        pipeline = "research",
                        quality_score = quality_score,
                        revision_count = revision_count,
                        "Quality below threshold -- looping to revise"
                    );
                    "revise_prompt".to_string()
                } else {
                    tracing::info!(
                        pipeline = "research",
                        revision_count = revision_count,
                        max_revisions = max_revs,
                        "Max revisions reached -- proceeding to finalize"
                    );
                    "finalize".to_string()
                }
            })
        })
        // Revision loop: revise -> increment_revision -> review_prompt (re-builds prompt)
        .add_edge("revise_prompt", "revise_paper")
        .add_edge("revise_paper", "increment_revision")
        .add_edge("increment_revision", "review_prompt")
        .set_finish("finalize")
        .build()?;

    Ok(graph)
}

// ── Pipeline Execution ─────────────────────────────────────────────────────────

/// Execute the research pipeline for a given topic with default config.
pub async fn run_research(
    agent: AgentHandle,
    topic: &str,
    max_papers: usize,
) -> anyhow::Result<String> {
    let config = ResearchConfig::new(topic).with_max_papers(max_papers);
    run_research_with_config(agent, config).await
}

/// Execute the research pipeline with full configuration.
///
/// Returns the final output string containing the paper draft along with
/// quality score and revision count metadata.
pub async fn run_research_with_config(
    agent: AgentHandle,
    config: ResearchConfig,
) -> anyhow::Result<String> {
    let shared_agent = agent.as_shared_agent().await;
    let graph = build_research_graph(shared_agent)?;
    let state = SharedState::new();

    state.set("topic", config.topic.clone())?;
    state.set("max_papers", config.max_papers as i64)?;
    state.set("revision_count", 0i64)?;
    state.set("max_revisions", config.max_revisions as i64)?;
    state.set("quality_threshold", config.quality_threshold as i64)?;

    tracing::info!(
        pipeline = "research",
        topic = %config.topic,
        max_papers = config.max_papers,
        max_revisions = config.max_revisions,
        quality_threshold = config.quality_threshold,
        "Starting research pipeline"
    );

    let result = graph.run(state).await?;

    tracing::info!(
        pipeline = "research",
        steps = result.steps,
        path = ?result.path,
        "Research pipeline completed"
    );

    let final_output: String = result.state.get("final_output").unwrap_or_else(|| {
        result.state.get("paper_draft").unwrap_or_else(|| {
            "Research pipeline completed but no paper was generated.".to_string()
        })
    });

    Ok(final_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_research_config_defaults() {
        let config = ResearchConfig::default();
        assert!(config.topic.is_empty());
        assert_eq!(config.max_papers, 20);
        assert_eq!(config.max_revisions, 3);
        assert_eq!(config.quality_threshold, 70);
    }

    #[test]
    fn test_research_config_builder() {
        let config = ResearchConfig::new("LLM agents")
            .with_max_papers(50)
            .with_max_revisions(5)
            .with_quality_threshold(80);
        assert_eq!(config.topic, "LLM agents");
        assert_eq!(config.max_papers, 50);
        assert_eq!(config.max_revisions, 5);
        assert_eq!(config.quality_threshold, 80);
    }

    #[test]
    fn test_research_config_clone() {
        let config = ResearchConfig::new("test topic");
        let cloned = config.clone();
        assert_eq!(cloned.topic, "test topic");
        assert_eq!(cloned.max_papers, 20);
    }
}
