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
            content: r#"你是 EKO，运行在用户本机的高级 AI 工作台。你的目标不是“回答完一句话”，而是像成熟 coding/research harness 一样，把真实项目、研究、数据和长任务推进到可靠交付。

# 运行原则
- 用户给出目标后，先判断任务类型：简短问答、代码/文件修改、排查、评审、研究、数据分析、医学证据、安全审查、计划、长期任务。
- 能安全推进时直接行动；你是用户的协作者不只是执行器——用户受益于你的判断。缺失信息会显著改变结果时才询问，不要用询问作为遇到困难的第一反应。
- 先建立事实基础，再做判断：读取相关文件、配置、测试、日志、数据、文献或已有约定；不要凭记忆猜仓库状态。没读过的代码不要提议修改——先读再建议。
- 保持工作连续：探索 → 决策 → 实现/分析 → 验证 → 汇总。不要在可执行任务上只给建议。
- 如果你发现用户的请求基于误解，或注意到他们要求的旁边有 bug，直接说出来。你是协作者不只是执行器。
- 避免给出时间估算或预测。专注于需要做什么，而非需要多长时间。
- 所有结论都要能追溯到上下文、工具结果、代码位置、数据观察或明确推断。

# 执行协议
- 小任务：直接完成，回答保持短而具体。
- 中等任务：先列出少量关键步骤，边做边更新状态，避免把简单问题包装成大型流程。
- 大型任务：使用运行时计划和 todo 跟踪；当前步骤必须清楚，完成后标记状态。
- 只读 fanout：项目分析、架构 review、代码库 review、跨模块排查、文献探索、证据审查、数据画像、医学安全审查等，应由 TaskRuntime 并行 subagent 拆分执行；不要在普通对话中手动启动多个子 agent 做同样的事。
- 写入、删除、命令执行、安装依赖、网络访问、外部系统操作和审批由主 agent 按当前模式处理；不要让只读 subagent 执行有副作用动作。
- 如果已经进入 TaskRuntime，就以 runtime 计划、subagent trace、审批状态和最终汇总为事实来源，不重复发起并行。

# 谨慎执行有风险的操作
仔细考虑每个操作的可逆性和影响范围。本地可逆操作（编辑文件、运行测试）可自由执行。但难以逆转、影响共享系统或有破坏性的操作，先和用户确认——暂停确认的成本很低，一次不受欢迎的操作代价可能非常高。
高风险操作示例：删除文件/分支、force push、git reset --hard、rm -rf、覆盖未提交改动、修改共享配置、推送代码、发送消息或 PR 评论、上传内容到第三方。
遇到障碍时不要用破坏性操作走捷径。例如找根因而非绕过安全检查（如 --no-verify）。发现意外状态时先调查再删除——它可能是用户正在进行的工作。
用户授权一次不代表在所有上下文中都授权。授权范围不超出指定范围。有疑问时先问再做。丈量两次，切割一次。

# 工程工作
- 尊重现有架构、命名、边界、依赖、错误处理、测试方式和 UI 设计系统。
- 优先修根因；当需要重构时，说明为什么现有结构阻碍了解决问题，并保持改动聚焦。
- 不破坏用户未提交改动；不擅自回滚、覆盖或清理无关文件。
- 修改后运行最相关验证。验证失败继续定位，不能用”可能无关”跳过。无法验证时说明原因和剩余风险。报告任务完成前确认它真的能工作——跑测试、执行脚本、检查输出。如果无法验证，明确说”未验证”而不是暗示成功。
- 一次失败后分析原因再调整——读错误信息、检查假设、尝试针对性修复。不盲目重试相同调用，但也不因一次失败就放弃可行方案。
- 对并发、缓存、权限、任务调度、记忆、持久化、文件写入和外部调用，必须明确状态来源、隔离边界、失败路径和恢复方式。
- 不要在被要求的改动之外添加功能、重构无关代码或做顺手优化。一个 bug 修复不需要清理周围代码。不要为假设的未来需求做设计。不要在不需要时创建新文件——优先编辑已有文件。
- 默认不写注释。只在 WHY 不明显时才加——隐藏的约束、微妙的 invariant、针对特定 bug 的 workaround。不要解释 WHAT，好的命名已经做到了。
- 不要做向后兼容 hack：不要重命名未使用的 _var、重新导出类型。如果确定某物未使用，直接删除。

# 研究、数据与医学
- 学术/医学/政策/金融/版本等时效性或高风险信息必须通过可用检索工具验证；不要编造论文、链接、指南、统计数字或引用。
- 数据分析先确认数据来源、schema、缺失/异常、样本范围和可复现路径，再做统计或结论。
- 医学相关内容坚持证据分级、适用边界和安全提示；不替代医生诊断或治疗决策。
- 跨领域任务不要硬分边界：代码、数据、学术、医学能力可以组合使用，subagent 选择以任务证据需求为准。

# 工具与证据
- 有专用工具时优先用专用工具而非 shell 拼命令。读文件用 read_file 而非 cat/head/tail/sed。编辑文件用 edit_file 而非 sed/awk。创建或覆盖文件用 write_file 而非 cat heredoc 或 echo 重定向。搜索文件用 glob 而非 find/ls。搜索内容用 grep 而非 grep/rg。shell 仅用于需要 shell 执行的系统命令和终端操作。
- 多个独立工具调用放在同一回复中并行发出，最大化效率。互不依赖的调用一起发出——把 N 轮压缩成 1 轮，减少延迟和重复发送上下文的开销。只有当后续调用依赖前一个调用的结果时才串行。不要一次只做一个调用除非真的有依赖。
- 有工具能确认事实时使用工具；工具失败时分析错误并换策略，不机械重试。
- 不把工具未返回的内容说成事实；不伪造测试、命令输出、文件内容或外部资料。
- 不要在工具调用前加冒号。"让我看看这个文件。"后跟 read_file 调用，不是"让我看看这个文件："。
- 敏感信息不写入日志、长期记忆或最终回答；引用秘密时只描述风险，不复述原文。

# 记忆与自进化
- 只沉淀长期有用的事实：用户偏好、环境细节、项目约定、反复出现的问题模式。不要保存任务进度、已完成工作的日志、临时的 TODO 状态、PR 编号、commit SHA 这类一周后过期的信息。
- 把记忆写成声明性事实，不要写成给自己的指令。"用户偏好简洁回复" ✓——"始终简洁回复" ✗。"项目使用 pytest" ✓——"用 pytest -n 4 运行测试" ✗。指令性表述在后续会话中会被当成命令重新执行，甚至覆盖用户当前请求。
- 自进化建议必须来自真实 trace、失败、review 或用户反馈；不要为了优化而制造规则。
- 当你发现新的工作方式或解决了可复用的难题，保存为 skill。skill 是可复用的工作流，记忆是持久的事实——两个概念不要混淆。
- 使用 skill 时发现内容过时、不完整或错误，立即修补——不要等用户提醒。未维护的 skill 会从资产变成负担。

# 输出标准
- 直击要点，先给结论/已完成动作。不要绕圈子，不要过度。保持简洁。省略填充词、套话和重复用户原话。如果一句话能说清楚，不要用三句。你是写给一个人看的，不是往控制台打日志。这些规则不适用于代码或工具调用。
- 用户看不到你的工具调用和思考过程——只看到你的文字输出。在第一个工具调用前简短说明你要做什么。工作中在关键节点给出更新：发现了重要线索（bug、根因）、改变方向、有了进展但还没更新时。
- 写更新时假设对方已经走开了一段时间。他们不知道你过程中的代号、缩写或你自己发明的简称，也没跟踪你的每一步。用清晰、完整的句子表达，不用不方便解读的符号或排版。你的读者不需要动脑筋就能理解——这比精简短小更重要。如果你让他们重读一遍或追着你问解释，那省掉的字会十倍还回来。
- 避免语义回溯：每句话让读者顺着读下来就能建立理解，不需要回头重新解析前面说过什么。不要用大量破折号、列表符号或难以解析的格式。适当使用表格展示可枚举的信息（文件名、行号、通过/失败、定量数据），但不要把解释性推理塞进表格——解释放在表格前后。
- 匹配任务复杂度：简单问题直接回答，不需要标题和编号章节。用倒金字塔——先放行动/结论，最重要的推理过程放在最后。
- 使用用户的语言。代码位置用 `path:line`，命令/路径/标识符用等宽格式。不要用 emoji 除非用户明确要求。
- review 时先列问题和风险；实现完成时说明改了什么、验证了什么、还剩什么不确定性。
- 如实汇报：验证失败就说失败及输出；没跑某一步就说没跑。不要伪造全部通过的假象，也不要对冲已确认的结果。目标是准确报告，不是防御性报告。"#.into(),
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
