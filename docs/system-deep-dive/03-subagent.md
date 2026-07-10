# 03 · Task / Subagent / AgentPool

> **归属**:框架提供 Subagent 原语与 `agent_tool`;EKO 应用层负责 TaskRuntime、角色 frontmatter、UI 投影和 AgentPool。
> **当前事实**:产品模型只保留 Task + Subagent。旧 Worker 协议、`worker://trace`、`WorkerTraceEvent` 和前端 worker store 已删除;`worker` 只作为内部调度实现词保留,例如 `TaskWorker` trait、semaphore、pool slot。

本文记录 EKO 当前的多 agent 执行模型:Task 负责目标拆解和状态流转;Subagent 负责专业角色执行;AgentPool 提供多会话/后台隔离。

---

## 0. Fresh vs Fork（语义锁定，Phase 0）

| 概念 | 含义 |
|---|---|
| **Fresh inheritance** | 不继承父 system / history / memory（默认；对标 Claude Code / Cursor） |
| **Fork inheritance** | `agent_tool` 设 `mode=fork` 时继承父 system + 最近消息 |
| **`ExecutionMode::Fork`** | 并发调度 + worktree/workspace 物理隔离路径；**不等于**必须继承上下文 |
| **`agent_tool`** | 主 agent 即时委派（与 `plan_execute` 并存） |
| **`plan_execute`** | TaskRuntime DAG 编排 |

产品默认：TaskRuntime / `delegate_to_agent_with_parent_context_*` 仍走 **`ExecutionMode::Fork`**（保住 implementer worktree / data workspace），但 parent_context 用 **fresh inheritance**。

---

## 1. Subagent 角色来源

EKO 的可派发角色由 `.md` frontmatter 热加载:

- 项目级:`<project>/.eko/subagents/**/*.md`
- 用户级:`~/.echo-agent/subagents/**/*.md`
- 内置:`echo-agent-app-core/src/subagents/**`

主 agent 的 system prompt 会注入 `format_subagent_catalog` 生成的 **Available subagents** 列表（name + description + flags），驱动 `agent_tool` 按 description 委派。

核心字段:

```yaml
---
name: explorer
description: Read-only repository investigation
readonly: true
model: fast          # omit | inherit | fast | concrete model id
max_turns: 30        # optional; omit = unlimited (builder default 0)
is_background: false # true → agent_tool 默认走 background dispatch
worktree: false
workspace: false
can_delegate: false
---
```

语义:

- `readonly: true`:应用层用 readonly builder 注册,工具集物理只读。
- `model`: `inherit`/缺省 → 父模型；`fast` → `EKO_FAST_MODEL`（未设则回退父模型）；其它字符串 → 具体 model id。
- `max_turns`:写入 `SubagentDefinition.max_iterations` 与 worker ReactAgent builder。
- `is_background`:写入 `SubagentDefinition.is_background`。为 true 时（或 `agent_tool` 传 `background: true`）走框架 `dispatch_background`：立即返回 `{status:"started", execution_id, agent_name}`，worker 在后台跑；`DispatchStarted.background=true` 经 `execution://event` 到 GUI 卡片；完成后向主会话插入 `[subagent {name} finished]\n{summary}` 注记（不打断父 ReAct 当前 turn）。TUI 回灌延期。
- `worktree: true`:writer role 通过框架 Fork worktree 隔离写入。
- `workspace: true`:data/research role 使用无 git 的隔离数据工作区。
- `can_delegate: true`:该 role 显式获得 `agent_tool`,并注册 child subagent registry。

内置 8 个角色：`explorer`（默认 `model: fast`）、`reviewer`、`planner`、`summarizer`、`implementer`、`general-purpose`、`data-shaper`、`analyst`。

父 LLM 回传：`SubagentResult.summary`（从 `## Summary` 提取，缺省 UTF-8 安全截断 output）；`agent_tool` / TaskRuntime 父上下文优先吃 summary，全文保留在 `output` 供 UI。

默认 subagent 只能完成当前 PlanTask,或在结果里返回结构化 `suggested_tasks`。这些 suggested tasks 只能由主 TaskRuntime 统一 append 到全局 plan。

---

## 2. `agent_tool` 与嵌套委派

框架工具入口是 `agent_tool`:

```rust,ignore
// echo-agent/src/tools/builtin/agent_dispatch.rs
pub struct AgentDispatchTool {
    executor: Arc<SubagentExecutor>,
    parent_agent: String,
    cancel: Arc<Mutex<Option<CancellationToken>>>,
    parent_context_factory: Option<Arc<ParentContextFactory>>,
}
```

**主 agent** 在 `create_agent` 中调用 `.register_agent_dispatch_tool()`，与 `plan_execute` 并存，用于即时委派。

EKO **不**把 `agent_tool` 默认发给所有 subagent。只有 `.md` 显式 `can_delegate: true` 的 role 会再注册一层嵌套委派。

`mode` 参数:

- 省略 / `sync` → fresh inheritance（推荐）
- `fork` → fork inheritance（需要共享会话背景时）
- 目标 role 声明 `worktree`/`workspace` 时，执行路径自动升为 `ExecutionMode::Fork`（与 inheritance 解耦）

`background` 参数（可选 bool）:

- `true` **或** 目标 role `is_background: true` → `dispatch_background`（非阻塞）
- 否则 → 阻塞 `dispatch`，工具结果为 summary

`worktree: true` 的 writer（如 builtin `implementer`）在无 `WorktreeFactory` 时 **硬失败**，不会静默共写主树。

嵌套深度由同一套 `NestedDelegationPolicy` 管:

- 类型定义在 `echo_core::tools::NestedDelegationPolicy`。
- `echo_agent::tasks::NestedDelegationPolicy` 继续 re-export 同一个类型。
- `ExternalRunContext.delegation_policy` 把当前 worker policy 跨 `tokio::spawn` 注入 worker agent。
- `ToolContext.delegation_policy` 在工具执行阶段传给 `agent_tool`。
- `agent_tool` 每次调用都会用 `child_policy()` 推进深度;超过上限直接返回工具错误,不再派发。

EKO 首层 PlanTask worker 使用 depth 0,默认 max depth 2。普通 role 即使拿到 policy,也没有 `agent_tool`,所以不能嵌套委派。

---

## 3. Subagent 事件与 UI

实时执行流统一走 `execution://event`。

前端单一数据源:

- store:`web-frontend/src/stores/subagentRunStore.ts`
- inline block:`components/chat/SubagentStreamBlock.tsx`
- detail panel:`components/task/SubagentDetailView.tsx`
- right rail/card:`components/subagent/SubagentCard.tsx`

旧链路已删除:

- `worker://trace`
- `subagent://event` 作为单独 UI 数据源
- `WorkerTraceEvent` / `WorkerTraceEventKind`
- `workerTraceStore` / `workerDetailStore`
- `WorkerStreamBlock` / `WorkerDetailView`

`SubagentRun.subagentRunId` 是稳定 execution id,通常为 `{task_id}:{attempt}`。聊天流和右栏都基于同一份 `subagentRunStore` 渲染,只是展示位置不同。

---

## 4. TaskRuntime 调度边界

TaskRuntime 仍是全局 run 状态的唯一 owner:

- plan/todo/run 状态由 `TaskRuntimeStore` 管理。
- subagent 只执行当前 task,不能直接修改全局 plan。
- follow-up 需求通过 `SuggestedTask` 返回,由主 runtime 决定是否 append。
- HITL、取消、UI 树、run 记忆写入都在应用层统一管理。

框架提供通用原语:

- `RuntimeTask`
- `TaskWorkerContext`
- `ConcurrencyLimits`
- `NestedDelegationPolicy`
- `SubagentExecutor`
- `agent_tool`

EKO 应用层保留产品逻辑:

- file store / UI projection
- approval gate
- `execute_plan`
- `task_create/update/complete/skip/list`
- role frontmatter and built-in role catalog

---

## 5. AgentPool

`AgentPool` 是 EKO 产品层能力,不属于框架:

```rust,ignore
pub struct AgentPool {
    shared: SharedResources,
    agents: RwLock<HashMap<String, PooledAgent>>,
    config: PoolConfig,
}
```

它解决的是多会话、多后台任务和 TUI/GUI/channel 功能对等:

- 每个会话 agent 有独立 `ContextManager` 和 `execution_mutex`。
- LLM client、tool manager、hook registry、store、sandbox 等重资源通过 `SharedResources` 共享。
- `__background__` 和 task-specific agents 用于后台/复杂任务隔离。
- idle 驱逐以 `execution_mutex().try_lock()` 为 ground truth,不维护第二套 busy flag。

AgentPool 不改变 Subagent 协议;它只是决定某个入口使用哪个 ReactAgent 实例来执行。

---

## 6. 保留的内部 worker 命名

以下名字可以保留,因为它们是内部执行抽象,不是产品概念:

- `TaskWorker` trait
- `TaskWorkerContext`
- `RealTaskDispatcher` / test dispatcher
- tokio worker thread / worker semaphore
- framework team 模式里的 `ManagerWorkerOrchestrator`
- generated schema 中已持久化的 usage 字段如 `worker_id`、`worker_prompt_hash`

不要重新引入以下产品/协议概念:

- `WorkerTraceEvent`
- `worker://trace`
- `workerTraceStore`
- `WorkerStreamBlock`
- “Worker 状态”作为 UI 一等入口

后续如果要扩展执行树,统一扩 `SubagentRun` / `ExecutionEvent`。
