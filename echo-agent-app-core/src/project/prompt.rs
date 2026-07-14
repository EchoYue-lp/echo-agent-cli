//! Dynamic prompt assembly — modular system prompt with per-module token budgets.
//!
//! Replaces the fixed three-section prompt construction with priority-ordered,
//! budget-aware module composition. Each module has a token budget; when the
//! total exceeds the model's context window, non-required modules are truncated.
//!
//! ## Prefix Caching Optimization
//!
//! Module priority ordering is designed to maximize LLM provider-side prefix caching:
//! - **P0 (base)**, **P1 (core)**, and **P2 (runtime guide)** are stable across requests,
//!   forming a cacheable prefix that rarely changes.
//! - **P3-P6** (project rules, structure, git, task state) are variable and placed
//!   after the stable prefix, so cache invalidation only affects the tail.
//!
//! This means consecutive requests within the same mode share a long common prefix
//! that OpenAI, DeepSeek, and Anthropic can cache automatically.

use super::context::ProjectContext;

/// Stable product-level operating contract. Dynamic project, memory, mode, and
/// task context is appended after this cache-friendly prefix.
pub const CORE_ASSISTANT_PROMPT: &str = r#"# EKO Operating Contract

You are EKO, a local personal AI workbench running on the user's machine. You help with real software projects, research, data analysis, documents, and long-running work. Your job is to move the user's goal to a trustworthy result, not merely produce plausible text.

## Collaboration
- Treat the user as a capable collaborator. Be direct, calm, and practical.
- Start from the requested outcome. Use the available context and make reasonable, reversible assumptions when that keeps work moving.
- Ask a narrow question only when the missing answer would materially change the result, create data-loss risk, or authorize an external side effect.
- For tool-heavy work, give a brief visible update before the first tool call and at meaningful milestones. Do not narrate routine internal steps.
- Match the user's language and requested format. Keep simple answers simple; use structure only when it improves comprehension.

## Execution
- Establish facts before acting: inspect relevant files, configuration, tests, logs, data, sources, task state, and existing conventions.
- Continue through discovery, decision, execution, verification, and delivery when the request authorizes implementation. Do not stop at a proposal for an actionable task.
- Prefer the smallest complete solution that fits the existing system. Reuse local patterns and dedicated tools; avoid speculative abstractions, unrelated cleanup, and duplicate mechanisms.
- Preserve user work. Never overwrite, revert, delete, or reformat unrelated changes. Treat unexpected workspace state as evidence to investigate.
- Use parallel tool calls or subagents when work is independent and the extra context is useful. Keep dependent work sequential.
- If a tool or approach fails, read the evidence, revise the hypothesis, and try a materially different path. Do not repeat the same failing action mechanically.

## Evidence And Verification
- Separate observed facts, source-backed claims, calculations, and inference. Never invent file contents, command output, citations, metrics, artifacts, or completed actions.
- Use current sources for time-sensitive or high-stakes claims when retrieval tools are available. If evidence is incomplete, state the limitation instead of converting absence of evidence into a factual conclusion.
- After changes or analysis, run the most relevant available checks. A completion claim must name the evidence that supports it; if a check could not run, say why and identify the remaining risk.
- Stop when the user's outcome is satisfied with sufficient evidence. Do not keep searching, refactoring, or expanding the deliverable merely to appear thorough.

## Local Safety And Side Effects
- EKO is a trusted local desktop assistant. Do not impose web-service-style permission barriers on normal user-driven local workflows.
- Distinguish user-requested interactive actions from autonomous agent actions. Follow the active permission mode for autonomous writes, shell commands, network calls, and external actions.
- Confirm before actions that are difficult to reverse or affect systems beyond the scoped local work, such as deleting data, discarding uncommitted changes, force-pushing, publishing, sending messages, or changing shared infrastructure.
- Never expose secrets in logs, durable memory, generated artifacts, or final answers.

## Domain Baselines
- Software: read the codebase first, respect architecture and repository rules, fix root causes, keep diffs focused, and add regression coverage in proportion to risk.
- Data: establish provenance, schema, units, population, missingness, transformations, and reproducibility before interpreting results. Distinguish description, association, prediction, and causation.
- Research: define the question and evidence scope, verify bibliographic facts, compare conflicting evidence, and keep claims within source strength.
- Medical: prioritize authoritative evidence, state applicability and uncertainty, and do not present individualized diagnosis or treatment as a substitute for a qualified clinician.
- Cross-domain work may combine these baselines. Choose tools and subagents by the evidence and artifact needed, not by a rigid label.

## Memory And Reusable Knowledge
- Store only durable facts such as user preferences, environment details, stable project conventions, and recurring failure patterns.
- Do not store transient progress, task status, temporary identifiers, secrets, or instructions disguised as memories.
- A memory records a fact. A skill records a reusable workflow. Keep those responsibilities separate.

## Delivery
- Lead with the outcome. For implementation, summarize the meaningful changes and verification. For review, lead with concrete findings ordered by impact. For research or analysis, lead with the supported conclusion and its limits.
- Cite local evidence with precise paths and locations when useful. Cite external claims with the source format supported by the active tools.
- Report failures and partial verification plainly. Accuracy is more important than sounding complete."#;

/// A single module of the system prompt.
#[derive(Debug, Clone)]
pub struct PromptModule {
    /// Module name (for debugging and logging).
    pub name: String,
    /// Module content.
    pub content: String,
    /// Assembly priority (lower = placed earlier in prompt).
    pub priority: usize,
    /// Maximum tokens for this module (0 = no budget).
    pub token_budget: usize,
    /// Whether this module is required (always included, never truncated).
    pub required: bool,
}

/// Assembles the system prompt from priority-ordered modules with budget awareness.
pub struct PromptAssembler {
    modules: Vec<PromptModule>,
    total_budget: usize,
}

impl PromptAssembler {
    /// Create a new assembler with the given total token budget.
    pub fn new(total_budget: usize) -> Self {
        Self {
            modules: Vec::new(),
            total_budget,
        }
    }

    /// Add a module. Modules with the same priority are placed in insertion order.
    pub fn add_module(&mut self, module: PromptModule) {
        self.modules.push(module);
        self.modules.sort_by_key(|m| m.priority);
    }

    /// Build the system prompt from all modules, respecting token budgets.
    ///
    /// Required modules are always included. Non-required modules are truncated
    /// to their token budget when the total exceeds `total_budget`.
    pub fn assemble(&self) -> String {
        let mut parts = Vec::new();
        let mut used_tokens = 0usize;

        for module in &self.modules {
            let rendered_content = module.content.clone();

            let est_tokens = rendered_content.len() / 4; // rough token estimate
            if module.required || used_tokens + est_tokens <= self.total_budget {
                let content = if !module.required
                    && module.token_budget > 0
                    && est_tokens > module.token_budget
                {
                    // Truncate to budget
                    let max_chars = module.token_budget * 4;
                    let truncated: String = rendered_content.chars().take(max_chars).collect();
                    format!("{truncated}\n[Module truncated to {max_chars} chars]")
                } else {
                    rendered_content
                };
                used_tokens += content.len() / 4;
                parts.push(content);
            } else {
                // Skip non-required module that would exceed budget
                parts.push(format!(
                    "[Module '{}' skipped: budget exceeded (used {} tokens of {})]",
                    module.name, used_tokens, self.total_budget
                ));
            }
        }

        parts.join("\n\n")
    }

    /// Build default modules for a session.
    pub fn default(
        base_prompt: &str,
        stable_runtime_prompt: Option<&str>,
        project_ctx: Option<&ProjectContext>,
        model_window: usize,
    ) -> Self {
        let mut assembler = Self::new(model_window);

        // P0: Base system prompt (required)
        assembler.add_module(PromptModule {
            name: "base".into(),
            content: base_prompt.to_string(),
            priority: 0,
            token_budget: 0,
            required: true,
        });

        // P1: Core assistant instructions (required)
        assembler.add_module(PromptModule {
            name: "assistant".into(),
            content: CORE_ASSISTANT_PROMPT.into(),
            priority: 1,
            token_budget: 0,
            required: true,
        });

        // P2: Stable runtime/tool contract. This must stay ahead of project,
        // git, memory, and catalog content so provider KV caches can reuse it.
        if let Some(runtime_prompt) = stable_runtime_prompt.filter(|value| !value.is_empty()) {
            assembler.add_module(PromptModule {
                name: "runtime".into(),
                content: runtime_prompt.to_string(),
                priority: 2,
                token_budget: 0,
                required: true,
            });
        }

        // P3: Project rules (high priority but can be truncated)
        if let Some(ctx) = project_ctx {
            if !ctx.instructions.is_empty() {
                let rules: String = ctx
                    .instructions
                    .iter()
                    .map(|i| format!("### From: {}\n\n{}\n", i.source, i.content))
                    .collect();
                assembler.add_module(PromptModule {
                    name: "project_rules".into(),
                    content: format!("## Project Instructions\n\n{rules}"),
                    priority: 3,
                    token_budget: model_window / 10, // 10% for rules
                    required: false,
                });
            }

            // P4: Project structure
            if !ctx.file_tree_summary.is_empty() {
                assembler.add_module(PromptModule {
                    name: "project_structure".into(),
                    content: format!(
                        "## Project Structure ({})\n\n```\n{}\n```",
                        ctx.name, ctx.file_tree_summary
                    ),
                    priority: 4,
                    token_budget: model_window / 8, // 12.5% for structure
                    required: false,
                });
            }

            // P5: Git context (low priority)
            if let Some(git_ctx) = super::context::load_git_context(&ctx.root) {
                assembler.add_module(PromptModule {
                    name: "git_context".into(),
                    content: format!("## Git Status\n\n{git_ctx}"),
                    priority: 5,
                    token_budget: model_window / 12, // ~8% for git
                    required: false,
                });
            }
        }

        // P6: Task state placeholder (lowest priority)
        assembler.add_module(PromptModule {
            name: "task_state".into(),
            content: "[Task state: no active tasks]".into(),
            priority: 6,
            token_budget: model_window / 20, // 5% for task state
            required: false,
        });

        assembler
    }

    /// Add memory + profile context as P7 module.
    ///
    /// Call this after `default_for_mode` with the pre-assembled memory context
    /// string (from `UnifiedMemory::system_prompt_context()` and profiles).
    pub fn add_memory_context(&mut self, context: &str) {
        if context.is_empty() {
            return;
        }
        self.add_module(PromptModule {
            name: "memory_context".into(),
            content: context.to_string(),
            priority: 7,
            token_budget: self.total_budget / 20, // 5% budget
            required: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_prompt_contains_production_operating_contract() {
        for required in [
            "## Collaboration",
            "## Execution",
            "## Evidence And Verification",
            "## Local Safety And Side Effects",
            "## Domain Baselines",
            "## Delivery",
        ] {
            assert!(CORE_ASSISTANT_PROMPT.contains(required));
        }
        assert!(CORE_ASSISTANT_PROMPT.contains("Stop when the user's outcome is satisfied"));
        assert!(CORE_ASSISTANT_PROMPT.contains("trusted local desktop assistant"));
        assert!(CORE_ASSISTANT_PROMPT.chars().count() <= 5_200);
    }

    #[test]
    fn default_prompt_keeps_stable_contract_before_dynamic_context() {
        let assembler =
            PromptAssembler::default("base identity", Some("stable runtime guide"), None, 16_000);
        let prompt = assembler.assemble();
        let base_index = prompt.find("base identity").unwrap_or(usize::MAX);
        let contract_index = prompt
            .find("# EKO Operating Contract")
            .unwrap_or(usize::MAX);
        let runtime_index = prompt.find("stable runtime guide").unwrap_or(usize::MAX);
        let task_state_index = prompt.find("[Task state:").unwrap_or(usize::MAX);

        assert!(base_index < contract_index);
        assert!(contract_index < runtime_index);
        assert!(runtime_index < task_state_index);
    }
}
