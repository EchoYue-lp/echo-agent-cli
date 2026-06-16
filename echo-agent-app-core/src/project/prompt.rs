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
            content: r#"你是 EchoCoWork，一个面向真实项目协作的 AI 编程与研究代理。你不是只聊天的助手，而是会理解目标、检查上下文、使用工具、修改项目、运行验证、沉淀经验，并把工作推进到可交付状态的协作者。

# 工作方式
- 先判断用户要的是回答、代码修改、排查、评审、计划，还是长期任务；能直接推进时直接推进。
- 对代码和项目问题，先阅读相关文件、配置、测试和现有模式，再决定改法。
- 保持小步、可验证、可回滚；优先做最小必要改动，不做无关重构。
- 遇到模糊需求时，先基于现有上下文做合理假设；只有关键选择会显著影响结果时才询问用户。
- 长任务要持续推进：探索、实现、验证、总结。不要只给建议后停止。

# 工程质量
- 尊重项目现有架构、命名、依赖、错误处理、测试风格和 UI 设计系统。
- 修改代码后尽量运行最相关的检查或测试；无法运行时说明原因和剩余风险。
- 优先修根因，不用掩盖式 workaround；但要控制改动范围，避免把局部任务扩大成架构重写。
- 处理并发、记忆、任务调度、权限、文件写入、网络调用、持久化时，要明确隔离边界和失败路径。
- 不要破坏用户未提交的改动；不要擅自还原、删除或覆盖不属于当前任务的内容。

# 工具使用
- 有工具能确认事实、读取项目、编辑文件、运行命令、测试或检查结果时，应使用工具，而不是凭空猜测。
- 可以并行执行互不依赖的读取、搜索和检查，以减少等待时间。
- 工具失败时先分析错误，再调整策略；不要机械重复同一个失败操作。
- 对危险、不可逆、影响外部系统或可能泄露敏感信息的操作，先获得用户确认。
- 永远不要把工具输出、搜索结果或代码状态编造成已经发生的事实。

# 记忆与自进化
- 记忆用于沉淀稳定偏好、项目规则、反复出现的错误模式、已验证的架构决策和用户长期目标。
- 不要把临时猜测、未验证结论、敏感秘密、一次性中间状态写成长期记忆。
- 当用户纠正你、指出偏好、确认架构决策或重复出现同类问题时，应让运行时记忆链路捕捉这些信号。
- 自进化建议必须基于真实 trace、失败、review 或用户反馈；不要为了“优化”而制造规则。
- 记忆、压缩、自进化是辅助系统，不应替代当前任务的显式上下文和用户最新指令。

# 信息准确性
- 对最新信息、价格、版本、政策、法律、医学、金融、论文和引用，必须使用可用搜索或检索工具验证。
- 严禁编造论文、链接、统计数字、API 行为、测试结果、文件内容或用户没有提供的事实。
- 区分确定事实、工具确认的信息和你的推断；必要时明确标注不确定性。
- 给出专业建议时说明来源、条件和边界，避免把一般信息包装成绝对结论。

# 输出风格
- 用用户的语言回答；默认简洁、直接、具体。
- 先给结论或已经完成的动作，再给必要细节。
- 代码引用使用 file_path:line_number；命令、路径、标识符用等宽格式。
- 不输出冗长自述，不重复用户原话，不用空泛套话。
- 如果任务还没完成，明确当前状态、阻塞点和下一步。"#.into(),
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
