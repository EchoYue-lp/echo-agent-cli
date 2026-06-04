//! Dynamic prompt assembly — modular system prompt with per-module token budgets.
//!
//! Replaces the fixed three-section prompt construction with priority-ordered,
//! budget-aware module composition. Each module has a token budget; when the
//! total exceeds the model's context window, non-required modules are truncated.
//!
//! # Prompt Template Integration
//!
//! The `PromptAssembler` can optionally delegate variable substitution to a
//! `PromptTemplateManager` (from `echo_core`). When a template engine is
//! provided, module content containing `{{variable}}` syntax is rendered
//! through the engine before assembly. This enables dynamic prompt templates
//! such as mode-specific prompts with runtime variable injection.

use echo_agent::prelude::PromptTemplateManager;
use std::sync::Arc;

use super::context::ProjectContext;
use super::modes::AgentMode;

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
    /// Optional prompt template engine for variable substitution.
    /// When set, module content containing `{{variable}}` syntax is
    /// rendered through the engine before assembly.
    template_engine: Option<Arc<PromptTemplateManager>>,
}

impl PromptAssembler {
    /// Create a new assembler with the given total token budget.
    pub fn new(total_budget: usize) -> Self {
        Self {
            modules: Vec::new(),
            total_budget,
            template_engine: None,
        }
    }

    /// Create a new assembler with a prompt template engine.
    ///
    /// When provided, module content that contains `{{variable}}` markers
    /// will be rendered through the template engine during assembly.
    pub fn with_template_engine(total_budget: usize, engine: Arc<PromptTemplateManager>) -> Self {
        Self {
            modules: Vec::new(),
            total_budget,
            template_engine: Some(engine),
        }
    }

    /// Set the prompt template engine on an existing assembler.
    pub fn set_template_engine(&mut self, engine: Arc<PromptTemplateManager>) {
        self.template_engine = Some(engine);
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
    ///
    /// When a `PromptTemplateManager` is set, module content containing
    /// `{{variable}}` markers is rendered through the template engine before
    /// assembly. Variables are resolved from the provided `template_vars`.
    pub fn assemble(&self, template_vars: &[(&str, &str)]) -> String {
        let mut parts = Vec::new();
        let mut used_tokens = 0usize;

        for module in &self.modules {
            // Render module content through template engine if available
            let rendered_content = if let Some(engine) = &self.template_engine {
                engine.render_template(&module.content, template_vars)
            } else {
                module.content.clone()
            };

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

    /// Build the system prompt from all modules without template substitution.
    ///
    /// Convenience wrapper for `assemble(&[])` when no template variables
    /// are needed.
    pub fn assemble_no_vars(&self) -> String {
        self.assemble(&[])
    }

    /// Build default modules for a coding session.
    pub fn default_for_mode(
        mode: &AgentMode,
        base_prompt: &str,
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

        // P1: Mode-specific instructions (required)
        assembler.add_module(PromptModule {
            name: "mode".into(),
            content: super::modes::chinese_mode_prompt(mode),
            priority: 1,
            token_budget: 0,
            required: true,
        });

        // P2: Project rules (high priority but can be truncated)
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
                    priority: 2,
                    token_budget: model_window / 10, // 10% for rules
                    required: false,
                });
            }

            // P3: Project structure
            if !ctx.file_tree_summary.is_empty() {
                assembler.add_module(PromptModule {
                    name: "project_structure".into(),
                    content: format!(
                        "## Project Structure ({})\n\n```\n{}\n```",
                        ctx.name, ctx.file_tree_summary
                    ),
                    priority: 3,
                    token_budget: model_window / 8, // 12.5% for structure
                    required: false,
                });
            }

            // P4: Git context (low priority)
            if let Some(git_ctx) = super::context::load_git_context(&ctx.root) {
                assembler.add_module(PromptModule {
                    name: "git_context".into(),
                    content: format!("## Git Status\n\n{git_ctx}"),
                    priority: 4,
                    token_budget: model_window / 12, // ~8% for git
                    required: false,
                });
            }
        }

        // P5: Task state placeholder (lowest priority)
        assembler.add_module(PromptModule {
            name: "task_state".into(),
            content: "[Task state: no active tasks]".into(),
            priority: 5,
            token_budget: model_window / 20, // 5% for task state
            required: false,
        });

        assembler
    }

    /// Build default modules for a mode with a prompt template engine.
    ///
    /// Same as `default_for_mode`, but also attaches a `PromptTemplateManager`
    /// so that module content containing `{{variable}}` markers can be rendered
    /// dynamically during `assemble()`.
    ///
    /// This enables the mode's system prompt to contain template variables
    /// (e.g., `{{project_name}}`, `{{language}}`) that are resolved at
    /// assembly time rather than at registration time.
    pub fn default_for_mode_with_engine(
        mode: &AgentMode,
        base_prompt: &str,
        project_ctx: Option<&ProjectContext>,
        model_window: usize,
        engine: Arc<PromptTemplateManager>,
    ) -> Self {
        let mut assembler = Self::with_template_engine(model_window, engine);

        // P0: Base system prompt (required)
        assembler.add_module(PromptModule {
            name: "base".into(),
            content: base_prompt.to_string(),
            priority: 0,
            token_budget: 0,
            required: true,
        });

        // P1: Mode-specific instructions (required)
        // Use the template engine to look up the mode prompt template if available.
        let mode_template_name = super::modes::template_key(mode);
        let mode_content = assembler
            .template_engine
            .as_ref()
            .and_then(|engine| engine.get_template(mode_template_name))
            .unwrap_or_else(|| super::modes::chinese_mode_prompt(mode));

        assembler.add_module(PromptModule {
            name: "mode".into(),
            content: mode_content,
            priority: 1,
            token_budget: 0,
            required: true,
        });

        // P2: Project rules (high priority but can be truncated)
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
                    priority: 2,
                    token_budget: model_window / 10, // 10% for rules
                    required: false,
                });
            }

            // P3: Project structure
            if !ctx.file_tree_summary.is_empty() {
                assembler.add_module(PromptModule {
                    name: "project_structure".into(),
                    content: format!(
                        "## Project Structure ({})\n\n```\n{}\n```",
                        ctx.name, ctx.file_tree_summary
                    ),
                    priority: 3,
                    token_budget: model_window / 8, // 12.5% for structure
                    required: false,
                });
            }

            // P4: Git context (low priority)
            if let Some(git_ctx) = super::context::load_git_context(&ctx.root) {
                assembler.add_module(PromptModule {
                    name: "git_context".into(),
                    content: format!("## Git Status\n\n{git_ctx}"),
                    priority: 4,
                    token_budget: model_window / 12, // ~8% for git
                    required: false,
                });
            }
        }

        // P5: Task state placeholder (lowest priority)
        assembler.add_module(PromptModule {
            name: "task_state".into(),
            content: "[Task state: no active tasks]".into(),
            priority: 5,
            token_budget: model_window / 20, // 5% for task state
            required: false,
        });

        assembler
    }
}
