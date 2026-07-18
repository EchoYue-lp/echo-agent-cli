# M11 P0 专业域 Subagent 编排闭环

## 目标

EKO 的统一产品模型是 `TaskRun -> PlanTask -> SubagentRun`。本阶段让已经存在的 `DomainProfile` 从 Run 真实传播到计划节点、Subagent 选择、执行提示和 review gate，并修复 `create_complex_task` 创建空 Run 后直接进入 DAG executor 的断点。

本阶段覆盖四个结果：

1. `TaskRun.domain_profile` 是领域事实源，`plan_create` 生成的每个 PlanTask 自动继承它。
2. `plan_create` 可显式选择已注册的 `subagent`；未选择时按领域和 task kind 给出稳定默认值。
3. `create_complex_task` 先驱动独立主 Agent 的 ReAct 循环，由它选择“生成正式 DAG 并执行”或“直接完成”，不再对空 plan 调 `execute_run`。
4. `echo-agent` / `echo-agent-cli` 统一使用 Subagent 术语；本阶段触及的旧称谓同步迁移。

## 业界依据

- [Claude Code subagents](https://code.claude.com/docs/en/sub-agents)：Subagent 拥有独立上下文、工具和权限边界，主 Agent 通过描述选择合适角色并接收结果。它强调按任务委派，不增加第二套执行角色模型。
- [OpenAI Codex app-server](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)：长任务仍以 thread/turn/item 的稳定事件和终态收敛；计划与执行由 Agent 行为和事件表达，不需要为“是否已计划”增加新的 Run 主状态。
- [OpenAI Deep Research](https://openai.com/index/introducing-deep-research/)：复杂研究先形成可检查的研究路径，再持续执行、综合来源并交付带依据的结果；领域方法应进入任务提示和验收合同，而不是只作为 UI 标签保存。
- [ChatGPT 数据分析](https://help.openai.com/en/articles/8437071-data-analysis-with-chatgpt)：数据任务在隔离执行环境中读取、转换、分析并生成可检查产物；数据整形和分析是不同能力，应通过具体 Subagent 角色表达。

跨系统共性是：主 Agent 根据任务选择专业 Subagent；计划是可观察 artifact；运行终态由事实事件收敛；领域方法通过上下文、工具和验收要求约束执行。EKO 因此复用现有 TaskRuntime、Subagent registry 和 `ProfileTemplate`，不新增领域状态机。

## 现状审计

- `TaskRun`、`TaskPlan`、`PlanTask` 都已有 `domain_profile`，但 `TaskCreateTool` 构造 PlanTask 时使用 `Default`，所以所有新节点实际落成 `general`。
- `ProfileTemplate.prompt_suffix` 和默认角色目录基本只被测试读取；review gate 是目前唯一稳定消费 task domain 的运行路径。
- `data-shaper` 与 `analyst` 已注册，但 `plan_create` 不能指定 Subagent，默认路由也从不选择它们。
- `create_complex_task` 已把领域和 goal 写入 Run，却立即调用只接受“已有 plan”的 `execute_run`；没有 plan 时会返回 `NoPlan`。
- cron/background service 已有正确范式：独立主 Agent 先执行 ReAct，可调用 `plan_create` + `plan_execute`，直接回答时则按完成门禁收敛。

## 框架与应用边界

### `echo-agent`

本阶段不改框架。Subagent registry、Fork/Team dispatch、结构化结果、取消、timeout、worktree/tmpdir 隔离都属于已存在的通用能力。

### `echo-agent-cli`

领域选择、内置角色默认值、`create_complex_task` 产品语义、TaskRuntime 持久化和 UI trace 都是 EKO 决策，全部留在应用层。

## 设计决策

### 1. Run 是领域事实源

`plan_create` 在确保 Run 存在后读取 `TaskRun.domain_profile`，写入新 PlanTask。调用方不重复传 domain，避免一个 Run 内出现互相冲突的领域标签。lazy bootstrap 的 TaskPlan 继续从同一个 Run 继承领域。

### 2. 参数名使用 `subagent`

`plan_create` 新增可选字符串参数 `subagent`，值是 registry 中的角色名。它不使用封闭 enum，因为项目级和用户级 `.eko/subagents/**/*.md` 可以扩展角色。

默认路由保持保守：

| DomainProfile | PlanTask kind | 默认 Subagent |
|---|---|---|
| general / ai_coding / academic_research / medical_research | investigation / read_only_review / test_plan | explorer |
| 同上 | review | reviewer |
| 同上 | summary | summarizer |
| 同上 | implementation / debugging | implementer |
| data_analysis | implementation / debugging | analyst |
| 任意 | verification | primary |

数据清洗、schema 对齐和可复现导出由规划 Agent 显式选择 `data-shaper`；统计、建模、可视化和综合分析选择 `analyst`。只读 kind 不默认选择带写入能力的数据 Subagent，避免把只读边界降级为提示词约束。

### 3. 领域方法进入规划和执行

复杂 Run 的主提示包含对应 `ProfileTemplate.prompt_suffix` 和可用 Subagent 目录，让规划 Agent 生成领域适配的 DAG。PlanTask 自身继承 domain，使 review gate 使用同一 checklist。Subagent 的任务描述继续携带具体问题、依赖摘要、产物和 verification，不复制一套领域状态。

### 4. `create_complex_task` 驱动 Agent，而不是空 DAG

独立 pool agent 先注册 `plan_execute`，再在该 Run 的 context 中执行 ReAct：

- `plan_then_execute`：提示明确要求先用 `plan_create` 物化可审查 DAG，再调用 `plan_execute()`。
- `direct_execute`：允许 Agent 直接使用普通工具完成；若执行中发现真实依赖或并行需求，仍可升级为正式 plan。

Agent stream 结束后读取持久化 Run 状态并经过已有 completion gate 收敛。foreground 继续转发 trace，background 使用独立 cancel token；成功后沿用 blocking memory write。该路径不增加 `Planning/AwaitingApproval/Ready` 等状态。

### 5. Subagent 单一术语

本阶段修改到的类型、函数、参数、日志、注释和文档统一使用 `subagent`。不存在第二套执行角色层；并发许可、pool entry 和执行实例都只是 Subagent runtime 的实现细节。

## 验收

- DataAnalysis Run 中通过 `plan_create` 新建的 PlanTask 保留 `data_analysis`，默认写任务选择 `analyst`，显式 `subagent=data-shaper` 不被覆盖。
- AcademicResearch / MedicalResearch Run 的 PlanTask 继承对应 profile，review prompt 使用相应 checklist。
- `create_complex_task(plan_then_execute)` 不会在空 plan 上直接调用 `execute_run`，独立 Agent 可物化并执行 DAG。
- `create_complex_task(direct_execute)` 可在不创建 plan 时正常完成，不产生 `NoPlan`。
- foreground trace、background cancel、terminal status 和 blocking memory 保持现有合同。
- 新增和修改的代码只使用 Subagent 术语，不增加框架 API 或 Run 主状态。
