# 04 · 记忆系统

> **归属**：横跨框架（`echo-core` traits + `echo-state` impls）与产品（`UnifiedMemory` + `InstructionProvider` + `auto_memory`）。
> **接口**：`ReactAgent.memory: MemorySubsystem` 持有所有 store；产品层 `AgentRuntime.unified_memory` 是面向 GUI/CLI 的入口；二者**不直接共享**写入路径（详见 §7）。

本文剖析 Echo Agent 的记忆系统：三层架构的实际形态、内置记忆工具、`SnapshotManager` 与 `RuntimeStateStore` 的本质区别、`UnifiedMemory` 与运行时工具之间存在的命名空间错位、`InstructionProvider` 三层 Markdown 加载、`auto_memory` vs `BackgroundReviewer`、以及"分层记忆"在当前代码中的实际形态。

---

## §1 三层架构总览

记忆系统按生命周期切成三层，正交：

| 层 | 接口 | 概念 | 持久化 | 主要使用者 |
|----|------|------|-------|-----------|
| 长期知识 | `Store` | 跨 conversation 的 KV + 关键词搜索 | `FileStore` / `SqliteStore` | LLM 通过 `remember`/`recall` 工具 |
| 运行时状态 | `RuntimeStateStore` | 单一对话的完整运行时快照 + DAG | `SqliteRuntimeStateStore` | `run_core_loop` 自动触发 |
| 历史投影 | `ConversationStore` | 用户可见的 transcript（一条 `StoredMessage` 一行） | `SqliteConversationStore` | GUI/TUI 历史面板 |

每个 conversation 都可能同时关联到 3 个 store（也可一个不挂）。它们通过 `conversation_id` 关联，但表结构、写入触发、查询语义完全独立。

> 与 `echo-agent/docs/zh/03-memory.md` 的分工：那篇是 API 参考（trait 方法签名、用法示例）；本文是协作关系剖析（写入触发条件、命名空间约定、与压缩 / SnapshotManager 的边界）。

---

## §2 `Store` —— 长期 KV 与命名空间

```rust,ignore
// echo-agent/echo-core/src/memory/store.rs:182
pub trait Store: Send + Sync {
    fn put<'a>(...);                    // L184  upsert
    fn get<'a>(...);                    // L192
    fn search<'a>(...);                 // L199  默认实现：keyword
    fn search_with<'a>(...);            // L209  统一入口；默认实现仅支持 Keyword
    fn delete<'a>(...);                 // L230
    fn list_namespaces<'a>(...);        // L233
    fn list<'a>(...);                   // L239
    fn prune_expired<'a>(...);          // L244  默认 no-op
    fn dedup_by_content<'a>(...);       // L253  默认 no-op
}
```

### §2.1 SearchMode

```rust,ignore
// echo-core/src/memory/store.rs:97
pub enum SearchMode {
    Keyword,
    Semantic,
    Hybrid { vector_weight: f32 },
}
const RRF_K: f64 = 60.0;
```

`Hybrid` 用 Reciprocal Rank Fusion（RRF）融合 keyword + semantic 排名（`store.rs:120` `rrf_score()`）。**默认 trait 仅支持 Keyword**：要 Semantic / Hybrid 必须用 `SqliteStore::with_embedder(...)`（`echo-state/src/memory/sqlite_store.rs:73`）。

### §2.2 命名空间隔离

每个 store 方法都接受 `namespace: &[&str]`。内存型实现 (`InMemoryStore` / `FileStore`) 用 `namespace.join("/")` 作为 HashMap key。

**框架内的命名空间约定**：

| 用途 | 命名空间字符串 | 在哪里 |
|------|---------------|--------|
| Agent 自动启用的记忆工具 | `[agent_name, "memories"]` | `src/agent/react/mod.rs:517` |
| Skill 遥测 | `["agent", "skill_telemetry"]` | `echo-state/src/skill_telemetry.rs:170` |
| `UnifiedMemory.memories`（产品） | `["agent", "memories"]` (固定字面量) | `echo-agent-cli/.../unified_memory.rs:198, 211, 235, 246` |
| 任务 store | `["tasks"]` | `echo-orchestration/src/tasks/store.rs:48` |

⚠️ **Note**：`UnifiedMemory` 用的是字面 `"agent"`，**不是** agent 名 —— 这与运行时记忆工具的 `[agent_name, "memories"]` **不匹配**。详见 §7.3。

### §2.3 实现路径

| 类型 | 文件 | 特点 |
|------|------|------|
| `InMemoryStore` | `echo-state/src/memory/store.rs:20` | 进程内 `HashMap`；search 简单 keyword 评分（`store.rs:516-530`），关键词数 / 总词数 |
| `FileStore` | `echo-state/src/memory/store.rs:208` | 单文件 JSON，atomic tmp + fsync + rename（`store.rs:242-266`） |
| `SqliteStore` | `echo-state/src/memory/sqlite_store.rs:60` | 三张表 `store_items` / `store_fts` (FTS5) / `store_vectors`；可选 `with_embedder` 启用 semantic |

---

## §3 `RuntimeStateStore`

详细介绍见 [02-task-planning.md §2.1](./02-task-planning.md#§21-runtimestatestoreagentcheckpoint--react-循环的运行时状态)，此处仅总结要点：

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

- 一个 conversation 一份 `AgentCheckpoint`（`SqliteRuntimeStateStore` 中是 PK）。
- TaskNode DAG 单独表 `task_nodes`，PK = `(id, conversation_id)`。
- 实现：`SqliteRuntimeStateStore`（`src/state/sqlite.rs:12`），`feature = "sqlite"`。
- 触发点全表见 [02-task-planning.md §5](./02-task-planning.md#§5-runtimestatestore-检查点触发条件表)。

`MemorySubsystem.state_store: Option<Arc<dyn RuntimeStateStore>>`（`subsystems/memory.rs:14-20`）—— 关键持有点。

---

## §4 `ConversationStore` —— 用户可见的 transcript

```rust,ignore
// echo-agent/echo-core/src/memory/conversation.rs:98
pub trait ConversationStore: Send + Sync {
    fn create_conversation(...);
    fn get_conversation(...);
    fn list_conversations(...);
    fn update_conversation(...);
    fn delete_conversation(...);
    fn save_messages(...);          // delete + insert upsert
    fn get_messages(...);
    fn count_messages(...);
    fn ensure_conversation(...);    // 默认实现 = get-or-create (L146)
    fn search_conversations(...);   // 默认实现 = naive scan (L164)
}
```

`StoredMessage` (`conversation.rs:65`) 字段：`id`、`conversation_id`、`role`、`content`、`attachments_json`、`tool_calls_json`、`tool_result_json`、`created_at`。

实现：`SqliteConversationStore`（`echo-state/src/memory/sqlite_conversation.rs:20`）。

### §4.1 写入触发

`AgentRunSnapshot::save_transcript_projection` (`echo-agent/src/agent/snapshot.rs:344-410`)。调用点：

| 位置 | 文件:行 |
|------|---------|
| 文本分支 final_answer | `phases/finalize.rs:156` |
| 工具分支 final_answer | `phases/finalize.rs:74` (cancellation path) |
| Max iterations | `phases/finalize.rs:241` |

每次写入是 delete + insert 全量替换（语义同 `save_messages` 默认实现）。

### §4.2 `is_internal_transcript_message` 过滤规则

并不是消息流里所有东西都该让用户看到。`save_transcript_projection` 在投影前用一个过滤器排除"框架内部消息"：

```rust,ignore
// echo-agent/src/agent/snapshot.rs:20
fn is_internal_transcript_message(message: &Message) -> bool {
    let trimmed = message.content_str().trim_start();
    match message.role {
        Role::System => true,                                    // 全部排除
        Role::User => trimmed.starts_with("[Relevant historical memories]")
            || trimmed.starts_with("[The above memories")
            || trimmed.starts_with("[Verifier feedback]")
            || trimmed.starts_with("[Hook:")
            || trimmed.starts_with("[Memory")
            || trimmed.starts_with("[Context")
            || trimmed.starts_with("[Compact")
            || trimmed.starts_with("[Compression"),
        Role::Tool => trimmed.contains("[placeholder]")
            || trimmed.contains("[synthetic]")
            || trimmed.contains("placeholder result"),
        Role::Assistant | Role::Custom(_) => false,
    }
}
```

效果：
- 所有 system 消息 → 不显示
- 框架伪装成 user role 推入的"系统通知"（memory 召回头、verifier 反馈、hook 输出、压缩通知）→ 不显示
- tool-pair 修复时塞入的合成 placeholder → 不显示
- 真实的 user / assistant 内容 → 保留

这条规则是**写**时用（决定哪些消息进 `ConversationStore`），不是读时用 —— `RuntimeStateStore` 持有的 `messages_json` 仍包含所有消息，这是为了崩溃恢复要还原"完整运行时状态"，而 `ConversationStore` 只服务用户面。

---

## §5 内置 4 个记忆工具

`enable_memory=true` 或显式调 `with_memory_tools(store)` 时会注册：

| 工具名 | 实现 | 文件:行 | 行为 |
|--------|------|---------|------|
| `remember` | `RememberTool` | `src/tools/builtin/memory.rs:25` | 生成 UUID v4 key，写入 `{content, importance, tags}` |
| `recall` | `RecallTool` | `memory.rs:134` | `store.search(ns, query, limit)` 关键词搜索 |
| `search_memory` | `SearchMemoryTool` | `memory.rs:310` | `store.search_with(ns, SearchQuery::hybrid(...))` 兜底回退 keyword |
| `forget` | `ForgetTool` | `memory.rs:226` | 按 ID 前缀解析 + 精确 delete |

> ⚠️ **没有 `list_memories` 工具**。`UnifiedMemory::list_memories()` 是产品层的 async 方法，但运行时工具集里**不**暴露 `list_memories`。

`with_memory_tools` builder 方法（`src/agent/react/builder.rs:520-524`）：

```rust,ignore
pub fn with_memory_tools(mut self, store: Arc<dyn Store>) -> Self {
    self.store = Some(store);
    self.enable_memory = true;
    self
}
```

实际注册发生在 `set_memory_store` (`src/agent/react/mod.rs:746-760`)，namespace = `[agent_name, "memories"]`。

`enable_memory=true` 但**没**显式调 `with_memory_tools` 时，`setup_memory_store`（`react/mod.rs:505-535`）会自动开一个 `FileStore::new(&config.memory_path)`（默认 `~/.echo-agent/store.json`），同样注册 4 个工具。

> **EKO 应用层覆盖**：`infra::create_agent` 用 `builder.store(...)` 注入了项目级 FileStore（`{workspace.root}/.eko/memory/store.json`），覆盖了上面的框架默认全局路径。动态记忆按 workspace 物理隔离，workspace 切换时热重载 Store + MemoryLayerManager（见 `infra.rs` 的 `resolve_memory_store_paths` / `create_memory_store_for_workspace` 与 `state.rs` 的 `switch_workspace`/`exit_workspace`）。

---

## §6 `SnapshotManager` —— 内存回滚 ≠ 持久化

```rust,ignore
// echo-agent/echo-state/src/memory/snapshot.rs:24
pub struct StateSnapshot {
    pub id:         String,             // UUID v4
    pub iteration:  usize,
    pub messages:   Vec<Message>,
    pub metadata:   HashMap<String, String>,
    pub created_at: u64,
}

// snapshot.rs:41
pub enum SnapshotPolicy {
    EveryIteration,    // 默认
    EveryN(usize),
    Manual,
}

// snapshot.rs:56
pub struct SnapshotManager {
    policy:        SnapshotPolicy,
    snapshots:     Vec<StateSnapshot>,
    max_snapshots: usize,
}
```

方法：`should_capture(iteration)` (`L81`)、`capture(...)` (`L90`)、`rollback(steps_back)` (`L124`)、`rollback_to(id)` (`L135`)、`latest`、`list`、`clear`。

环形 buffer：`len > max_snapshots` 时清掉最早的几条（`snapshot.rs:113-115`）。

### §6.1 与 RuntimeStateStore 的本质区别

| 属性 | `SnapshotManager` | `RuntimeStateStore` |
|------|------------------|--------------------|
| 持久化 | **否**（仅内存） | 是（SQLite） |
| 用例 | run 内 rollback（撤回最近 N 步） | 跨进程恢复 |
| 数据 | `Vec<Message>` deep clone | `AgentCheckpoint` JSON |
| 容量 | 硬上限 cap eviction | 仅 latest 一份（PK 覆盖） |
| 触发 | `auto_snapshot` 按 policy | 多个固定生命周期点（详见 [02-task-planning.md §5](./02-task-planning.md#§5-runtimestatestore-检查点触发条件表)） |
| 进程崩溃后还在吗 | **否** | 是 |

二者没有任何耦合 —— 同一份消息流可同时被两个机制独立捕获。

### §6.2 `auto_snapshot` 触发点

```rust,ignore
// echo-agent/src/agent/snapshot.rs:677-695
pub(crate) async fn auto_snapshot(&self, iteration: usize) {
    let should_capture = self.snapshot_manager.read()
        .is_some_and(|m| m.should_capture(iteration));
    if should_capture { /* m.capture(...) */ }
}
```

调用点：
- `phases/tools.rs:236` —— 每轮工具执行后
- `phases/finalize.rs:140` —— FinalAnswer 之后

注册：`ReactAgentBuilder::snapshot_policy(...)` 走 `agent.set_snapshot_manager(SnapshotManager::new(policy, max_snapshots))`（`builder.rs:784-786`）。

---

## §7 `UnifiedMemory`（产品层）

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/unified_memory.rs:118
pub struct UnifiedMemory {
    instructions: InstructionProvider,                  // 静态 .md 文件加载器
    memories:     Option<Arc<dyn Store>>,               // 动态 KV
}
```

### §7.1 公开 API

```rust,ignore
pub fn load() -> Self;                                              // L127
pub fn with_store(mut self, store: Arc<dyn Store>) -> Self;         // L135
pub fn get_instructions(&self, tier: InstructionTier) -> Option<&str>;  // L143
pub fn set_instructions(&self, tier: InstructionTier, content: &str)
                                          -> Result<(), String>;    // L152
pub fn system_prompt_context(&self) -> MemoryContext;               // L269

pub async fn remember(&self, content: &str, importance: f32)
                                  -> Result<String, String>;        // L187
pub async fn recall(&self, query: &str)
                                  -> Result<Vec<MemoryEntry>, String>;     // L208
pub async fn forget(&self, key: &str) -> Result<bool, String>;      // L232
pub async fn list_memories(&self) -> Result<Vec<MemoryEntry>, String>;     // L243
```

`InstructionTier`（`unified_memory.rs:37`）：`User | Project | Local`，对应 `~/.echo-agent/user.md` / `<project_root>/.echo-agent/project.md` / `<cwd>/.echo-agent/local.md`。

`MemoryContext`（`unified_memory.rs:75-113`）：`{instructions: String, memories: Vec<String>}`，`to_prompt_suffix()` 输出两节 `## ...` 格式 system prompt 后缀。

### §7.2 消费点

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/runtime.rs:32
pub struct AgentRuntime {
    pub unified_memory: crate::unified_memory::UnifiedMemory,
    // ...
}
// runtime.rs:91
let unified_memory = UnifiedMemory::load();         // ← 注意：没有 .with_store(...)
```

### §7.3 ⚠️ 两个已知陷阱

#### #1 `AgentRuntime::new` 不挂 store

`runtime.rs:91` 的 `UnifiedMemory::load()` 只加载 `.md` 文件，**没**有调 `.with_store(...)`。结果：

```rust,ignore
// unified_memory.rs:187
pub async fn remember(&self, content: &str, importance: f32) -> Result<String, String> {
    let store = self.memories.as_ref()
        .ok_or_else(|| "No memory store configured".to_string())?;
    // ...
}
```

产品代码若想通过 `AgentRuntime.unified_memory.remember(...)` 写记忆，**永远拿到 `Err("No memory store configured")`**，除非外部代码再手动挂一次 `with_store(...)` 或重新构造。测试代码 (`tests/runtime_state_e2e.rs:148`) 用了 `UnifiedMemory::load().with_store(...)`，但 production bootstrap 没这么做。

#### #2 命名空间字面 `"agent"` ≠ agent 名

```rust,ignore
// unified_memory.rs:198, 211, 235, 246
let ns = vec!["agent".to_string(), "memories".to_string()];
```

但运行时记忆工具用的是 `[agent_name, "memories"]`（`react/mod.rs:517`，`agent_name` 是配置传入的 `AgentConfig::name`）。**两者写入读取的是不同的 namespace 桶**，除非 `agent_name` 恰好就是 `"agent"`。

实际后果：通过 `remember` 工具写的记忆，从产品层 `unified_memory.list_memories()` 读不到；反之亦然。

两项都记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 3、4 项。

---

## §8 `InstructionProvider` —— 三层 Markdown 加载

`UnifiedMemory.instructions` 字段的类型，原名 `ProjectMemory`，已重命名为 `InstructionProvider`（详见 git log）。

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/instruction_provider.rs:16
pub struct InstructionProvider {
    pub project_level: Option<String>,
    pub user_level:    Option<String>,
    pub local_level:   Option<String>,
}
```

加载路径：

| Tier | 路径 | 加载逻辑 |
|------|------|---------|
| `User` | `~/.echo-agent/user.md` | `instruction_provider.rs:68-73` |
| `Project` | `<project_root>/.eko/project.md` | 找 root：从 `current_dir` 往上找 `.git` 或 `.eko` 父目录（`L85-94`） |
| `Local` | `<cwd>/.eko/local.md` | 直接看 `current_dir`（`L76-82`） |

`get_system_prompt_suffix()` 输出顺序：User → Project → Local，每段拼 `## *-level instructions\n{...}`，整体前缀 `\n\n`（如非空）。

### §8.1 ⚠️ Save/Load 路径不对称

加载 project tier 时找 project root（往上爬）；保存 project tier (`save_project_instructions`) 时却写到 `<cwd>/.eko/project.md` —— 即如果用户在子目录跑，加载/保存指向不同文件。这是当前实现细节，使用时若依赖 cwd 与 project root 重合，是隐性约定。

---

## §9 `auto_memory` vs `BackgroundReviewer`

两套**post-run review** 机制。前者无 LLM、轻量启发式；后者基于 LLM、能力强但成本高。

### §9.1 `auto_memory`（产品层、启发式）

`echo-agent-cli/echo-agent-app-core/src/auto_memory/mod.rs`，单文件。

```rust,ignore
// auto_memory/mod.rs:14-39
pub struct Observation {
    pub category:    ObservationCategory,
    pub text:        String,
    pub confidence:  f64,
    pub source_turn: Option<usize>,
}
pub enum ObservationCategory {
    Project | User | Bug | Decision | FilePath
}
```

`extract_observations(messages, config)` (`auto_memory/mod.rs:89-254`)：纯关键词匹配 + 启发式规则。例：

- User role + "always "/"never "/"prefer "/"i want " → `User`，confidence 0.8
- Assistant role + "this project uses" / "project pattern" → `Project`，0.75
- Assistant + "the bug was" / "fixed the issue" / "root cause" → `Bug`，0.85
- Assistant + "we decided to" / "chosen approach" → `Decision`，0.8
- 任何 role：含 `/` + 长度 > 5 + 后缀 `.rs/.ts/.py/...` → `FilePath`，0.6（默认 min_confidence=0.7 过滤掉）

`deduplicate_observations` (`mod.rs:262-282`) 按 category + 文本长度倒序排，丢弃"短的（≤20 字符）是别人前缀"的项。

`append_to_project_memory(observations)` (`mod.rs:326-364`)：写到 `<project_root>/.eko/project.md`。如果文件已含 `## Auto-extracted observations`，从该 marker 开始截断后追加新版（语义是**替换**该段而非累加）。

#### 触发点

- REPL 退出 hook：`echo-agent-cli/src/cli/repl.rs:189-258` 的 `run_auto_memory_on_exit(agent)`；受 `AUTO_MEMORY_ENABLED` atomic flag 控制（`L201`）。
- 显式 slash command：`echo-agent-cli/src/cli/cmd_impls/all.rs:306-342` 调用 `run_auto_memory_extraction()`（便捷封装）。

### §9.2 `BackgroundReviewer`（框架、LLM）

```rust,ignore
// echo-agent/src/improve/background_review.rs:111-116
pub struct BackgroundReviewer {
    config:        BackgroundReviewConfig,
    llm_client:    Arc<dyn LlmClient>,
    memory_store:  Option<Arc<dyn Store>>,
    run_store:     Option<Arc<dyn RunStore>>,
}
```

`review(&self, run: &Run) -> JoinHandle<ReviewOutcome>` (`L192-221`)：异步 spawn 一个 sub-agent，`build_transcript()` 拼装对话稿后挂 `MEMORY_REVIEW_PROMPT|SKILL_REVIEW_PROMPT|COMBINED_REVIEW_PROMPT`，构造受限工具集（仅记忆工具）的子 agent 来"审稿"。

调用点：仅 CLI `evolution` 命令（`echo-agent-cli/src/cli/cmd_impls/evolution.rs:136`），**没**有自动 post-run hook。也就是说：要触发 LLM 级别的对话审查，用户必须显式跑 `/review` 或 `POST /api/evolution/review`。

### §9.3 选谁

| 场景 | 用 |
|------|---|
| 每次对话结束默默把"用户偏好 / 项目模式"提取出来 | `auto_memory`（轻量、零 LLM 成本） |
| 用户显式发"帮我从这次对话里学一些经验" | `BackgroundReviewer`（LLM 总结、写入记忆） |
| 大批量历史对话的回顾分析 | `BackgroundReviewer` |

二者写的也不是同一个地方：`auto_memory` 写 `.md` 文件，`BackgroundReviewer` 写 `Store`。

---

## §10 关于"分层记忆"——`TieredMemory` 不存在的实情

⚠️ **当前代码中没有 `TieredMemory`**。全 workspace `grep "TieredMemory|tiered_memory|MemoryTier"` 零命中。

历史上 `echo-agent/docs/{en,zh}/39-tiered-memory.md` 描述过 4 层（Working / ShortTerm / LongTerm / Core）+ 重要度淘汰。该文档已删除（参见 git log）。当前等价的"分层"由几件独立机制组合实现：

| 你想要的 | 实际由什么承担 |
|----------|--------------|
| Working layer（当前活跃消息） | `ContextManager.messages` —— 见 [05-compression.md §1](./05-compression.md#§1-contextmanager-的职责与字段) |
| ShortTerm（最近结构化条目） | 没有专门层；通过 `Store.search` + 时间排序实现 |
| LongTerm（持久检索） | `Store`（本文 §2） |
| Core（永久注入到系统提示） | 注入靠 `InstructionProvider` (.md 文件) + `system_prompt_context()` |
| 重要度淘汰 | 部分由 `Store::prune_expired` 自定义；`StoreItem.importance: f32` 字段（默认 5.0）目前**仅作 UI 排序用**，未触发自动驱逐 |

如果产品层日后想要"基于重要度自动迁移"，需要新写一个组件来组合上面这些 —— 当前没有。

---

## §11 `MemoryScope` —— 是 tag，不是缓存层

```rust,ignore
// echo-agent/echo-core/src/memory/scope.rs:17
pub enum MemoryScope {
    User, Project, Repo, Task, Session, Run
}
impl MemoryScope {
    pub fn priority(&self) -> u8 { /* 0..5 */ }     // L46-55
    pub fn is_persistent(&self) -> bool {           // L58-60
        matches!(self, User | Project | Repo)
    }
}
```

它**不是**缓存层，是元数据 tag —— 描述某条记忆的"作用域"（用户级 / 项目级 / 仓库级 / 任务级 / 会话级 / 运行级）。priority 排序辅助检索时排错优先级；`is_persistent()` 可被存储后端用来决定要不要持久化。

**它不会单独形成一层 Store**。一个 `Store` 实例可以容纳任何 scope 的记忆，scope 只是 `StoreItem` 的一个属性维度。

---

## §12 与其他文档的接口

- **`current_plan` / `active_skills` 进入 AgentCheckpoint** → 本文 §3
- **压缩流程怎么和 `ContextManager` / `MemoryPromoter` 互动** → [05-compression.md §5](./05-compression.md#§5-完整压缩流程)
- **保护 marker 与 SkillRegistry 注入** → [06-skills.md §4](./06-skills.md#§4-两条-skill-激活路径)
- **`save_runtime_checkpoint` / `save_transcript_projection` 触发点** → [02-task-planning.md §5](./02-task-planning.md#§5-runtimestatestore-检查点触发条件表)
- **既有 API 参考** → `echo-agent/docs/{en,zh}/03-memory.md`（已与本文 §1–§4 同步）
