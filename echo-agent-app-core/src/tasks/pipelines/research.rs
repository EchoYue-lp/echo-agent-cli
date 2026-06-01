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

// ── Configuration ──────────────────────────────────────────────────────────────

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

// ── Quality Score Extraction (Structured Output) ───────────────────────────────

/// Structured quality assessment from the review stage.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct QualityAssessment {
    /// Overall quality score (0-100).
    #[serde(default = "default_quality_score")]
    pub quality_score: u32,
    /// Confidence in the assessment (0.0-1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Brief summary of the assessment.
    #[serde(default)]
    pub summary: String,
    /// Specific suggestions for improvement.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Whether the output needs revision.
    #[serde(default)]
    pub needs_revision: bool,
}

fn default_quality_score() -> u32 {
    60
}
fn default_confidence() -> f64 {
    0.5
}

/// Extract structured quality assessment from review text.
///
/// Primary strategy: parse JSON code block from the review.
/// Fallback: heuristic regex scanning (legacy behavior).
pub fn extract_quality_assessment(review_text: &str) -> QualityAssessment {
    // Strategy 1: Extract fenced JSON block
    if let Some(json_str) = extract_json_block(review_text) {
        if let Ok(assessment) = serde_json::from_str::<QualityAssessment>(&json_str) {
            tracing::info!(
                pipeline = "research",
                quality_score = assessment.quality_score,
                confidence = assessment.confidence,
                "Parsed structured quality assessment"
            );
            return assessment;
        }
    }

    // Strategy 2: Try parsing the entire text as JSON
    if let Ok(assessment) = serde_json::from_str::<QualityAssessment>(review_text.trim()) {
        return assessment;
    }

    // Strategy 3: Fallback to legacy regex extraction
    let score = extract_quality_score_legacy(review_text);
    QualityAssessment {
        quality_score: score,
        confidence: 0.3,
        summary: "Extracted via legacy regex".to_string(),
        suggestions: vec![],
        needs_revision: score < 70,
    }
}

/// Extract the quality score (backward-compatible wrapper).
///
/// Tries structured JSON first, falls back to regex.
pub fn extract_quality_score(review_text: &str) -> u32 {
    extract_quality_assessment(review_text).quality_score
}

/// Returns the prompt suffix for structured quality assessment.
///
/// Append this to the review prompt to get JSON output.
pub fn quality_assessment_prompt() -> &'static str {
    r#"

IMPORTANT: After your review, output a JSON assessment block in this exact format:

```json
{
  "quality_score": <0-100>,
  "confidence": <0.0-1.0>,
  "summary": "<brief summary>",
  "suggestions": ["<suggestion 1>", "<suggestion 2>"],
  "needs_revision": <true/false>
}
```"#
}

/// Extract a JSON code block from markdown text.
fn extract_json_block(text: &str) -> Option<String> {
    // Look for ```json ... ``` or ```JSON ... ```
    let markers = ["```json", "```JSON"];
    for marker in &markers {
        if let Some(start_idx) = text.find(marker) {
            let after_marker = &text[start_idx + marker.len()..];
            if let Some(end_idx) = after_marker.find("```") {
                let json_str = after_marker[..end_idx].trim();
                return Some(json_str.to_string());
            }
        }
    }
    // Try bare ``` blocks
    if let Some(start_idx) = text.find("```") {
        let after = &text[start_idx + 3..];
        // Skip optional language tag
        let content_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after[content_start..];
        if let Some(end_idx) = content.find("```") {
            let json_str = content[..end_idx].trim();
            // Only return if it looks like JSON
            if json_str.starts_with('{') {
                return Some(json_str.to_string());
            }
        }
    }
    None
}

/// Legacy regex-based quality score extraction (kept as fallback).
fn extract_quality_score_legacy(review_text: &str) -> u32 {
    // Primary: look for "QUALITY_SCORE: <number>" pattern
    for line in review_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("QUALITY_SCORE:") {
            let rest = rest.trim();
            if let Ok(score) = rest.parse::<u32>() {
                return score.min(100);
            }
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(score) = digits.parse::<u32>() {
                return score.min(100);
            }
        }
    }

    // Fallback heuristic: look for "Score:" prefix
    for line in review_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Score:") {
            if let Ok(score) = rest.trim().parse::<u32>() {
                return score.min(100);
            }
        }
    }

    // Fallback heuristic: look for "Quality Score" or "quality score" phrases
    for line in review_text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("quality score") {
            if let Some(pos) = lower.find("quality score") {
                let rest = &line[pos..];
                let digits: String = rest
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(score) = digits.parse::<u32>() {
                    return score.min(100);
                }
            }
        }
    }

    tracing::warn!(
        pipeline = "research",
        "Could not extract quality score from review text; defaulting to 60"
    );
    60
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
                        "Based on the following search results, identify the most relevant \
                         and impactful papers (up to {max_papers}). For each selected paper:\n\
                         1. Summarize the key findings and contributions\n\
                         2. Note the methodology and approach\n\
                         3. Identify how it relates to the research topic\n\
                         4. Note any limitations or gaps identified by the authors"
                    ),
                )?;

                state.set(
                    "tpl_synthesize",
                    format!(
                        "You are writing a comprehensive literature review on: {topic}\n\n\
                         Based on the following analyzed papers, write a structured literature review \
                         that includes:\n\
                         1. Introduction and background context\n\
                         2. Key themes and approaches in the field\n\
                         3. Comparison of methodologies across papers\n\
                         4. Major findings and contributions\n\
                         5. Identified gaps, contradictions, and future directions\n\n\
                         Use proper academic citations [1], [2], etc.\n\
                         Ensure each claim is grounded in the papers analyzed."
                    ),
                )?;

                state.set(
                    "tpl_write",
                    format!(
                        "You are writing an academic paper on: {topic}\n\n\
                         Using the literature review below, write a complete paper draft with:\n\
                         1. Title\n\
                         2. Abstract (150-250 words)\n\
                         3. Introduction\n\
                         4. Related Work (based on the literature review)\n\
                         5. Methodology / Approach\n\
                         6. Analysis / Discussion\n\
                         7. Conclusion and Future Work\n\
                         8. References\n\n\
                         Maintain academic tone, proper citations, and logical flow.\n\
                         Ensure every citation references an actual paper from the review."
                    ),
                )?;

                state.set(
                    "tpl_review",
                    format!(
                        "You are a peer reviewer evaluating an academic paper on: {topic}\n\n\
                         Review the following paper draft and provide:\n\
                         1. Overall quality score (0-100) -- at the very beginning of your response, \
                         output exactly: QUALITY_SCORE: <number>\n\
                         2. Strengths (what works well)\n\
                         3. Weaknesses (what needs improvement)\n\
                         4. Specific, actionable suggestions for each section\n\n\
                         Be thorough and constructive."
                    ),
                )?;

                state.set(
                    "tpl_revise",
                    "You are a revision specialist.\n\n\
                     Revise the following paper draft based on the reviewer feedback.\n\
                     Address every point raised by the reviewer. Improve clarity, rigor, \
                     citation accuracy, and overall quality.\n\n\
                     Provide the complete revised paper with improvements.",
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
