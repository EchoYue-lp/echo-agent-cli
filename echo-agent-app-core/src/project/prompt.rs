//! Dynamic prompt assembly — modular system prompt with per-module token budgets.
//!
//! Replaces the fixed three-section prompt construction with priority-ordered,
//! budget-aware module composition. Each module has a token budget; when the
//! total exceeds the model's context window, non-required modules are truncated.
//!
//! ## Prefix Caching Optimization
//!
//! Module priority ordering is designed to maximize LLM provider-side prefix caching:
//! - **P0 (base)** and **P1 (mode)** are always included and stable across requests,
//!   forming a cacheable prefix that rarely changes.
//! - **P2-P5** (project rules, structure, git, task state) are variable and placed
//!   after the stable prefix, so cache invalidation only affects the tail.
//!
//! This means consecutive requests within the same mode share a long common prefix
//! that OpenAI, DeepSeek, and Anthropic can cache automatically.

use super::context::ProjectContext;

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
            content: r#"你是 EchoCoWork，一个智能 AI 助手。你帮助用户完成各种任务：回答问题、编写和修改代码、分析信息、创意工作、通过工具执行操作。

# 核心原则
- 直接给出答案或采取行动，不要先描述你打算做什么
- 使用工具执行操作，不要只描述你会做什么而不实际执行
- 承认不确定性，优先考虑真正有用而非冗长
- 高效且有目标地探索和调查

# 信息准确性（极其重要）
- 当用户要求查找文章、数据、研究、文献时，**必须使用搜索工具**（web_search、pubmed_search、arxiv_search 等），绝不能凭记忆编造
- **严禁编造参考文献、引用、数据、统计数字**。每一条引用必须来自工具返回的真实搜索结果
- 如果工具没有找到相关信息，如实告知用户"未找到相关结果"，而不是编造看似合理的内容
- 区分你确定知道的信息和需要验证的信息；对于具体数据、日期、人名、引用，优先使用工具验证
- 当提供医学、法律、金融等专业领域的信息时，必须标注信息来源

# 工具使用
- 当有可用工具能完成任务时，必须使用它们而不是描述你打算做什么
- 每个响应要么包含推进任务的工具调用，要么向用户交付最终结果
- 可以并行调用多个独立工具来提高效率
- 工具失败时先诊断原因再切换策略，不要盲目重试相同操作

# 行动准则
- 仔细考虑行动的可逆性和影响范围
- 可以自由执行本地、可逆的操作（编辑文件、运行测试）
- 对于难以逆转、影响共享系统或有风险的操作，先与用户确认
- 不要使用破坏性操作作为捷径来绕过问题

# 输出风格
- 简洁直接，先给答案/行动，再给推理
- 不要重述用户说的话，直接执行
- 只在需要用户输入、关键里程碑、或计划变更时输出文字
- 引用代码时使用 file_path:line_number 格式"#.into(),
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

    /// Add memory + profile context as P6 module.
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
            priority: 6,
            token_budget: self.total_budget / 20, // 5% budget
            required: false,
        });
    }
}
