# ADR 0038：统一 TaskRun 与 Subagent 执行模型

- Status: Proposed
- Date: 2026-09-04
- Owners: `tasks/task_runtime`、`agent_pool`、`subagent`

## 背景

EKO 的一个大目标需要跨越需求拆解、计划、实施、Review 和交付，并且可以在实施阶段
继续增加步骤。该过程应当由一个 `TaskRun` 和一条全局 revisioned Task Graph 承载，
而不是把每个阶段拆成新的 TaskRun 或新的任务队列。

当前还容易混淆 `TaskRun`、`PlanTask`、`SubagentRun`、直接 `agent_tool`、嵌套派发、
`Sync`、`Fork` 和 `AgentPool`。如果 Subagent 可以继续调用 `agent_tool`，派生数量会
无界，且多个局部 Semaphore 不能表达 EKO 的真实产品上限。

## 参考实现

- Claude Code 官方 Subagent 文档将运行中并发上限与嵌套深度分开，并在容量耗尽时拒绝
  新派发：
  <https://code.claude.com/docs/en/sub-agents#concurrent-subagent-limit>。
- EKO ADR 0003 已记录 Agent、Task CRUD、后台句柄和 Plan artifact 的边界。
- EKO ADR 0015 已将 `TaskRun -> PlanTask -> SubagentRun` 固定为唯一关系图。
- `echo-agent` 已提供 `RuntimeTaskService`、`NestedDelegationPolicy` 和
  `KeyedExecutionAdmission`；EKO 当前 `EkoExecutionLimits`/`PROCESS_EXECUTION_GOVERNOR`
  的 Subagent 部分是应用层重复实现，应收敛到 framework 原语。

## 决策

1. 一个活动用户大目标只有一个 `TaskRun`、一个 TaskRuntimeStore 和一个权威任务图。
   阶段拆分通过同一 graph 的新 revision 完成；不创建嵌套 TaskRun、子队列或第二个
   Todo/TaskRuntime authority。
2. `PlanTask` 是全局队列节点，`SubagentRun` 是该节点的一次执行 attempt。Subagent
   的重试仍属于原 PlanTask 和原 TaskRun。
3. 主智能体保留 `agent_tool`；Subagent 不注册 `agent_tool`/`task_execute`，并且 runtime
   对 `delegate_depth >= 1` 返回 `delegation_depth_exceeded`。最多只有主智能体到一层
   Subagent。
4. Subagent 只能返回拆分建议、证据和结果；任务图 revision 的提交由主智能体和
   TaskRuntime 的现有权威 API 完成。
5. EKO 以 `max_concurrent_subagents` 表达进程内运行中 `SubagentRun` 总数，默认 `5`；
   该产品值通过 framework `KeyedExecutionAdmission` 的 shared execution admission 注入
   `RuntimeTaskService` 和 `SubagentExecutor`。TaskRuntime、direct `agent_tool`、Sync、
   Fork 和保留的 Teammate 路径共享同一 admission。
6. Framework 的 `RuntimeTaskServiceConfig.max_concurrent_subagents` 继续表示独立
   framework consumer 的 per-runtime 调度宽度；`SubagentExecutorConfig.max_concurrent_forks`
   继续作为 standalone Fork fallback。两者不再作为 EKO 的第二个产品配额。
7. `Sync`/`Fork` 是等待和隔离模式，不是两种不同的 Subagent 配额。`AgentPool` 只负责
   Agent 实例、workspace、model、memory 和 lease，不负责任务图或 Subagent 层级。

## 取舍

单一 TaskRun 保留目标、revision、依赖、恢复和证据的一致生命周期，代价是动态展开需要
严格的 revision/CAS。关闭递归 Subagent 防止无限扇出、资源耗尽和结果归属混乱，代价是
复杂协调必须回到主智能体和同一任务图。单一产品并发配额减少了多个 Semaphore 的心智
分裂，代价是 framework 独立消费者仍需保留自己的兼容配置。EKO 删除重复的 Subagent
process governor，但保留 AgentPool 前台执行、TaskRuntime write/shell/LLM 等不同资源类的
独立额度。

## 影响范围

- framework：继续拥有 Task graph、Subagent depth 和通用 admission；扩展现有
  `KeyedExecutionAdmission`/execution admission 接口，避免 EKO 自建同义 semaphore；
- app-core：注入 EKO 默认值、委派 capability policy，并组合 TaskRuntime、AgentPool 和
  durable projection；
- generated contracts/tests/docs：同步深度拒绝、容量状态、配置和恢复行为。

## 状态与后续

本 ADR 记录已确认的目标运行模型，当前仍为 Proposed。实施前需基于绑定本设计 revision
的执行计划拆分 framework、app-core 和验证交付；代码、测试和恢复验证通过后再更新为
Accepted 并附 commit 证据。
