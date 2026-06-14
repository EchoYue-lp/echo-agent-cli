# 03 · Agent 拆分 / SubAgent / AgentPool

> **归属**：横跨框架（`AgentRole` / SubAgent system）与产品（`AgentPool`）。
> **接口**：SubAgent 由 `agent_tool` 工具调起，依赖 `enable_subagent=true`；产品层 `AgentPool` 是 ReactAgent 的复用池，对核心循环透明。

本文剖析"一个 ReactAgent 不够用了怎么办"的两个维度：**单 agent 内部的角色拆分**（`AgentRole` —— 当前生效范围有限）和**多 agent 实例的并行分工**（SubAgent 三模式 + `AgentPool`）。

---

## §1 ⚠️ `AgentRole` —— 当前仅在 `TaskExecutor` 生效

```rust,ignore
// echo-agent/src/agent/config.rs:31
#[derive(Default, Debug, Clone, PartialEq)]
pub enum AgentRole {
    Orchestrator,        // 任务编排者，把工作分发给 SubAgent，自己不直接干活
    #[default]
    Worker,              // 工人，直接执行任务
}
```

`AgentConfig::role` 字段（`config.rs:52`）+ setter `role(self, role)`（`config.rs:258`）。

**这个字段当前只在 `TaskExecutor` 内部分支**，与 `ReactAgent::run_core_loop` 无关：

```rust,ignore
// echo-agent/src/agent/react/planning.rs:236
fn build_execute_fn(&self) -> TaskExecuteFn {
    let is_orchestrator = self.config.role == AgentRole::Orchestrator;
    let subagent_names: Vec<String> = if is_orchestrator {
        // 抓 SubAgent 名单提供给 TaskExecutor
        self.tools.subagent_registry.agents_map().try_read()...
    } else {
        Vec::new()
    };
    // ...
}
```

`AgentConfig` 自己的注释（`config.rs:25-29`）也明确写：

> "This role field currently **only** affects behavior in the TaskExecutor's execution logic. It has no additional effect in other modules (ReactAgent, PlanExecute, etc.)."

也就是说：**给一个跑 ReAct 循环的 agent 设 `role(AgentRole::Orchestrator)` 不会改变它在主循环里的任何行为**。要让 Orchestrator 真正"只编排不干活"，需要走 `TaskExecutor`+`enable_subagent`+`agent_tool` 这条路（详见 §5）。

记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 6 项，待跟进。

---

## §2 SubAgent 三种执行模式

```rust,ignore
// echo-agent/src/agent/subagent/types.rs:13
pub enum ExecutionMode {
    Sync,        // 父 agent 阻塞等待
    Fork,        // 父 agent 继续；子 agent 浅继承上下文（系统提示 + 部分历史）
    Teammate,    // 长生命周期对等 agent，跨多个 turn 协作
}
```

**继承策略表** 由 `ContextInheritance::for_mode`（`echo-agent/src/agent/subagent/context.rs:72`）决定：

| 字段 | Sync 默认 | Fork 默认 | Teammate 默认 |
|------|---------|---------|------------|
| `inherit_system_prompt` | false | true | (Teammate 默认值) |
| `inherit_recent_messages` | (限定窗口) | (浅复制) | (按需) |
| `inherit_tools` | (按 filter) | (按 filter) | (独立) |

具体 trait 定义在 `context.rs:24` `ContextInheritance`，三个 default 工厂在 `sync_default()` / `fork_default()` / `teammate_default()`（`context.rs:80+`）。

无论哪种模式，子 agent 都拿到独立的 `ContextManager`，跟父 agent 的消息流物理隔离 —— 仅按继承策略**复制一份初始消息**，不共享指针。

---

## §3 `SubagentRegistry` —— lazy factory + 竞态保护

```rust,ignore
// echo-agent/src/agent/subagent/registry.rs:72
pub struct SubagentRegistry {
    agents:               AgentMap,                               // = Arc<RwLock<HashMap<String, Arc<dyn Agent>>>>
    definitions:          Arc<RwLock<HashMap<String, SubagentDefinition>>>,
    factories:            Arc<RwLock<HashMap<String, Arc<dyn AgentFactory>>>>,
    instantiating:        Arc<RwLock<HashSet<String>>>,           // 正在实例化的名字集合
    instantiating_done:   Arc<Notify>,                            // 单实例化完成后唤醒等待者
    event_bus:            SubagentEventBus,
}
```

懒实例化 + 竞态保护：当多个并发请求同时要拿同名 SubAgent 而它还没创建时，第一个进入 `get_or_instantiate`（`registry.rs:270+`）的请求会把名字插入 `instantiating` HashSet 并开始构造；其他请求看到该名字在 set 中就 `Notify::notified().await`，等第一个构造完后唤醒并取得 `agents` 中刚插入的实例。

避免的反例是"两个请求各自构造一份 SubAgent，导致重复初始化（昂贵的 LLM client、tool manager 等）"。

---

## §4 `IsolatedSubAgentConfig` —— 隔离运行的预算

```rust,ignore
// echo-agent/src/agent/subagent/isolated.rs:12
pub struct IsolatedSubAgentConfig {
    pub system_prompt:   String,
    pub max_iterations:  usize,    // 默认 5
    pub token_budget:    usize,    // 默认 16_000
    pub tool_call_limit: usize,    // 默认 20
    pub timeout_secs:    u64,      // 默认 120
}
```

`run_isolated`（`isolated.rs:47+`）在每次调用时**全新**构造一个 `ReactAgent`（用 `ReactAgentBuilder`），跑完任务后整个 agent 被 drop —— 真正意义上的"用完即弃"，对父 agent 的状态零污染。

适用场景：长时上下文已经膨胀的父 agent 想把"清晰、有边界的子任务"扔给一个干净环境处理（典型例子：跑一段对成本敏感的代码生成 + 验证）。

---

## §5 `agent_tool` —— SubAgent 的入口工具

```rust,ignore
// echo-agent/src/tools/builtin/agent_dispatch.rs:50
pub struct AgentDispatchTool {
    executor:                 Arc<SubagentExecutor>,
    parent_agent:             String,
    cancel:                   CancellationToken,
    parent_context_factory:   Option<Arc<ParentContextFactory>>,
}
```

工具名 `"agent_tool"`（`agent_dispatch.rs:83`）。Description（`agent_dispatch.rs:87`）明确告诉 LLM：**"Dispatch a task to a specialized SubAgent for execution. As the orchestrator, prefer using this tool to delegate computation, data fetching, etc. to professional SubAgents rather than answering directly."**

```rust,ignore
// echo-agent/src/tools/builtin/agent_dispatch.rs:17
pub struct ParentContextFactory {
    pub system_prompt:    Arc<String>,
    pub tool_manager:     Arc<ToolManager>,
    pub context:          Arc<Mutex<ContextManager>>,
    pub store:            Option<Arc<dyn Store>>,
}
```

`ParentContextFactory::build(mode)`（`agent_dispatch.rs:30`）在 dispatch 时按 `ExecutionMode` 抓取一份父 agent 的关键状态（最近消息 + 工具定义，但**剔除 `final_answer` 工具**），用 `SubagentContext::from_parent` 加上 `ContextInheritance::for_mode` 构造子 agent 的初始上下文。

`enable_subagent=true` 时，框架在 `react/mod.rs:378-395`（`cfg "subagent"`）注册：

```rust,ignore
let parent_factory = Arc::new(ParentContextFactory { /* 抓取 self 的指针 */ });
let dispatch_tool = AgentDispatchTool::new(executor, parent_name, cancel)
    .with_parent_context(parent_factory);
tool_manager.register(Box::new(dispatch_tool));
```

---

## §6 隔离保证矩阵

| 隔离维度 | 保证方式 | 文件:行 |
|---------|---------|---------|
| Context（消息历史） | 每个 SubAgent 是独立的 `ReactAgent` Rust 对象，`ContextManager` 互不引用 | `agent/subagent/context.rs` |
| 工具集 | `inherit_tools` filter 决定哪些父工具暴露给子；子 agent 也可注册私有工具 | `context.rs:147` |
| 长期记忆 | 默认 namespace = `[agent_name, "memories"]`，名字不同→不同 namespace | `react/mod.rs:517` |
| Runtime checkpoint | 子 agent 的 `conversation_id` 由父调起时传入；与父用同一 `RuntimeStateStore` 不同 key | `agent_dispatch.rs` 调度路径 |
| 资源预算（isolated mode） | `IsolatedSubAgentConfig` 强制 `max_iterations`/`token_budget`/`tool_call_limit`/`timeout_secs` | `isolated.rs:12` |
| 并发执行 | 每个 SubAgent 自己的 `execution_mutex`，互不阻塞 | `react/mod.rs:176` |

---

## §7 `AgentPool`（产品层）

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/agent_pool.rs:174
pub struct AgentPool {
    shared:             SharedResources,
    agents:             RwLock<HashMap<String, PooledAgent>>,
    config:             PoolConfig,
    app_config:         AppConfig,
    skill_descriptors:  Vec<SkillDescriptor>,
    cleanup_cancel:     CancellationToken,
}
```

### §7.1 `PoolConfig` 与 `SharedResources`

```rust,ignore
// agent_pool.rs:48
pub struct PoolConfig {
    pub max_agents:               usize,        // 默认 10
    pub idle_timeout:             Duration,     // 默认 1800s = 30min
    pub enable_background_agent:  bool,         // 默认 true
}

// agent_pool.rs:93
pub struct SharedResources {
    pub llm_client:               Arc<dyn LlmClient>,
    pub tool_manager:             Arc<ToolManager>,
    pub hook_registry:            Arc<RwLock<HookRegistry>>,
    pub sandbox_manager:          Option<Arc<SandboxManager>>,
    pub store:                    Option<Arc<dyn Store>>,
    pub conversation_store:       Option<Arc<dyn ConversationStore>>,
    pub run_store:                Option<Arc<dyn RunStore>>,
    pub token_tracker:            Arc<TokenUsageTracker>,
    pub permission_service:       Option<Arc<PermissionService>>,    // cfg
    pub state_store:              Option<Arc<dyn RuntimeStateStore>>,
    pub tool_execution_pipeline:  Option<Arc<ToolExecutionPipeline>>,
}
```

**Pool 的核心价值**：所有 agent 实例共享 `SharedResources`，每个 `Arc::clone` 仅引用计数+1。**唯一不共享**的是各 agent 自己的 `ContextManager`、`execution_mutex`、消息历史 —— 这是真正能并行的部分。

`SharedResources::extract_from(handle)`（`agent_pool.rs:114-146`）从一个已存在的 `AgentHandle` 抓取这些 Arc，作为后续创建池中 agent 的模板。

### §7.2 `acquire` 与 `try_lock` idle 探测

```rust,ignore
// agent_pool.rs:234
pub async fn acquire(&self, conversation_id: &str) -> Result<AgentHandle, PoolError> {
    // (1) 已存在该 conversation_id 的 agent → bump last_used 返回
    // (2) 否则需要新建：先看池容量
    let active_count = agents.iter()
        .filter(|k| k.as_str() != "__background__")    // 排除 __background__
        .count();
    if active_count >= self.config.max_agents {
        // (3) 容量满，找 idle 的驱逐
        let mut candidates: Vec<_> = agents.iter()
            .filter(|(id, _)| id.as_str() != "__background__")
            .collect();
        candidates.sort_by_key(|(_, pa)| pa.last_used);
        for (id, pa) in candidates {
            // 关键：try_lock execution_mutex 探测是否在跑
            let evictable = pa.handle.read(|a| {
                a.execution_mutex().try_lock().is_ok()
            });
            if evictable {
                agents.remove(&id);
                break;       // 驱逐第一个 idle 的
            }
        }
        if agents.len() >= max_agents { return Err(PoolError::PoolFull); }
    }
    // (4) 新建 agent 并插入
}
```

> **为什么用 `try_lock` 而不是手动维护"忙/闲"标志？** 因为 `execution_mutex` 已经是核心循环的串行化机制 —— 它的状态就是 ground truth。任何额外标志都可能与之偏离。`try_lock` 失败 = "此刻不安全驱逐"，自然且零误报。

### §7.3 `__background__` Agent

```rust,ignore
// agent_pool.rs:207-213
match pool.create_agent("__background__").await {
    Ok(handle) => {
        agents.insert(
            "__background__".to_string(),
            PooledAgent::new(handle, "__background__".to_string()),
        );
    }
    // ...
}

// agent_pool.rs:331
pub fn get_background_agent(&self) -> Option<AgentHandle> {
    let agents = self.agents.read();
    agents.get("__background__").map(|pa| pa.handle.clone())
}
```

行为对比表：

| 特性 | 普通池 agent | `__background__` agent |
|------|------------|----------------------|
| `acquire` 容量计数 | 计入 | **不计入**（`agent_pool.rs:246`） |
| 驱逐候选 | 是 | **永不**（`agent_pool.rs:252`） |
| `cleanup_monitor` 周期清理 | 是（idle > timeout 即驱逐） | **永不**（`agent_pool.rs:363`） |
| 访问入口 | `pool.acquire(conv_id)` | `pool.get_background_agent()` |

用例：`spawn_background_task` 工具创建的后台任务（详见 [02-task-planning.md §3](./02-task-planning.md#§3-任务工具集-enable_task-true-注册)）必须在用户对话之外有一个稳定的执行 agent，不能因为对话切换或池满被赶走。`__background__` 就是这个保留位。

### §7.4 `cleanup_monitor`

```rust,ignore
// agent_pool.rs:344
pub async fn spawn_cleanup_monitor(self: &Arc<Self>) {
    // 每分钟扫一次，evict 满足 last_used.elapsed() > idle_timeout 的非 __background__ agent
}
```

通过 `cleanup_cancel: CancellationToken` 控制；`AgentPool::shutdown()` 调用 `cleanup_cancel.cancel()` 让监视器退出。

---

## §8 Capability flags 对照表

`AgentConfig` 的能力开关位（`echo-agent/src/agent/config.rs`），全部默认 `false`：

| Flag | 字段位 | 开启后注册 / 影响 |
|------|--------|----------------|
| `enable_tool` | L54 | (a) `register_feature_gated_tools` 调 `echo_tools::register_all_tools(tool_manager)`（`react/mod.rs:498-500`）；(b) 联合 `enable_cot` 注入 CoT 引导语 |
| `enable_task` | L56 | 注册 8 个任务工具（`react/mod.rs:352-367`，详见 [02-task-planning.md §3](./02-task-planning.md)）。要求 `feature = "tasks"` |
| `enable_human_in_loop` | L58 | 注册 `HumanInLoop` 工具（`react/mod.rs:347-349`，cfg `human-loop`） |
| `enable_subagent` | L60 | 构建 `ParentContextFactory` 并注册 `agent_tool`（`react/mod.rs:378-395`，cfg `subagent`） |
| `enable_cot` | L73 | 与 `enable_tool` 联合，把 `COT_INSTRUCTION`（"Before calling any tool, briefly describe your analysis and execution plan."）拼到 `system_prompt`（`react/mod.rs:475-483`） |
| `enable_memory` | L86 | 触发 `setup_memory_store`（`react/mod.rs:507`）：开 `FileStore` + 注册 `remember`/`recall`/`search_memory`/`forget` 工具（详见 [04-memory.md §2 / §5](./04-memory.md)） |

### §8.1 预设组合

```rust,ignore
// echo-agent/src/agent/config.rs (around L226-237)
impl AgentConfig {
    pub fn with_full_capabilities(mut self) -> Self {
        self.enable_tool = true;
        self.enable_memory = true;
        self.enable_task = true;
        self.enable_cot = true;
        self
    }
    pub fn with_dev_capabilities(mut self) -> Self {
        self.enable_tool = true;
        self.enable_cot = true;
        self
    }
}
```

> 注意：`enable_subagent` 和 `enable_human_in_loop` **没有**被任何预设打开 —— 这两项需要显式 opt-in，因为它们引入跨 agent / 用户介入的复杂性。

---

## §9 与其他文档的接口

- **`SubagentRegistry` 与 `SkillRegistry` 是不同概念** → [06-skills.md §1](./06-skills.md#§1-skill-trait--skillregistry)
- **`PoolConfig.idle_timeout` 与 `cleanup_monitor` 的关系** → 本文 §7.4
- **`enable_memory=true` 注册的工具具体行为** → [04-memory.md §5](./04-memory.md#§5-内置-4-个记忆工具)
- **既有 API 参考**（`AgentRole`、`SubAgent` builder、`enable_subagent` 等）→ `echo-agent/docs/{en,zh}/06-subagent.md`、`echo-agent/docs/{en,zh}/26-multi-agent.md`
