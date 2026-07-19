# 07 · 跨切关注点

> **归属**：跨所有子系统。
> **接口**：本文不引入新概念，只**汇总**前 6 篇的横切线索（命名空间、生命周期 hook、已知陷阱清单、与既有文档的引用矩阵）。

读完前 6 篇后，回到这里查表 / 对照陷阱清单。

---

## §1 命名空间约定一览

整个代码库出现的 namespace（`Vec<&str>`）字符串数组：

| 命名空间 | 出现位置 | 用途 |
|---------|----------|------|
| `[agent_name, "memories"]` | `echo-agent/src/agent/react/mod.rs:517` | Agent 启动 `enable_memory=true` 自动注册的记忆工具，`agent_name` 来自 `AgentConfig::name` |
| `["agent", "memories"]` | `echo-agent-cli/echo-agent-app-core/src/unified_memory.rs:198, 211, 235, 246` | ⚠️ `UnifiedMemory.remember/recall/forget/list_memories` 写入命名空间。**字面 `"agent"`，与上一行的 `agent_name` 不一致**（详见 §3 陷阱 #3） |
| `["agent", "skill_telemetry"]` | `echo-agent/echo-state/src/skill_telemetry.rs:170, 186, 208` | Skill 遥测记录（⚠️ 当前无 runtime 写入端，详见 §3 陷阱 #5） |
| `["tasks"]` | `echo-agent/echo-orchestration/src/tasks/store.rs:48` | `SqliteTaskStore` |

> **观察**：`agent_name` vs `"agent"` 字面值的不一致是当前代码事实。任何想"在产品层和运行时之间共享同一个记忆桶"的需求都必须显式选择一个 namespace 然后两端都用它，不能默认它们已经联通。

`Store::list_namespaces` 可以用来 enumerate 当前数据库里有哪些 namespace（产品端 debug/迁移时有用）。

---

## §2 Lifecycle Hooks 概览

定义在 `echo-agent/echo-execution/src/skills/hooks.rs` 的 `HookEvent` 枚举。覆盖范围比文档常说的"PreToolUse + PostToolUse"宽得多。

### §2.1 完整事件表

| 事件 | 触发时机 | 典型用途 |
|------|---------|---------|
| `UserPromptSubmit` | 用户提交 prompt | 上下文注入 / 阻塞 |
| `PreToolUse` | 工具执行前 | 权限校验 / 输入修改 / 阻止 |
| `PostToolUse` | 工具执行成功后 | 输出审查 / 触发后续 |
| `PostToolUseFailure` | 工具执行失败后 | 错误反馈 |
| `PostToolBatch` | 并发批工具执行后 | 聚合结果 |
| `PermissionRequest` | 权限对话框出现 | 自动 approve/deny |
| `PermissionDenied` | 权限被拒 | retry 信号 |
| `SessionStart` | 会话开始或恢复 | 上下文注入 |
| `SessionEnd` | 会话终止 | 清理 |
| `Stop` | Agent 完成回复 | continue reason |
| `StopFailure` | Agent 遇不可恢复错误 | 告警 / 恢复 |
| `Notification` | Agent 需用户介入 | 权限快捷处理 |
| `PreCompact` | 上下文压缩前 | 上下文注入 |
| `PostCompact` | 上下文压缩后 | 上下文注入 |
| `ConfigChange` | 配置文件变更 | 阻塞 / reload |
| `InstructionsLoaded` | Skills/instructions 加载完 | 加载后校验 |
| `SubagentStart` | SubAgent 派遣前 | 上下文注入 |
| `SubagentStop` | SubAgent 完成 | 结果注入 |
| `TaskCreated` | 任务创建/调度 | 上下文注入 |
| `TaskCompleted` | 任务完成 | 结果注入 |

`hooks.rs:30-47` 是这张表的代码权威 doc 注释。

### §2.2 Hook 处理类型

| Type | 行为 |
|------|------|
| `command` | 执行 shell 命令；stdin 接收 JSON 上下文 |
| `prompt` | 注入 prompt 给 LLM |
| `permission` | 直接返回权限决定（allow/deny/ask） |
| `http` | POST 事件到 URL，解析响应 |
| `mcp_tool` | 调 MCP server 的 tool |

### §2.3 Hook 桥接器

```rust,ignore
// echo-agent/src/hooks_bridge.rs:35
pub struct TaskHookBridge { /* TaskCreated/Completed/Timeout/Cancelled → echo-core hooks */ }

// hooks_bridge.rs:103
pub struct SubagentHookBridge { /* SubagentStart/Stop/Cancelled → echo-core hooks */ }
```

为什么需要 bridge？`echo-orchestration::TaskExecutor` 与 `echo-agent::SubagentExecutor` 各自有内部 hook callback 接口；为了让它们的事件汇入统一的 `HookRegistry`（被 SKILL.md frontmatter 的 `hooks:` 声明消费），引入 bridge 把"内部回调"映射到 `HookEvent` 上。详细描述见 `hooks_bridge.rs:1-30` 的模块 doc。

### §2.4 触发点汇总

| Hook | 主要触发位置 | 备注 |
|------|------------|------|
| `UserPromptSubmit` | `phases/prepare.rs` 内 prepare_turn | 早于 ContextManager 任何变更 |
| `PreCompact` | `phases/compact.rs:28`、`capabilities.rs:181, 195, 253` | "auto" / "manual" / "force" 等 matcher |
| `PostCompact` | `phases/compact.rs:62`、`capabilities.rs` 多处 | 输出可被合并回 context（system 消息） |
| `PreToolUse` / `PostToolUse` | 工具执行 pipeline 内 | 若干阶段；详见 echo-agent/docs/{en,zh}/23-hooks.md |
| `Stop` / `StopFailure` | `phases/finalize.rs` | best-effort，不阻断终结流程 |
| `SessionEnd` | `phases/finalize.rs` | 至少 "complete" / "max_iterations" 两种 reason |

---

## §3 已知陷阱清单

> **本文档的核心索引**。前 6 篇中所有 ⚠️ 标记的项汇总于此，方便排查 / 跟进。

### ⚠️ 陷阱 #1：`"plan"` 工具空缺

| | |
|---|---|
| 现象 | `TOOL_PLAN = "plan"` 常量声明在 `echo-agent/src/agent/react/mod.rs:82`；`has_planning_tools()` 检查包含它（`mod.rs:202-207`），但**无任何生产路径注册名字为 `"plan"` 的工具** |
| 后果 | `has_planning_tools()` 在 `enable_task=true` 时**总是返回 false** |
| 文件:行 | `src/agent/react/mod.rs:82`、`mod.rs:202-207` |
| 详见 | [02-task-planning.md §3.1](./02-task-planning.md#§31-️-plan-工具的-wiring-空缺) |
| 决策待跟进 | 注册 `CreatePlanTool` 让它名字成为 `"plan"`，或删 `TOOL_PLAN` 常量 + 简化 `has_planning_tools()` |

### ⚠️ 陷阱 #2：Skill 激活路径分裂（产物不对称）

| | |
|---|---|
| 现象 | LLM 工具激活产物是 `Role::Tool` + `<skill_content>` XML 包装（受 `protected_marker` 保护）；IntentRouter 激活产物是 `Role::System` + 裸 `instructions`（**不**受保护） |
| 后果 | 走 IntentRouter 路径激活的 skill 内容可能在长对话压缩中丢失；流式入口（GUI/TUI）只走路径 1，CLI 一次性 chat 走路径 2 —— 同一 skill 在不同入口下行为不同 |
| 文件:行 | `src/agent/react/run/react_loop.rs:738-764`（路径 2）vs `echo-execution/src/skills/external/types.rs:334-372`（路径 1 包装） |
| 详见 | [06-skills.md §4](./06-skills.md#§4-两条-skill-激活路径)、[01-runtime.md §7](./01-runtime.md#§7-intentrouter仅在非流式入口生效) |
| 决策待跟进 | 让路径 2 也调 `to_prompt_block()`（产物统一）；或注册第二个 marker 兼容 raw `Role::System` 形式 |

### ⚠️ 陷阱 #3：`UnifiedMemory` 命名空间不一致

| | |
|---|---|
| 现象 | `UnifiedMemory.remember/recall/forget/list_memories` 用 namespace `["agent", "memories"]`（字面字符串）；运行时记忆工具用 `[agent_name, "memories"]`（来自 `AgentConfig::name`）。两者不匹配 |
| 后果 | 通过 `remember` 工具写的记忆，从产品层 `unified_memory.list_memories()` 读不到；反之亦然 |
| 文件:行 | `echo-agent-cli/echo-agent-app-core/src/unified_memory.rs:198, 211, 235, 246` vs `echo-agent/src/agent/react/mod.rs:517` |
| 详见 | [04-memory.md §7.3](./04-memory.md#§73-️-两个已知陷阱) |
| 决策待跟进 | `UnifiedMemory` 让 namespace 可配置 + 默认改成跟运行时同步；或运行时改成固定 `["agent", "memories"]` |

### ⚠️ 陷阱 #4：`UnifiedMemory` 默认 store=None

| | |
|---|---|
| 现象 | `AgentRuntime::new`（`echo-agent-cli/echo-agent-app-core/src/runtime.rs:91`）调用 `UnifiedMemory::load()` —— 只加载 `.md` instructions，**不**调 `with_store(...)` |
| 后果 | 产品代码调 `unified_memory.remember(...)` / `recall(...)` 永远返回 `Err("No memory store configured")`，除非外部代码再手动挂 store |
| 文件:行 | `runtime.rs:91`、`unified_memory.rs:189-205`（remember 方法） |
| 详见 | [04-memory.md §7.3](./04-memory.md#§73-️-两个已知陷阱) |
| 决策待跟进 | bootstrap 中加一行 `unified_memory = unified_memory.with_store(shared_store.clone());` |

### ⚠️ 陷阱 #5：`skill_telemetry` 无 runtime 写入点

| | |
|---|---|
| 现象 | `echo-state/src/skill_telemetry.rs` 定义了 `SkillExecutionRecord` / `SkillTelemetry` 类型 + 基于 `Store` 的读写；CLI `evolution` 命令读取记录；**但 runtime 中无任何 `record_execution` 调用** |
| 后果 | `/evolution` 看到的遥测数据永远空（除非外部种子）；skill 性能优化没有数据驱动基础 |
| 文件:行 | `echo-state/src/skill_telemetry.rs`（schema + 读取 API 全在）；运行时 grep `record_execution` 零命中 |
| 详见 | [06-skills.md §9](./06-skills.md#§9-️-skill_telemetry--模块在但无写入点) |
| 决策待跟进 | 在 `SkillRegistry::activate` 末尾或 `phases/tools.rs` 工具批结束时记录一次；按已激活 skill 维度归并 |

### ⚠️ 陷阱 #6：`AgentRole` 在 ReactAgent 中无效

| | |
|---|---|
| 现象 | `AgentRole::Orchestrator` vs `Subagent` 仅在 `TaskExecutor::build_execute_fn`（`src/agent/react/planning.rs:236`）分支；`ReactAgent::run_core_loop` 完全不读这字段 |
| 后果 | 仅设 `role(AgentRole::Orchestrator)` 跑 ReAct 循环不会有任何区别；要让 Orchestrator 真正"只编排不干活"必须走 `TaskExecutor` + `enable_subagent` + `agent_tool` |
| 文件:行 | `src/agent/config.rs:31`（enum）、`src/agent/react/planning.rs:236`（唯一使用点）；`config.rs:25-29` 注释自己也明示这个事实 |
| 详见 | [03-subagent.md §1](./03-subagent.md#§1-️-agentrole--当前仅在-taskexecutor-生效) |
| 决策待跟进 | 若需扩展，应在 `run_core_loop` 中读 `config.role` 并影响 system prompt / 工具暴露策略 |

### ⚠️ 陷阱 #7：`protected_marker` `try_lock` 静默跳过

| | |
|---|---|
| 现象 | `agent/react/capabilities.rs:589-597` 用 `try_lock` 注册 marker，争用时仅 `warn!` 然后跳过 |
| 后果 | 如果该路径与对话流并发触发，注册可能丢失；后续 skill 激活的 `<skill_content>` 包装无 marker 保护，可被压缩淘汰 |
| 文件:行 | `src/agent/react/capabilities.rs:589-597` |
| 详见 | [05-compression.md §4.2](./05-compression.md#§42-️-默认空--生产仅注册一个-marker) |
| 决策待跟进 | 改为 `lock().await` 必然成功；或在 `discover_skills` 启动时机 + `ContextManager` 创建时机做更早串行 |

### ⚠️ 陷阱 #8：`compress_threshold_ratio` 未接入 `prepare()`

| | |
|---|---|
| 现象 | `AgentConfig.compress_threshold_ratio: f64`（默认 0.2）的语义是"剩余 token 比例 < 20% 时主动压缩"，但 `ContextManager::prepare()` 内部判断仅用 `estimated_tokens > token_limit`，**不读** `compress_threshold_ratio` |
| 后果 | 调高 `compress_threshold_ratio` 不影响 `prepare()` 的触发；它实际由另一条 pre-think 路径消费（与 ContextManager 解耦） |
| 文件:行 | `src/agent/config.rs:104`（字段）、`echo-state/src/compression/mod.rs:980-988`（`prepare` 决策） |
| 详见 | [05-compression.md §2](./05-compression.md#§2-️-prepare-的真实触发条件) |
| 决策待跟进 | 把 `compress_threshold_ratio` 通过 `TokenBudget` 传给 `ContextManager` 作为另一种 needs_compression 判定；或文档明示两条路径分工 |

### ⚠️ 陷阱 #9：`SkillsHub` CLI 子命令各自 `new()`

| | |
|---|---|
| 现象 | `/skills` 的 list/search/install/uninstall/info/refresh 子命令都 `SkillsHub::new()` 各自构造一个实例，不共享 AppState 中的 hub |
| 后果 | 一个命令的 install 完成后、另一个命令仍持着旧实例，可能漂移；多窗口/多并发调用更容易暴露 |
| 文件:行 | `echo-agent-cli/src/cli/cmd_impls/skills.rs:11, 28, ...` |
| 详见 | [06-skills.md §5](./06-skills.md#§5-skillshub--与-skillregistry-完全不同的概念) |
| 决策待跟进 | 改用 `AppState::skills_hub` 共享实例（已存在 `Arc<RwLock<SkillsHub>>` at `state.rs:426`），子命令只读不创建 |

---

## §4 与 `echo-agent/docs/{en,zh}/` 引用关系矩阵

整套 `system-deep-dive/` 文档的目标是**补充**而非替代既有 API 参考。下表说明哪个章节看哪份现有文档。

| 主题 | 本套文档 | 现有 API 参考 |
|------|---------|--------------|
| ReactAgent + ReAct 循环 | [01-runtime.md](./01-runtime.md) | `echo-agent/docs/{en,zh}/01-react-agent.md` |
| 工具系统 | [01-runtime.md §3](./01-runtime.md#§3-4-个子系统) | `echo-agent/docs/{en,zh}/02-tools.md` |
| 记忆 trait & store | [04-memory.md](./04-memory.md) | `echo-agent/docs/{en,zh}/03-memory.md` |
| 上下文压缩 | [05-compression.md](./05-compression.md) | `echo-agent/docs/{en,zh}/04-compression.md` |
| Human-in-the-loop / approval | [03-subagent.md §8](./03-subagent.md#§8-capability-flags-对照表)（capability flag）| `echo-agent/docs/{en,zh}/05-human-loop.md` |
| SubAgent | [03-subagent.md §2–§6](./03-subagent.md#§2-subagent-三种执行模式) | `echo-agent/docs/{en,zh}/06-subagent.md` |
| Skill 系统 | [06-skills.md](./06-skills.md) | `echo-agent/docs/{en,zh}/07-skills.md`、`skill-authoring.md`（已与本套同步） |
| Streaming | [01-runtime.md §1, §6](./01-runtime.md#§1-单核心循环--双入口) | `echo-agent/docs/{en,zh}/11-streaming.md` |
| Mock testing | （本套不覆盖） | `echo-agent/docs/{en,zh}/12-mock.md` |
| Chat / multi-turn | [04-memory.md §3, §4](./04-memory.md#§3-runtimestatestore) | `echo-agent/docs/{en,zh}/13-chat.md` |
| Hooks | §2 上面 | `echo-agent/docs/{en,zh}/23-hooks.md` |
| Task graph (orchestration) | [02-task-planning.md §4](./02-task-planning.md#§4-echo-orchestration-crate-概览) | `echo-agent/docs/{en,zh}/24-task-graph.md`、`25-orchestration.md` |
| Multi-agent | [03-subagent.md](./03-subagent.md) | `echo-agent/docs/{en,zh}/26-multi-agent.md` |
| Tracing | （本套不覆盖） | `echo-agent/docs/{en,zh}/27-tracing.md` |
| Config reference | [03-subagent.md §8](./03-subagent.md#§8-capability-flags-对照表)（仅 capability 部分） | `echo-agent/docs/{en,zh}/28-config-reference.md` |
| Plugin system | （本套不覆盖） | `echo-agent/docs/{en,zh}/32-plugin-system.md` |
| Self-improvement | [04-memory.md §9.2](./04-memory.md#§92-backgroundreviewer框架llm) | `echo-agent/docs/{en,zh}/25-self-improvement.md` |

> **不重复，不冲突**：本套强调"协作 + 当前实现细节 + 已知陷阱"，既有 `docs/{en,zh}/` 提供 trait 方法签名 + 用法示例。如发现两边不一致，**以代码为准**，本套先更新；既有文档作为下一轮跟进。

---

## §5 文档维护约定（提醒）

- 添加新发现的陷阱：必须同时写到对应章节 + 本文 §3
- 新增 line:file 锚：line 号可能漂移，原则是"宁可锚精确到 ±20 行也别只贴文件名"
- 跨文件引用：相对路径 `[…](./04-memory.md#§7)`；引用既有 `docs/{en,zh}/` 文件用相对路径或全路径
- 模型名称：仅使用 CLAUDE.md 允许列表中的型号
- 不写费用 / 价格 / token 成本估算
- 双语文档（如 `echo-agent/docs/{en,zh}/07-skills.md`）必须成对修改

---

## §6 后续可能的跟进方向

非本文档实施范围，仅记录候选方向供下次会议讨论：

- 修陷阱 #1（`"plan"` 工具）—— 决定保留还是删除常量
- 修陷阱 #2（Skill 路径分裂）—— 让两条路径产物统一
- 修陷阱 #3 + #4（UnifiedMemory 命名空间 + 默认 store）—— 一次性修
- 接入陷阱 #5（skill_telemetry 写入端）—— 让 `/evolution` 真有数据
- 文档化陷阱 #6 + #8（`AgentRole` 与 `compress_threshold_ratio`）—— 把它们的"半生效"边界写进 `28-config-reference.md`
- 修陷阱 #7（`try_lock` 静默跳过）—— 改成 `lock().await`
- 修陷阱 #9（`SkillsHub` 共享）—— 让 CLI 复用 `AppState::skills_hub`
