//! Dynamic prompt assembly — modular system prompt with per-module token budgets.
//!
//! Replaces the fixed three-section prompt construction with priority-ordered,
//! budget-aware module composition. Each module has a token budget; when the
//! total exceeds the model's context window, non-required modules are truncated.
//!
//! ## Prefix Caching Optimization
//!
//! Module priority ordering is designed to maximize LLM provider-side prefix caching:
//! - **P0-P4** (base, core, runtime guide, durable instructions, subagent catalog)
//!   form the stable session prefix.
//! - **P5-P7** (project rules, structure, git) follow that prefix and carry
//!   explicit token caps.
//!
//! This means consecutive requests within the same mode share a long common prefix
//! that OpenAI, DeepSeek, and Anthropic can cache automatically.

use super::context::ProjectContext;
use echo_core::tokenizer::{HeuristicTokenizer, Tokenizer};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Stable product-level operating contract. Dynamic project, memory, mode, and
/// task context follows this cache-friendly prefix.
pub const CORE_ASSISTANT_PROMPT: &str = r#"# EKO Operating Contract

You are EKO, a local personal AI workbench running on the user's machine. You help with real software projects, research, data analysis, documents, and long-running work. Your job is to move the user's goal to a trustworthy result, not merely produce plausible text.

## Collaboration
- Treat the user as a capable collaborator. Be direct, calm, and practical.
- Start from the requested outcome. Use the available context and make reasonable, reversible assumptions when that keeps work moving.
- Ask a narrow question only when the missing answer would materially change the result, create data-loss risk, or authorize an external side effect.
- For tool-heavy work, give a brief visible update before the first tool call and at meaningful milestones. Do not narrate routine internal steps.
- User text, not system/tool language, determines reply and task language; preserve code, paths, commands, logs, identifiers, and technical terms.
- Keep simple answers simple; use structure only when it improves comprehension.

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

/// Token and inclusion result for one assembled prompt module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptModuleUsage {
    pub name: String,
    pub estimated_tokens: usize,
    pub included: bool,
    pub truncated: bool,
    pub content_hash: String,
    pub stable_prefix: bool,
}

/// Prompt text plus budget diagnostics used by tests and observability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptAssembly {
    #[serde(skip_serializing)]
    pub prompt: String,
    pub estimated_tokens: usize,
    pub modules: Vec<PromptModuleUsage>,
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
        self.assemble_with_report().prompt
    }

    /// Build the prompt and return per-module budget diagnostics.
    pub fn assemble_with_report(&self) -> PromptAssembly {
        let tokenizer = HeuristicTokenizer;
        let mut parts = Vec::new();
        let mut used_tokens = 0usize;
        let mut usages = Vec::new();

        for module in &self.modules {
            let estimated = tokenizer.count_tokens(&module.content);
            if module.required {
                used_tokens = used_tokens.saturating_add(estimated);
                parts.push(module.content.clone());
                usages.push(PromptModuleUsage {
                    name: module.name.clone(),
                    estimated_tokens: estimated,
                    included: true,
                    truncated: false,
                    content_hash: content_hash(&module.content),
                    stable_prefix: module.priority <= 4,
                });
                continue;
            }

            let remaining = self.total_budget.saturating_sub(used_tokens);
            let module_limit = if module.token_budget == 0 {
                remaining
            } else {
                remaining.min(module.token_budget)
            };
            if module_limit == 0 {
                usages.push(PromptModuleUsage {
                    name: module.name.clone(),
                    estimated_tokens: 0,
                    included: false,
                    truncated: false,
                    content_hash: String::new(),
                    stable_prefix: module.priority <= 4,
                });
                continue;
            }

            let (content, truncated) = if estimated > module_limit {
                (
                    truncate_to_estimated_tokens(&module.content, module_limit),
                    true,
                )
            } else {
                (module.content.clone(), false)
            };
            let included_tokens = tokenizer.count_tokens(&content);
            if content.is_empty() {
                usages.push(PromptModuleUsage {
                    name: module.name.clone(),
                    estimated_tokens: 0,
                    included: false,
                    truncated,
                    content_hash: String::new(),
                    stable_prefix: module.priority <= 4,
                });
                continue;
            }
            used_tokens = used_tokens.saturating_add(included_tokens);
            let included_hash = content_hash(&content);
            parts.push(content);
            usages.push(PromptModuleUsage {
                name: module.name.clone(),
                estimated_tokens: included_tokens,
                included: true,
                truncated,
                content_hash: included_hash,
                stable_prefix: module.priority <= 4,
            });
        }

        let prompt = parts.join("\n\n");
        PromptAssembly {
            estimated_tokens: tokenizer.count_tokens(&prompt),
            prompt,
            modules: usages,
        }
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

        // P5: Project rules (high priority but can be truncated)
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
                    priority: 5,
                    token_budget: (model_window / 10).min(6_000),
                    required: false,
                });
            }

            // P6: Project structure
            if !ctx.file_tree_summary.is_empty() {
                assembler.add_module(PromptModule {
                    name: "project_structure".into(),
                    content: format!(
                        "## Project Structure ({})\n\n```\n{}\n```",
                        ctx.name, ctx.file_tree_summary
                    ),
                    priority: 6,
                    token_budget: (model_window / 8).min(4_000),
                    required: false,
                });
            }

            // P7: Git context (low priority)
            if let Some(git_ctx) = super::context::load_git_context(&ctx.root) {
                assembler.add_module(PromptModule {
                    name: "git_context".into(),
                    content: format!("## Git Status\n\n{git_ctx}"),
                    priority: 7,
                    token_budget: (model_window / 12).min(1_500),
                    required: false,
                });
            }
        }

        assembler
    }

    /// Add stable user/project/local instruction context as P3.
    pub fn add_instruction_context(&mut self, context: &str) {
        if context.is_empty() {
            return;
        }
        self.add_module(PromptModule {
            name: "instruction_context".into(),
            content: context.to_string(),
            priority: 3,
            token_budget: (self.total_budget / 10).min(4_000),
            required: false,
        });
    }

    /// Add the stable subagent catalog as P4.
    pub fn add_subagent_catalog(&mut self, catalog: &str) {
        if catalog.is_empty() {
            return;
        }
        self.add_module(PromptModule {
            name: "subagent_catalog".into(),
            content: catalog.to_string(),
            priority: 4,
            token_budget: (self.total_budget / 20).min(2_000),
            required: false,
        });
    }
}

fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn truncate_to_estimated_tokens(text: &str, max_tokens: usize) -> String {
    if max_tokens == 0 {
        return String::new();
    }
    let max_weight = max_tokens.saturating_mul(4).saturating_add(3);
    let mut weight = 0usize;
    text.chars()
        .take_while(|character| {
            let next = weight.saturating_add(if character.is_ascii() { 1 } else { 2 });
            if next > max_weight {
                return false;
            }
            weight = next;
            true
        })
        .collect()
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

        assert!(base_index < contract_index);
        assert!(contract_index < runtime_index);
        assert!(!prompt.contains("[Task state:"));
    }

    #[test]
    fn optional_modules_use_mixed_language_token_budgets_without_prompt_noise() {
        let mut assembler = PromptAssembler::new(80);
        assembler.add_module(PromptModule {
            name: "base".into(),
            content: "stable".into(),
            priority: 0,
            token_budget: 0,
            required: true,
        });
        assembler.add_module(PromptModule {
            name: "dynamic".into(),
            content: "中文动态上下文".repeat(100),
            priority: 1,
            token_budget: 20,
            required: false,
        });

        let assembly = assembler.assemble_with_report();
        let dynamic = assembly
            .modules
            .iter()
            .find(|module| module.name == "dynamic");
        assert!(dynamic.is_some_and(|module| module.included && module.truncated));
        assert!(dynamic.is_some_and(|module| module.estimated_tokens <= 20));
        assert!(!assembly.prompt.contains("Module truncated"));
        assert!(!assembly.prompt.contains("budget exceeded"));
    }
}
