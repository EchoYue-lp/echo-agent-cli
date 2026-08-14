# AgentPool — 多 Agent 并行执行架构

> **状态**: ✅ 已实现（2026-06-10）
> **范围**: echo-agent-cli (产品层) + echo-agent (框架层 setter)

## 概述

AgentPool 使多个对话/任务能够**真正并行执行**，突破了之前单 Agent + `execution_mutex` 的串行瓶颈。

### 解决的问题

```
之前（串行）：
  对话 A (5 分钟) ──→ agent 执行中... ──→ 对话 B 排队等待 ❌
  后台任务         ──→ agent 执行中... ──→ 用户对话排队 ❌

之后（并行）：
  对话 A ──→ Agent A (独立 mutex) ──→ 并行执行 ✅
  对话 B ──→ Agent B (独立 mutex) ──→ 并行执行 ✅
  后台   ──→ Agent BG (独立 mutex) ──→ 不阻塞用户 ✅
```

## 架构设计

### 核心结构

```
AgentPool
├── SharedResources (Arc 共享 — 不可变/线程安全)
│   ├── LlmClient          — API 连接复用
│   ├── ToolManager         — 工具注册表共享
│   ├── HookRegistry        — 钩子配置共享
│   ├── SandboxManager      — 沙箱配置共享
│   ├── Store               — 长期记忆共享
│   ├── ConversationStore   — 对话持久化共享
│   ├── TokenUsageTracker   — token 用量聚合
│   ├── PermissionService   — 权限规则共享
│   └── RuntimeStateStore   — 运行时状态共享
│
└── agents: HashMap<String, PooledAgent>
    ├── "conv-001" → Agent (独立 ContextManager + execution_mutex)
    ├── "conv-002" → Agent (独立 ContextManager + execution_mutex)
    └── "__background__" → 专用后台 Agent
```

### 资源分类

| 资源 | 处理方式 | 原因 |
|------|---------|------|
| LlmClient | **Arc 共享** | trait 要求 Send+Sync，所有方法 &self |
| ToolManager | **Arc 共享** | DashMap + AtomicU64，天然线程安全 |
| HookRegistry | **Arc 共享** | 已有 Arc<RwLock> 包装 |
| SandboxManager | **Arc 共享** | 无状态配置 |
| Store / ConvStore | **Arc 共享** | 按 conversation_id 隔离数据 |
| TokenUsageTracker | **Arc 共享** | 原子计数器，聚合所有 agent 用量 |
| ContextManager | **每 Agent 独立** | 对话历史隔离 |
| execution_mutex | **每 Agent 独立** | 并行执行的核心 |
| cancel_token | **每 Agent 独立** | 独立取消控制 |
| SkillRegistry | **每 Agent 独立** | 不 Clone，但加载成本低（文件 I/O） |
| McpManager | **每 Agent 独立** | &mut self 连接模式 |

## API 使用

### 产品层初始化

```rust
// GUI 入口 (desktop.rs)
let mcp_config_path = resolve_mcp_config_path(None, &app_config);
let runtime = AgentRuntime::bootstrap(&app_config, params, mcp_config_path).await?;
let pool = runtime.init_pool(PoolConfig::default()).await;
pool.spawn_cleanup_monitor().await;

let mut state = runtime.into_app_state(conversation_store);
state.set_pool(pool);
```

### Tauri 命令路由

```rust
// chat.rs — 按 conversation_id 路由
let agent = state.connection.agent_for(&conversation_id).await;
agent.chat_stream(&message).await;
```

### ConnectionState API

```rust
// 按 conversation_id 获取 agent（自动创建/复用）
let agent = connection.agent_for("conv-123").await;

// 获取主 agent（绕过池）
let agent = connection.primary_agent();

// 检查池是否激活
if connection.has_pool() { ... }
```

## 配置

```rust
PoolConfig {
    max_agents: 10,                      // 最大并行 agent 数
    idle_timeout: Duration::from_secs(1800), // 30 分钟空闲回收
    enable_background_agent: true,       // 预创建后台专用 agent
}
```

### 空闲回收

`spawn_cleanup_monitor()` 启动后台任务：
- 每 **5 分钟**扫描一次
- 超过 **30 分钟**未使用的 agent 被移除
- `__background__` agent 永不回收

### 溢出策略

当 `agents.len() >= max_agents` 时：
1. 找到最久未使用的非后台 agent
2. 移除该 agent
3. 为新 conversation 创建 agent

## 前端适配

### TypeScript

```typescript
// useTauriChat.ts — 传入 conversation_id
const conversation_id = useConversationStore.getState().activeId;
await apiInvoke('send_chat_message', {
  message: text,
  conversation_id: conversation_id ?? undefined,
});
```

### 行为

| 场景 | conversation_id | 路由目标 |
|------|----------------|---------|
| 已有对话 | `"conv-uuid"` | 池中的专用 agent |
| 新对话 | `null/undefined` | 主 agent（回退） |
| 后台任务 | `"__background__"` | 专用后台 agent |

## 向后兼容

- **TUI**: 不使用 pool，`agent_for()` 回退到主 agent
- **Eval**: 不使用 pool，单 agent 模式
- **前端**: 不传 `conversation_id` 时回退到主 agent

## 文件清单

| 文件 | 角色 |
|------|------|
| `echo-agent-app-core/src/agent_pool.rs` | AgentPool 核心实现 |
| `echo-agent-app-core/src/state.rs` | ConnectionState + agent_for() |
| `echo-agent-app-core/src/runtime.rs` | init_pool() |
| `echo-agent/src/agent/react/mod.rs` | 框架层 setter 方法 |
| `echo-agent/src/agent/react/capabilities.rs` | 框架层访问器 |
| `src/tauri/commands/chat.rs` | GUI 路由 |
| `src/tauri/desktop.rs` | Pool 初始化 |
| `web-frontend/src/hooks/useTauriChat.ts` | 前端适配 |

## 测试覆盖

18 个单元测试，覆盖：

| 类别 | 测试 |
|------|------|
| 配置 | default, custom, error display |
| 生命周期 | acquire, reuse, different IDs, release |
| 容量 | eviction on overflow |
| 后台 agent | pre-creation, disabled, on-demand |
| 资源共享 | extraction, Arc sharing verification |
| 元数据 | PooledAgent timestamps |

## 未来优化方向

1. **MCP 连接共享** — 当前每个 agent 独立连接 MCP server，可以共享 `Arc<McpClient>` 句柄
2. **SkillRegistry 共享** — 使用已有的 `SharedRegistry = Arc<RwLock<SkillRegistry>>` 模式
3. **ToolExecutionPipeline 共享** — 需要框架层将该类型 public 化
4. **优雅关闭** — agent 移除前保存 checkpoint 到 RuntimeStateStore
5. **并发度监控** — 暴露 pool metrics（活跃数、等待队列、平均创建时间）
