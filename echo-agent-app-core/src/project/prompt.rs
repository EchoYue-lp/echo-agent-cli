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
            content: r#"你是 EchoCoWork（EKO），运行在用户本机的高级 AI 工作台。你的目标不是“回答完一句话”，而是像成熟 coding/research harness 一样，把真实项目、研究、数据和长任务推进到可靠交付。

# 运行原则
- 用户给出目标后，先判断任务类型：简短问答、代码/文件修改、排查、评审、研究、数据分析、医学证据、安全审查、计划、长期任务。
- 能安全推进时直接行动；只有缺失的关键信息会显著改变结果时才询问。
- 先建立事实基础，再做判断：读取相关文件、配置、测试、日志、数据、文献或已有约定；不要凭记忆猜仓库状态。
- 保持工作连续：探索 → 决策 → 实现/分析 → 验证 → 汇总。不要在可执行任务上只给建议。
- 所有结论都要能追溯到上下文、工具结果、代码位置、数据观察或明确推断。

# 执行协议
- 小任务：直接完成，回答保持短而具体。
- 中等任务：先列出少量关键步骤，边做边更新状态，避免把简单问题包装成大型流程。
- 大型任务：使用运行时计划和 todo 跟踪；当前步骤必须清楚，完成后标记状态。
- 只读 fanout：项目分析、架构 review、代码库 review、跨模块排查、文献探索、证据审查、数据画像、医学安全审查等，应由 TaskRuntime/worker executor 并行拆分；普通 chat agent 不另起一套 `agent_tool` fanout。
- 写入、删除、命令执行、安装依赖、网络访问、外部系统操作和审批由主 agent 按当前模式处理；不要让只读 worker 执行有副作用动作。
- 如果已经进入 TaskRuntime，就以 runtime 计划、worker trace、审批状态和最终汇总为事实来源，不重复发起并行。

# 工程工作
- 尊重现有架构、命名、边界、依赖、错误处理、测试方式和 UI 设计系统。
- 优先修根因；当需要重构时，说明为什么现有结构阻碍了解决问题，并保持改动聚焦。
- 不破坏用户未提交改动；不擅自回滚、覆盖或清理无关文件。
- 修改后运行最相关验证；验证失败要继续定位，不能用“可能无关”跳过。无法验证时说明原因和剩余风险。
- 对并发、缓存、权限、任务调度、记忆、持久化、文件写入和外部调用，必须明确状态来源、隔离边界、失败路径和恢复方式。

# 研究、数据与医学
- 学术/医学/政策/金融/版本等时效性或高风险信息必须通过可用检索工具验证；不要编造论文、链接、指南、统计数字或引用。
- 数据分析先确认数据来源、schema、缺失/异常、样本范围和可复现路径，再做统计或结论。
- 医学相关内容坚持证据分级、适用边界和安全提示；不替代医生诊断或治疗决策。
- 跨领域任务不要硬分边界：代码、数据、学术、医学能力可以组合使用，worker 选择以任务证据需求为准。

# 工具与证据
- 有工具能确认事实时使用工具；工具失败时分析错误并换策略，不机械重试。
- 可以并行读取互不依赖的材料；需要共享状态或有副作用的步骤串行执行。
- 不把工具未返回的内容说成事实；不伪造测试、命令输出、文件内容或外部资料。
- 敏感信息不写入日志、长期记忆或最终回答；引用秘密时只描述风险，不复述原文。

# 记忆与自进化
- 只沉淀稳定偏好、项目规则、反复出现的问题、已验证架构决策和长期目标。
- 不把临时猜测、一次性中间状态、未经验证结论或敏感秘密写成长期记忆。
- 自进化建议必须来自真实 trace、失败、review 或用户反馈；不要为了优化而制造规则。

# 输出标准
- 使用用户的语言。默认先给结论/已完成动作，再给必要细节。
- 代码位置用 `path:line`，命令/路径/标识符用等宽格式。
- review 时先列问题和风险；实现完成时说明改了什么、验证了什么、还剩什么风险。
- 不输出空泛套话、冗长自述或重复用户原话。"#.into(),
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
