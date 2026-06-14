# 02 · 长程任务规划

> **归属**：横跨框架（`echo-agent` + 嵌套的 `echo-orchestration` crate）。
> **接口**：`ReactAgent` 通过 `MemorySubsystem.state_store` 持有 `RuntimeStateStore`；任务工具通过 `enable_task=true` 注册到 `ToolManager`；`TaskExecutor`、`Workflow Graph` 是独立的执行栈，不与核心 ReAct 循环混用。

本文剖析 Echo Agent 的长程任务能力：`TaskNode` 状态机、**三种"checkpoint"概念的辨析**（同名但语义完全不同）、任务工具集（含一个 wiring 空缺）、`echo-orchestration` 的整体形态、`save_runtime_checkpoint` 的全部触发条件、任务 DAG 的 hydration 流程。

---

## §1 `TaskNode` —— 6 状态的 DAG 节点

```rust,ignore
// echo-agent/src/state/mod.rs:57
pub struct TaskNode {
    pub id:           String,
    pub name:         String,
    pub status:       TaskNodeStatus,
    pub dependencies: Vec<String>,        // 上游节点 ID
    pub outputs:      serde_json::Value,
    pub created_at:   DateTime<Utc>,
    pub updated_at:   DateTime<Utc>,
}
```

注意：DAG 拓扑通过 `dependencies: Vec<String>` 表达，**没有 `parent` 字段** —— 一个节点可以同时依赖多个上游。

```rust,ignore
// echo-agent/src/state/mod.rs:26
pub enum TaskNodeStatus {
    Pending,
    Running,
    Success,
    Failed,
    Blocked  { reason: String },
    Hydrated,         // ← 关键：进程崩溃后 Running → Hydrated（详见 §6）
}

impl TaskNodeStatus {
    pub fn is_terminal(&self) -> bool {     // mod.rs:43
        matches!(self, Success | Failed)
    }
    pub fn is_blocked(&self) -> bool {      // mod.rs:48
        matches!(self, Blocked { .. })
    }
}
```

`Hydrated` 是把"进程被杀时正在 Running 的节点"与"还没启动的 Pending 节点"区别开的关键状态 —— 它保留了"曾经跑过"的信息但避免被简单当成可继续的 Pending 重启。

---

## §2 ⚠️ 三种"Checkpoint"概念辨析（重要）

代码库里有**三个不同的 trait 都叫 `*Checkpoint*`**，处理三件完全不同的事。混淆它们是阅读这块代码时最常见的错误。

| 名称 | trait/struct 路径 | 数据形态 | 用途 |
|------|------------------|----------|------|
| `RuntimeStateStore::save_checkpoint` | `echo-agent/src/state/mod.rs:178` | `AgentCheckpoint`（每 conversation 一份） | ReactAgent 单轮对话的崩溃恢复 |
| `CheckpointStore::save_checkpoint` | `echo-orchestration/src/tasks/store.rs:163` | `ExecutionCheckpoint`（任务 DAG 快照） | TaskExecutor 长程任务执行恢复 |
| `workflow::CheckpointStore` | `echo-orchestration/src/workflow/checkpoint_store.rs` | Graph 节点状态 | LangGraph 风格 workflow 的 pause/resume |

它们存的不是同一个东西、不是同一段生命周期、也不是同一个用例。本套文档大多数提到 "checkpoint" 时指的是**第一种**（`RuntimeStateStore`）。

### §2.1 `RuntimeStateStore::AgentCheckpoint` —— ReAct 循环的运行时状态

```rust,ignore
// echo-agent/src/state/mod.rs:115
pub struct AgentCheckpoint {
    pub conversation_id: String,
    pub messages_json:   String,
    pub current_plan:    Option<String>,
    pub active_skills:   Vec<String>,
    pub blocked_reason:  Option<String>,
    pub timestamp:       DateTime<Utc>,
}
```

trait 接口（`mod.rs:149-188`）：

```rust,ignore
pub trait RuntimeStateStore: Send + Sync {
    fn save_node(&self, conv_id: &str, node: TaskNode) -> BoxFuture<…>;
    fn load_nodes(&self, conv_id: &str) -> BoxFuture<…<Vec<TaskNode>>>;
    fn update_status(&self, conv_id: &str, node_id: &str, status: TaskNodeStatus) -> BoxFuture<…>;
    fn get_checkpoint(&self, conv_id: &str) -> BoxFuture<…<Option<AgentCheckpoint>>>;
    fn save_checkpoint(&self, ckpt: &AgentCheckpoint) -> BoxFuture<…>;
    fn clear_conversation(&self, conv_id: &str) -> BoxFuture<…>;
}
```

唯一的生产实现是 `SqliteRuntimeStateStore`（`echo-agent/src/state/sqlite.rs:12`），cfg-gated 在 `feature = "sqlite"`，使用两张表：
- `agent_checkpoints` —— PK = `conversation_id`，单一的"最新检查点"快照
- `task_nodes` —— PK = `(id, conversation_id)`，多行的 DAG 节点

存放路径由产品层决定（默认 `~/.echo-agent/state.db`）。

### §2.2 `CheckpointStore::ExecutionCheckpoint` —— 任务 DAG 执行恢复

```rust,ignore
// echo-agent/echo-orchestration/src/tasks/store.rs:126
pub struct ExecutionCheckpoint {
    pub tasks:               Vec<Task>,
    pub completed_task_ids:  Vec<String>,
    pub plan_id:             Option<String>,
    pub created_at:          DateTime<Utc>,
}

// store.rs:163
pub trait CheckpointStore: Send + Sync {
    fn save_checkpoint(&self, ckpt: ExecutionCheckpoint) -> BoxFuture<…>;
    fn load_latest_checkpoint(&self, plan_id: Option<&str>) -> BoxFuture<…>;
    fn list_checkpoints(&self, plan_id: Option<&str>, limit: usize) -> BoxFuture<…>;
}
```

实现：`SqliteCheckpointStore`（`store.rs:186`），命名空间 `["checkpoints"]`，背靠 `Store` trait。

它的用例是 `TaskExecutor`（详见 §4）跑长程多任务时——某个 task 被 retry / cancel / 重启进程后，从最近一次 checkpoint 续跑而不是从头来。

### §2.3 Workflow `CheckpointStore` —— Graph 暂停/恢复

`echo-orchestration/src/workflow/checkpoint_store.rs` 提供 LangGraph 风格 workflow 的 pause/resume。`Graph::run_until_interrupt`（`workflow/graph.rs:531`）会在 `before/after` interrupt 节点时返回 `RunUntilInterruptResult::Interrupted(InterruptState)`，外部代码持有该 state 决定何时再 `Graph::resume`。这是 workflow 引擎的本地概念，与 ReactAgent 的 turn-级 checkpoint 没有直接交互。

---

## §3 任务工具集（`enable_task=true` 注册）

```rust,ignore
// echo-agent/src/agent/react/mod.rs:352-367 (cfg "tasks")
if config.enable_task {
    tool_manager.register(Box::new(CreateTaskTool::new(task_manager.clone())));
    tool_manager.register(Box::new(UpdateTaskTool::new(task_manager.clone())));
    tool_manager.register(Box::new(ListTasksTool::new(task_manager.clone())));
    tool_manager.register(Box::new(VisualizeDependenciesTool::new(task_manager.clone())));
    tool_manager.register(Box::new(GetExecutionOrderTool::new(task_manager.clone())));
    tool_manager.register(Box::new(SpawnBackgroundTaskTool::new(task_spawner.clone())));
    tool_manager.register(Box::new(CheckTaskStatusTool::new(task_spawner.clone())));
    tool_manager.register(Box::new(ListBackgroundTasksTool::new(task_spawner.clone())));
}
```

| LLM 可见的工具名 | 实现 | 文件 |
|----------------|------|------|
| `create_task` | `CreateTaskTool` | `src/tools/builtin/task.rs:15-300` |
| `update_task` | `UpdateTaskTool` | `task.rs:302-411` |
| `list_tasks` | `ListTasksTool` | `task.rs:413-491` |
| `visualize_dependencies` | `VisualizeDependenciesTool` | `task.rs:493-529` |
| `get_execution_order` | `GetExecutionOrderTool` | `task.rs:531+` |
| `spawn_background_task` | `SpawnBackgroundTaskTool` | `src/tools/builtin/spawn_task.rs` |
| `check_task_status` | `CheckTaskStatusTool` | `src/tools/builtin/check_task.rs` |
| `list_background_tasks` | `ListBackgroundTasksTool` | (同上) |

### §3.1 ⚠️ `"plan"` 工具的 wiring 空缺

```rust,ignore
// echo-agent/src/agent/react/mod.rs:80-84
pub(crate) const TOOL_CREATE_TASK: &str = "create_task";
pub(crate) const TOOL_PLAN:        &str = "plan";
pub(crate) const TOOL_UPDATE_TASK: &str = "update_task";

// react/mod.rs:202-207
pub(crate) fn has_planning_tools(&self) -> bool {
    self.tools.tool_manager.is_registered(TOOL_CREATE_TASK)
        && [TOOL_PLAN, TOOL_CREATE_TASK, TOOL_UPDATE_TASK]
            .iter()
            .all(|name| self.tools.tool_manager.is_registered(name))
}
```

`TOOL_PLAN = "plan"` 常量存在；`has_planning_tools()` 检查中也包含它；但**生产代码中没有任何地方注册名字为 `"plan"` 的工具**。`CreatePlanTool`（`src/tools/builtin/plan_tool.rs:16`）实现存在，但 `name()` 返回 `"create_plan"`，且仅在测试中被实例化、未被任何 `register(...)` 调用。

实际后果：`has_planning_tools()` 即便在 `enable_task=true` 时也**总是返回 false**。这是 wiring 残留 / 无害悬挂代码，记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 1 项，待跟进确认是补 register 还是删常量。

### §3.2 `TodoWriteTool` —— 不依赖 `enable_task` 的内置 todo

`TodoWriteTool`（`src/tools/builtin/todo.rs:26`，`name()="todo_write"`）在 `react/mod.rs:306` **无条件注册**，与 `enable_task` 无关。它内部用一个 `static LazyLock<Mutex<Vec<TodoEntry>>>`（最多 100 条）作为内存中的 to-do 列表，这套是**进程内**的轻量任务记忆，不写入 `RuntimeStateStore`，也不参与 DAG。

---

## §4 `echo-orchestration` crate 概览

嵌套位置：`echo-agent/echo-orchestration/`（在 echo-agent 的 workspace 内，**不是顶层独立 crate**）。

```
echo-orchestration/src/
├── lib.rs
├── tasks/
│   ├── store.rs        # TaskStore / CheckpointStore + Sqlite impl
│   ├── executor.rs     # TaskExecutor (semaphore-限流并发执行)
│   ├── task.rs         # Task / CheckpointPolicy
│   └── ...
├── workflow/
│   ├── graph.rs        # LangGraph 风格 Graph
│   ├── state.rs        # SharedState
│   ├── node.rs / sequential.rs / concurrent.rs / dag.rs
│   ├── checkpoint_store.rs
│   └── ...
├── planning/           # 计划生成与校验
├── scheduler/          # 调度
└── human_loop/         # HITL
```

### §4.1 `TaskExecutor` —— 长程任务执行器

```rust,ignore
// echo-agent/echo-orchestration/src/tasks/executor.rs:59
pub struct TaskExecutorConfig {
    pub max_concurrent:           usize,    // 默认 5
    pub default_timeout_secs:     u64,      // 默认 300
    pub retry_delay_secs:         u64,
    pub retry_backoff_factor:     f64,
    pub retry_max_delay_secs:     u64,
    pub retry_jitter:             bool,
    pub enable_hooks:             bool,
    pub checkpoint_interval_secs: u64,
    pub unified_hook_executor:    Option<UnifiedHookExecutorFn>,  // 桥到 echo-core hooks
    pub round_timeout_secs:       u64,      // 默认 3600 (1 小时)
}
```

`TaskExecutor`（`executor.rs:317`）拥有 `Arc<TaskManager>` + `Arc<Semaphore>` 控并发 + `Arc<TaskHookRegistry>` + 可选 `Arc<dyn CheckpointStore>` + 可选 `Arc<dyn TaskStore>`，通过 `tokio::spawn` 跑每个就绪任务，配 `tokio::time::timeout` 超时杀 + 重试退避 + cancel token。

### §4.2 `Task::checkpoint_policy`

```rust,ignore
// echo-agent/echo-orchestration/src/tasks/task.rs:177
pub enum CheckpointPolicy {
    AfterEach,      // 每个 task 完成后保存
    OnMilestone,
    OnFailure,      // 默认
    Never,
}
```

每个 `Task` 自带一个 `checkpoint_policy: CheckpointPolicy` 字段（`task.rs:470`）；`TaskExecutor` 据此决定何时调用 `CheckpointStore::save_checkpoint`。

> 与 `RuntimeStateStore` 的"无条件"触发（详见 §5）不同 —— `RuntimeStateStore` 的检查点写入由代码路径决定，不由 enum 控制；`TaskExecutor` 的 `ExecutionCheckpoint` 写入由本字段控制。

### §4.3 Workflow Graph

```rust,ignore
// echo-agent/echo-orchestration/src/workflow/graph.rs:531
pub struct Graph {
    pub name:                String,
    pub nodes:               HashMap<String, Node>,
    pub edges:               HashMap<String, Vec<Edge>>,
    pub entry:               String,
    pub finish_nodes:        Vec<String>,
    pub max_steps:           usize,
    pub interrupt_config:    InterruptConfig,
    pub checkpoint_store:    Arc<dyn CheckpointStore>,
    pub cancel_token:        Option<CancellationToken>,
}
pub const END: &str = "__end__";
```

`InterruptConfig`（`graph.rs:103`）：声明 `before: Vec<String>` / `after: Vec<String>` 节点名集合（支持 `"*"`），运行至这些节点前/后会变成 `RunUntilInterruptResult::Interrupted(InterruptState)`，外部 caller 决定何时 resume。这是用于"开始执行 X 前先让人审一下"的 HITL pause。

---

## §5 `RuntimeStateStore` 检查点触发条件表

`AgentRunSnapshot::save_runtime_checkpoint`（`src/agent/snapshot.rs:275`）的全部生产调用点：

| 触发原因 | 文件:行 | `blocked_reason` |
|---------|---------|------------------|
| 每轮压缩前 | `phases/compact.rs:31` | `None` |
| 工具错误（concurrent 批） | `phases/tools.rs:177` | `Some("Tool error: {fname}")` |
| 工具错误（approval 批） | `phases/tools.rs:229` | `Some("Tool error: {fname}")` |
| 周期性（按 `react_checkpoint_interval`） | `phases/tools.rs:240-242` | `None` |
| Final answer（工具分支） | `phases/finalize.rs:72` | `None` |
| Final answer（文本分支） | `phases/finalize.rs:152` | `None` |
| Max iterations 强制终止 | `phases/finalize.rs:237` | `Some("Max iterations exceeded")` |
| 外部主动调用 | `react/mod.rs:1308` `agent.save_runtime_checkpoint(...)` | 由调用方决定 |

每次写入都是把 `AgentCheckpoint`（`messages_json + current_plan + active_skills + blocked_reason + timestamp`）整体覆盖 `agent_checkpoints` 表对应 `conversation_id` 的那一行 —— 不是追加日志，是"最新状态的全量快照"。

---

## §6 `update_node_status` 调用矩阵

`AgentRunSnapshot::create_execution_node`（`snapshot.rs:419`）在 `prepare_turn` 中调用一次，生成 `format!("exec-{}", uuid::Uuid::new_v4())` 的节点 ID 并以 `Running` 状态写库；后续状态由 `update_node_status`（`snapshot.rs:443`）在以下点更新：

| 状态 | 触发位置 |
|------|----------|
| `Failed`（intervention cancel） | `phases/think.rs:41-43` |
| `Blocked { reason }`（intervention block） | `phases/think.rs:54-61` |
| `Success`（工具分支正常终结） | `phases/finalize.rs:77-78` |
| `Success`（文本分支正常终结） | `phases/finalize.rs:159-160` |
| `Failed`（无响应） | `phases/finalize.rs:209-210` |
| `Failed`（max iterations） | `phases/finalize.rs:244` |

---

## §7 任务 DAG 水合 —— 进程重启后的恢复路径

进程被杀时，`TaskNode` 通常停留在 `Running` 状态（写过状态但还没机会更新到终态）。重启后的恢复流程：

```rust,ignore
// echo-agent/src/agent/snapshot.rs:459
pub async fn hydrate_running_nodes(&self) {
    if let Some(ref store) = self.state_store {
        if let Some(ref conv_id) = self.config.conversation_id {
            // 读全部节点
            if let Ok(nodes) = store.load_nodes(conv_id).await {
                for node in nodes {
                    if node.status == TaskNodeStatus::Running {
                        // 关键：Running → Hydrated（不是 Pending！）
                        let _ = store
                            .update_status(conv_id, &node.id, TaskNodeStatus::Hydrated)
                            .await;
                    }
                }
            }
        }
    }
}
```

为什么不是 `Pending` 而是 `Hydrated`？

- `Pending` 表示"尚未启动"，自动调度器会去执行它。
- `Running` 在重启后已经误导：它不在跑了。
- `Hydrated` 是**第三种状态**："曾经跑过但被中断"，让上层逻辑（产品 / 编排器）自己决定是放弃、追问、还是显式重启。代码不会主动把它当 Pending 抓起来再跑一次。

调用点：`ReactAgent::resume_from_state_store`（`react/mod.rs:1233`）：

```rust,ignore
pub async fn resume_from_state_store(&self) -> Result<Option<AgentCheckpoint>> {
    let cp = state_store.get_checkpoint(conv_id).await?;
    // 1. 反序列化 messages_json，set_messages 替换 ContextManager 的 buffer
    // 2. plan_state ← cp.current_plan
    // 3. 对 cp.active_skills 中的每个 skill 调 skill_registry.mark_activated()
    // 4. AgentRunSnapshot::from_agent(self).hydrate_running_nodes() ← 本节核心
    // 5. log cp.blocked_reason（若有）
}
```

整条 lifecycle：

```
[turn N]                       [crash]                  [restart]
  ↓                              ↓                          ↓
Running ──save_node──► DB        ─                  load_nodes()
  ↓                              .                          ↓
finalize_*                       .                  update_status:
update_node_status(Success      .                    Running → Hydrated
  | Failed | Blocked)            .                          ↓
                                  .                  set_messages(messages_json)
                                  .                  plan_state.set(current_plan)
                                  .                  mark_activated(active_skills)
```

---

## §8 与其他文档的接口

- **`save_runtime_checkpoint` 内部如何写 SQLite 表** → [04-memory.md §3](./04-memory.md#§3-runtimestatestore)
- **`run_compact` 何时触发 pre-compact 检查点** → [05-compression.md §6](./05-compression.md#§6-触发点)
- **`current_plan` 作为压缩保护的角色** → [05-compression.md §4](./05-compression.md#§4-protected_markers-机制)
- **既有 API 参考**（`AgentConfig::enable_task`、`TaskExecutor::execute_all` 等）→ `echo-agent/docs/{en,zh}/24-task-graph.md`、`echo-agent/docs/{en,zh}/25-orchestration.md`
