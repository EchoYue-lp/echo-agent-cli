//! Research-to-Writing continuous workflow
//!
//! Chains the research pipeline output into a writing pipeline, producing
//! a publication-ready document from research through final polish.
//!
//! Graph topology:
//! ```text
//! ┌── Research Pipeline ───────────────────────────────────────────────────┐
//! │ init -> search_arxiv_prompt -> search_semantic_prompt -> start_search  │
//! │   ──┬─> search_arxiv ──┬─> merge_results -> fetch_prompt -> fetch     │
//! │      └─> search_semantic┘                                             │
//! │   -> synthesize_prompt -> synthesize -> write_prompt -> write_paper   │
//! │   -> review_prompt -> review_paper -> evaluate_research_quality       │
//! │     ─┬─> bridge                                                        │
//! │      └─> revise_prompt -> revise_paper -> increment_research_revision │
//! │         -> review_prompt (loop)                                        │
//! └───────────────────────────────────────────────────────────────────────┘
//!       -> bridge -> outline_prompt -> outline -> draft_prompt
//!       -> writing_draft -> writing_review_prompt -> writing_review
//!       -> evaluate_writing_quality ─┬─> finalize_prompt -> finalize
//!                                     └─> writing_revise_prompt -> writing_revise
//!                                         -> increment_writing_revision
//!                                         -> writing_review_prompt (loop)
//! ```
//!
//! The bridge node connects the two phases by feeding the research output
//! (literature review, paper draft, quality score) into the writing pipeline
//! context, which then produces a polished final document.
//!
//! Uses the canonical prompt-construction + `add_shared_agent_node_with_mode`
//! pattern for all agent-calling stages.

use super::quality::extract_quality_score;
use crate::agent_handle::AgentHandle;
use echo_agent::workflow::{Graph, GraphBuilder, SharedAgent, SharedState};
use futures::future::BoxFuture;

// ── Configuration ──────────────────────────────────────────────────────────────

/// Configuration for the research-to-writing continuous workflow.
///
/// Combines research and writing configuration into a single struct for
/// the end-to-end pipeline.
#[derive(Debug, Clone)]
pub struct ResearchToWritingConfig {
    /// Research topic to search and synthesize.
    pub topic: String,
    /// Maximum number of papers to search and analyze.
    pub max_papers: usize,
    /// Maximum number of revision iterations in the research phase.
    pub research_max_revisions: u32,
    /// Quality threshold (0-100) for the research review phase.
    pub research_quality_threshold: u32,
    /// Target audience for the final written output.
    pub audience: String,
    /// Desired format for the final output (e.g. "academic paper", "blog post", "report").
    pub format: String,
    /// Maximum number of revision iterations in the writing phase.
    pub writing_max_revisions: u32,
    /// Quality threshold (0-100) for the writing review phase.
    pub writing_quality_threshold: u32,
}

impl Default for ResearchToWritingConfig {
    fn default() -> Self {
        Self {
            topic: String::new(),
            max_papers: 20,
            research_max_revisions: 3,
            research_quality_threshold: 70,
            audience: "academic peers".to_string(),
            format: "academic paper".to_string(),
            writing_max_revisions: 2,
            writing_quality_threshold: 80,
        }
    }
}

impl ResearchToWritingConfig {
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

    /// Set research-phase revision parameters.
    pub fn with_research_revisions(mut self, max: u32, threshold: u32) -> Self {
        self.research_max_revisions = max;
        self.research_quality_threshold = threshold;
        self
    }

    /// Set the target audience for the final output.
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = audience.into();
        self
    }

    /// Set the output format.
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = format.into();
        self
    }

    /// Set writing-phase revision parameters.
    pub fn with_writing_revisions(mut self, max: u32, threshold: u32) -> Self {
        self.writing_max_revisions = max;
        self.writing_quality_threshold = threshold;
        self
    }
}

// ── Build the Research-to-Writing Graph ─────────────────────────────────────────

/// Build the research-to-writing continuous workflow as a single Graph.
///
/// This creates a unified graph that combines both the research and writing
/// phases. The `bridge` node connects the two phases by preparing the
/// research output for the writing pipeline.
///
/// All agent-calling stages use the canonical prompt-construction +
/// `add_shared_agent_node_with_mode` pattern. Non-agent stages (merge,
/// evaluate, increment, finalize formatting) remain as function nodes.
pub fn build_research_to_writing_graph(agent: SharedAgent) -> anyhow::Result<Graph> {
    let agent_search_arxiv = agent.clone();
    let agent_search_semantic = agent.clone();
    let agent_fetch = agent.clone();
    let agent_synthesize = agent.clone();
    let agent_write = agent.clone();
    let agent_review = agent.clone();
    let agent_revise = agent.clone();
    let agent_outline = agent.clone();
    let agent_draft = agent.clone();
    let agent_writing_review = agent.clone();
    let agent_writing_revise = agent.clone();
    let agent_finalize = agent.clone();

    let graph = GraphBuilder::new("research_to_writing")
        // ═══════════════════════════════════════════════════════════════
        // INIT: Store research-phase templates in state
        // ═══════════════════════════════════════════════════════════════
        .add_function_node("init", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let topic: String = state.get("topic").unwrap_or_default();
                let max_papers: i64 = state.get("max_papers").unwrap_or(20);

                // Research phase templates
                state.set(
                    "tpl_search_arxiv",
                    format!(
                        "Search arxiv for the top {max_papers} papers about: {topic}\n\
                         For each paper, extract: title, authors, year, abstract, arxiv_id.\n\
                         Return results as a JSON array."
                    ),
                )?;

                state.set(
                    "tpl_search_semantic",
                    format!(
                        "Search Semantic Scholar for the top {max_papers} papers about: {topic}\n\
                         For each paper, extract: title, authors, year, abstract, paper_id, citation_count.\n\
                         Return results as a JSON array."
                    ),
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
                         the full text. Use the PDF URL or paper ID provided in the search results \
                         (for ArXiv papers, construct the URL as https://arxiv.org/pdf/<arxiv_id>).\n\
                         3. After reading each paper, extract:\n\
                            - Key findings and main contributions\n\
                            - Methodology and approach details\n\
                            - Specific results, metrics, or evidence\n\
                            - Limitations and gaps identified by the authors\n\
                            - How it relates to the research topic: {topic}\n\
                         4. If a PDF is not available or download fails, use the abstract \
                         and note that full text was not accessible.\n\
                         5. Prioritize depth over breadth: it is better to thoroughly analyze \
                         {top_n} papers than superficially skim {max_papers}.\n\n\
                         IMPORTANT: You MUST use pdf_fetch to download papers. Do NOT just \
                         summarize the abstracts — read the actual papers.",
                        top_n = (max_papers / 2).clamp(3, 10),
                    ),
                )?;

                state.set(
                    "tpl_synthesize",
                    format!(
                        "You are writing a comprehensive literature review on: {topic}\n\n\
                         IMPORTANT: The following paper analyses are based on FULL TEXT readings, not just abstracts. \
                         Use the detailed findings, methodology descriptions, and specific evidence extracted from the papers.\n\n\
                         Based on the following analyzed papers, write a structured literature review \
                         that includes:\n\
                         1. Introduction and background\n\
                         2. Key themes and approaches in the field\n\
                         3. Comparison of methodologies (use specific details from the full texts)\n\
                         4. Major findings and contributions (cite specific results and metrics)\n\
                         5. Identified gaps and future directions\n\n\
                         Use proper academic citations [1], [2], etc.\n\
                         Reference specific experiments, datasets, or results mentioned in the full texts."
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
                         Maintain academic tone, proper citations, and logical flow."
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
                         4. Specific suggestions for each section\n\n\
                         Be thorough and constructive."
                    ),
                )?;

                state.set(
                    "tpl_revise",
                    "You are a revision specialist.\n\n\
                     Revise the following paper draft based on the reviewer feedback.\n\
                     Address every point raised by the reviewer.\n\n\
                     Original Draft:\n\n\n\
                     Reviewer Feedback:\n\n\n\
                     Provide the complete revised paper with improvements.",
                )?;

                Ok(())
            })
        })

        // ═══════════════════════════════════════════════════════════════
        // RESEARCH PHASE
        // ═══════════════════════════════════════════════════════════════

        // Stage R1: Prompt construction for parallel search (runs before fork)
        .add_function_node("search_arxiv_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_search_arxiv").unwrap_or_default();
                state.set("search_arxiv_prompt", tpl)?;
                Ok(())
            })
        })
        .add_function_node("search_semantic_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_search_semantic").unwrap_or_default();
                state.set("search_semantic_prompt", tpl)?;
                Ok(())
            })
        })
        // Router node: triggers parallel fan-out
        .add_router_node("start_search")
        // Parallel search branches (agent nodes read pre-built prompts)
        .add_shared_agent_node_with_mode(
            "search_arxiv",
            agent_search_arxiv,
            "search_arxiv_prompt",
            "arxiv_results",
            false,
        )
        .add_shared_agent_node_with_mode(
            "search_semantic",
            agent_search_semantic,
            "search_semantic_prompt",
            "semantic_results",
            false,
        )
        // Stage R2: Merge and deduplicate (pure function node, no agent)
        .add_function_node("merge_results", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let arxiv: String = state.get("arxiv_results").unwrap_or_default();
                let semantic: String = state.get("semantic_results").unwrap_or_default();
                let merged = format!(
                    "## ArXiv Results\n{arxiv}\n\n## Semantic Scholar Results\n{semantic}"
                );
                state.set("merged_results", merged)?;
                Ok(())
            })
        })
        // Stage R3: Fetch paper content
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
            "fetch_papers",
            agent_fetch,
            "fetch_prompt",
            "fetched_papers",
            false,
        )
        // Stage R4: Synthesize literature review
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
        // Stage R5: Write initial paper draft
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
        // Stage R6: Review paper (research phase)
        .add_function_node("review_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_review").unwrap_or_default();
                let draft: String = state.get("paper_draft").unwrap_or_default();
                let revision_count: i64 = state.get("research_revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is research revision round {revision_count}.\n\nPaper Draft:\n{draft}",
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
            "research_review_feedback",
            false,
        )
        // Evaluate research quality (pure function node)
        .add_function_node("evaluate_research_quality", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let review_text: String = state.get("research_review_feedback").unwrap_or_default();
                let score = extract_quality_score(&review_text);
                state.set("research_quality_score", score as i64)?;

                let revision_count: i64 = state.get("research_revision_count").unwrap_or(0);
                tracing::info!(
                    pipeline = "research_to_writing",
                    phase = "research",
                    quality_score = score,
                    revision_count = revision_count,
                    "Research review quality evaluated"
                );
                Ok(())
            })
        })
        // Stage R7: Revise paper (research phase loop)
        .add_function_node("revise_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_revise").unwrap_or_default();
                let draft: String = state.get("paper_draft").unwrap_or_default();
                let review: String = state.get("research_review_feedback").unwrap_or_default();
                let revision_count: i64 = state.get("research_revision_count").unwrap_or(0);
                let prompt = format!(
                    "{}\n\nThis is research revision round {revision_count}.\n\nOriginal Draft:\n{draft}\n\nReviewer Feedback:\n{review}",
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
        // Increment research revision counter (pure function node)
        .add_function_node("increment_research_revision", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let count: i64 = state.get("research_revision_count").unwrap_or(0);
                let new_count = count + 1;
                state.set("research_revision_count", new_count)?;
                tracing::info!(
                    pipeline = "research_to_writing",
                    phase = "research",
                    revision = new_count,
                    "Research revision iteration completed"
                );
                Ok(())
            })
        })

        // ═══════════════════════════════════════════════════════════════
        // BRIDGE: Research -> Writing
        // ═══════════════════════════════════════════════════════════════

        // Bridge node: prepare research output for the writing phase
        // (kept as a function node -- stores research context and writing templates)
        .add_function_node("bridge", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let research_draft: String = state.get("paper_draft").unwrap_or_default();
                let literature_review: String = state.get("literature_review").unwrap_or_default();
                let research_score: i64 = state.get("research_quality_score").unwrap_or(0);
                let research_revisions: i64 = state.get("research_revision_count").unwrap_or(0);
                let topic: String = state.get("topic").unwrap_or_default();
                let audience: String = state.get("audience").unwrap_or_else(|| "academic peers".to_string());
                let format: String = state.get("format").unwrap_or_else(|| "academic paper".to_string());

                tracing::info!(
                    pipeline = "research_to_writing",
                    phase = "bridge",
                    research_quality_score = research_score,
                    research_revisions = research_revisions,
                    "Bridging research output to writing phase"
                );

                // Store the research output as context for the writing phase
                state.set("research_context", format!(
                    "## Research Phase Results\n\n\
                     Topic: {topic}\n\
                     Research Quality Score: {research_score}/100\n\
                     Research Revision Rounds: {research_revisions}\n\n\
                     ### Literature Review\n{literature_review}\n\n\
                     ### Research Paper Draft\n{research_draft}"
                ))?;

                // Store prompt templates for the writing phase
                state.set(
                    "tpl_outline",
                    format!(
                        "You are an expert content planner. Based on the research context provided, \
                         create a detailed outline for a {format} on the topic '{topic}' \
                         targeted at {audience}. \
                         The outline should refine and restructure the research findings into a \
                         compelling narrative structure. \
                         Include: title, sections with key points, and logical flow. \
                         Output the outline as structured text."
                    ),
                )?;
                state.set(
                    "tpl_draft",
                    format!(
                        "You are a skilled writer. Based on the outline and research context provided, \
                         write a complete {format} on '{topic}' for {audience}. \
                         Incorporate the research findings, literature review insights, and \
                         maintain academic rigor while ensuring accessibility for the target audience. \
                         Follow the outline structure closely. Write in a clear, engaging style. \
                         Output the full draft."
                    ),
                )?;
                state.set(
                    "tpl_review",
                    "You are a critical reviewer. Review the draft provided and evaluate it on: \
                         clarity, coherence, accuracy, audience fit, academic rigor, and overall quality. \
                         Score the draft from 0 to 100. \
                         At the very beginning of your response, output exactly: \
                         QUALITY_SCORE: <number> \
                         Then provide specific, actionable feedback for improvement. \
                         Output the review with quality score.".to_string(),
                )?;
                state.set(
                    "tpl_revise",
                    format!(
                        "You are a revision specialist. Based on the draft and review feedback \
                         provided, revise the {format} on '{topic}' to address all the reviewer's concerns. \
                         Improve clarity, coherence, accuracy, audience fit, and academic rigor. \
                         Output the revised version of the full content."
                    ),
                )?;
                state.set(
                    "tpl_finalize",
                    format!(
                        "You are a final editor. Polish the content provided into a final, \
                         publication-ready {format} on '{topic}' for {audience}. \
                         Fix any remaining grammar, style, or formatting issues. \
                         Ensure citations are properly formatted. \
                         Output the final polished version."
                    ),
                )?;

                Ok(())
            })
        })

        // ═══════════════════════════════════════════════════════════════
        // WRITING PHASE
        // ═══════════════════════════════════════════════════════════════

        // Stage W1: Outline (based on research context)
        .add_function_node("outline_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_outline").unwrap_or_default();
                let research_context: String = state.get("research_context").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the research context to draw from:\n{}",
                    tpl, research_context,
                );
                state.set("outline_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "outline",
            agent_outline,
            "outline_prompt",
            "outline",
            false,
        )
        // Stage W2: Draft (based on outline + research context)
        .add_function_node("draft_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_draft").unwrap_or_default();
                let outline_text: String = state.get("outline").unwrap_or_default();
                let research_context: String = state.get("research_context").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the outline to follow:\n{}\n\nHere is the research context:\n{}",
                    tpl, outline_text, research_context,
                );
                state.set("draft_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "writing_draft",
            agent_draft,
            "draft_prompt",
            "writing_draft",
            false,
        )
        // Stage W3: Review (writing phase)
        .add_function_node("writing_review_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_review").unwrap_or_default();
                let draft_text: String = state.get("writing_draft").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the draft to review:\n{}",
                    tpl, draft_text,
                );
                state.set("writing_review_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "writing_review",
            agent_writing_review,
            "writing_review_prompt",
            "writing_review",
            false,
        )
        // Evaluate writing quality (pure function node)
        .add_function_node("evaluate_writing_quality", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let review_text: String = state.get("writing_review").unwrap_or_default();
                let score = extract_quality_score(&review_text);
                state.set("writing_quality_score", score as i64)?;

                let revision_count: i64 = state.get("writing_revision_count").unwrap_or(0);
                tracing::info!(
                    pipeline = "research_to_writing",
                    phase = "writing",
                    quality_score = score,
                    revision_count = revision_count,
                    "Writing review quality evaluated"
                );
                Ok(())
            })
        })
        // Stage W4: Revise (writing phase loop)
        .add_function_node("writing_revise_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_revise").unwrap_or_default();
                let draft_text: String = state.get("writing_draft").unwrap_or_default();
                let review_text: String = state.get("writing_review").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the current draft:\n{}\n\nHere is the review feedback:\n{}",
                    tpl, draft_text, review_text,
                );
                state.set("writing_revise_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "writing_revise",
            agent_writing_revise,
            "writing_revise_prompt",
            "writing_draft", // overwrite writing draft with revised version
            false,
        )
        // Increment writing revision counter (pure function node)
        .add_function_node("increment_writing_revision", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let count: i64 = state.get("writing_revision_count").unwrap_or(0);
                let new_count = count + 1;
                state.set("writing_revision_count", new_count)?;
                tracing::info!(
                    pipeline = "research_to_writing",
                    phase = "writing",
                    revision = new_count,
                    "Writing revision iteration completed"
                );
                Ok(())
            })
        })
        // Stage W5: Finalize
        .add_function_node("finalize_prompt", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let tpl: String = state.get("tpl_finalize").unwrap_or_default();
                let draft_text: String = state.get("writing_draft").unwrap_or_default();
                let prompt = format!(
                    "{}\n\nHere is the content to polish:\n{}",
                    tpl, draft_text,
                );
                state.set("finalize_prompt", prompt)?;
                Ok(())
            })
        })
        .add_shared_agent_node_with_mode(
            "finalize",
            agent_finalize,
            "finalize_prompt",
            "final_output",
            false,
        )
        // Finalize metadata node: append quality scores and revision counts
        .add_function_node("finalize_metadata", |state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
            Box::pin(async move {
                let final_output: String = state.get("final_output").unwrap_or_default();
                let research_score: i64 = state.get("research_quality_score").unwrap_or(0);
                let research_revisions: i64 = state.get("research_revision_count").unwrap_or(0);
                let writing_score: i64 = state.get("writing_quality_score").unwrap_or(0);
                let writing_revisions: i64 = state.get("writing_revision_count").unwrap_or(0);

                tracing::info!(
                    pipeline = "research_to_writing",
                    research_quality_score = research_score,
                    research_revisions = research_revisions,
                    writing_quality_score = writing_score,
                    writing_revisions = writing_revisions,
                    "Research-to-writing pipeline completed"
                );

                // Append metadata to final output
                let final_with_metadata = format!(
                    "{final_output}\n\n---\n\
                     **Research Quality Score**: {research_score}/100\n\
                     **Research Revision Rounds**: {research_revisions}\n\
                     **Writing Quality Score**: {writing_score}/100\n\
                     **Writing Revision Rounds**: {writing_revisions}"
                );
                state.set("final_output", final_with_metadata)?;
                Ok(())
            })
        })

        // ── Edges ──
        // Research phase: init -> prompt construction -> parallel search -> merge -> fetch -> synthesize -> write -> review -> evaluate
        .set_entry("init")
        .add_edge("init", "search_arxiv_prompt")
        .add_edge("search_arxiv_prompt", "search_semantic_prompt")
        .add_edge("search_semantic_prompt", "start_search")
        .add_parallel_edge("start_search", vec!["search_arxiv".into(), "search_semantic".into()], "merge_results")
        .add_edge("merge_results", "fetch_prompt")
        .add_edge("fetch_prompt", "fetch_papers")
        .add_edge("fetch_papers", "synthesize_prompt")
        .add_edge("synthesize_prompt", "synthesize")
        .add_edge("synthesize", "write_prompt")
        .add_edge("write_prompt", "write_paper")
        .add_edge("write_paper", "review_prompt")
        .add_edge("review_prompt", "review_paper")
        .add_edge("review_paper", "evaluate_research_quality")
        // Research conditional: evaluate -> bridge or revise
        .add_conditional_edge("evaluate_research_quality", |state: &SharedState| {
            Box::pin(async move {
                let quality_score: i64 = state.get("research_quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("research_revision_count").unwrap_or(0);
                let threshold: i64 = state.get("research_quality_threshold").unwrap_or(70);
                let max_revs: i64 = state.get("research_max_revisions").unwrap_or(3);

                if quality_score >= threshold {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "research",
                        quality_score = quality_score,
                        threshold = threshold,
                        "Research quality threshold met -- bridging to writing"
                    );
                    "bridge".to_string()
                } else if revision_count < max_revs {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "research",
                        quality_score = quality_score,
                        threshold = threshold,
                        revision_count = revision_count,
                        "Research quality below threshold -- looping to revise"
                    );
                    "revise_prompt".to_string()
                } else {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "research",
                        quality_score = quality_score,
                        revision_count = revision_count,
                        max_revisions = max_revs,
                        "Research max revisions reached -- bridging to writing"
                    );
                    "bridge".to_string()
                }
            })
        })
        // Research revise loop: revise -> increment -> re-review (prompt node)
        .add_edge("revise_prompt", "revise_paper")
        .add_edge("revise_paper", "increment_research_revision")
        .add_edge("increment_research_revision", "review_prompt")
        // Bridge -> Writing phase
        .add_edge("bridge", "outline_prompt")
        .add_edge("outline_prompt", "outline")
        .add_edge("outline", "draft_prompt")
        .add_edge("draft_prompt", "writing_draft")
        .add_edge("writing_draft", "writing_review_prompt")
        .add_edge("writing_review_prompt", "writing_review")
        .add_edge("writing_review", "evaluate_writing_quality")
        // Writing conditional: evaluate -> finalize or revise
        .add_conditional_edge("evaluate_writing_quality", |state: &SharedState| {
            Box::pin(async move {
                let quality_score: i64 = state.get("writing_quality_score").unwrap_or(0);
                let revision_count: i64 = state.get("writing_revision_count").unwrap_or(0);
                let threshold: i64 = state.get("writing_quality_threshold").unwrap_or(80);
                let max_revs: i64 = state.get("writing_max_revisions").unwrap_or(2);

                if quality_score >= threshold {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "writing",
                        quality_score = quality_score,
                        threshold = threshold,
                        "Writing quality threshold met -- proceeding to finalize"
                    );
                    "finalize_prompt".to_string()
                } else if revision_count < max_revs {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "writing",
                        quality_score = quality_score,
                        threshold = threshold,
                        revision_count = revision_count,
                        "Writing quality below threshold -- looping to revise"
                    );
                    "writing_revise_prompt".to_string()
                } else {
                    tracing::info!(
                        pipeline = "research_to_writing",
                        phase = "writing",
                        quality_score = quality_score,
                        revision_count = revision_count,
                        max_revisions = max_revs,
                        "Writing max revisions reached -- proceeding to finalize"
                    );
                    "finalize_prompt".to_string()
                }
            })
        })
        // Writing revise loop: revise -> increment -> re-review (prompt node)
        .add_edge("writing_revise_prompt", "writing_revise")
        .add_edge("writing_revise", "increment_writing_revision")
        .add_edge("increment_writing_revision", "writing_review_prompt")
        // Finalize path: prompt -> agent -> metadata
        .add_edge("finalize_prompt", "finalize")
        .add_edge("finalize", "finalize_metadata")
        .set_finish("finalize_metadata")
        .build()?;

    Ok(graph)
}

// ── Pipeline Execution ─────────────────────────────────────────────────────────

/// Execute the research-to-writing continuous workflow.
///
/// Runs the entire pipeline: research (search, synthesize, draft with revision
/// loop) through writing (outline, draft, review, revise loop, final polish).
///
/// Returns the final output string -- the polished, publication-ready document.
///
/// The `SharedState` after execution contains these keys:
/// - `literature_review` -- synthesized literature review from research phase
/// - `paper_draft` -- research-phase paper draft (may be revised)
/// - `research_quality_score` -- research phase quality score (0-100)
/// - `research_revision_count` -- number of research revision rounds
/// - `research_context` -- combined context bridging research to writing
/// - `outline` -- writing-phase outline
/// - `writing_draft` -- writing-phase draft (may be revised)
/// - `writing_review` -- latest writing review feedback
/// - `writing_quality_score` -- writing phase quality score (0-100)
/// - `writing_revision_count` -- number of writing revision rounds
/// - `final_output` -- the polished final document
pub async fn run_research_to_writing(
    agent: AgentHandle,
    config: ResearchToWritingConfig,
) -> anyhow::Result<String> {
    let shared_agent = agent.as_shared_agent().await;
    let graph = build_research_to_writing_graph(shared_agent)?;
    let state = SharedState::new();

    // Store all config values in state before graph execution starts
    state.set("topic", config.topic.clone())?;
    state.set("max_papers", config.max_papers as i64)?;
    state.set("audience", config.audience.clone())?;
    state.set("format", config.format.clone())?;
    state.set("research_revision_count", 0i64)?;
    state.set(
        "research_max_revisions",
        config.research_max_revisions as i64,
    )?;
    state.set(
        "research_quality_threshold",
        config.research_quality_threshold as i64,
    )?;
    state.set("writing_revision_count", 0i64)?;
    state.set("writing_max_revisions", config.writing_max_revisions as i64)?;
    state.set(
        "writing_quality_threshold",
        config.writing_quality_threshold as i64,
    )?;

    tracing::info!(
        pipeline = "research_to_writing",
        topic = %config.topic,
        max_papers = config.max_papers,
        research_max_revisions = config.research_max_revisions,
        research_quality_threshold = config.research_quality_threshold,
        audience = %config.audience,
        format = %config.format,
        writing_max_revisions = config.writing_max_revisions,
        writing_quality_threshold = config.writing_quality_threshold,
        "Starting research-to-writing continuous workflow"
    );

    let result = graph.run(state).await?;

    tracing::info!(
        pipeline = "research_to_writing",
        steps = result.steps,
        path = ?result.path,
        "Research-to-writing pipeline completed"
    );

    // Extract the final output from the state
    let final_output: String = result.state.get("final_output").unwrap_or_else(|| {
        result.state.get("writing_draft").unwrap_or_else(|| {
            "Research-to-writing pipeline completed but no final output was generated.".to_string()
        })
    });

    Ok(final_output)
}
