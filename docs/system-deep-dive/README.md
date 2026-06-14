# 剖析系统重点组件和系统

> 整套文档以**当前代码**为依据，剖析 Echo Agent 各核心组件的实际形态与协作方式。所有关键论断都附带 `file_path:line` 锚，方便直接跳到源码核对。

## 文档定位

- **是什么**：组件级 + 跨组件协作的系统级深度剖析。读完应能在脑中重建运行时 / 长程任务规划 / Agent 拆分 / 记忆 / 压缩 / Skill 这几大子系统的整体形态以及它们如何衔接。
- **不是什么**：API 参考（去 `echo-agent/docs/{en,zh}/NN-*.md` 各章看）；实现细节教程（去源码注释看）；新手 quick-start。
- **不复述 API**：本套文档刻意与既有章节解耦，重点讲"它和别的子系统怎么衔接"以及"哪些细节当前章节没说清"。

## 总架构图

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                        echo-agent-cli  (Product layer)                        │
│ ┌──────────────────────────────────────────────────────────────────────────┐ │
│ │ AgentRuntime ── unified_memory: UnifiedMemory ── InstructionProvider     │ │
│ │              └─ AgentPool ── SharedResources ── PooledAgent ──┐           │ │
│ │                       │                                       │           │ │
│ │                       ├─ "__background__" Agent (永不驱逐)    │           │ │
│ │                       └─ try_lock(execution_mutex) 探测 idle  │           │ │
│ │ SkillsHub  (~/.echo-agent/skills/, marketplace UI)            │           │ │
│ └───────────────────────────────────────────────────────────────┼──────────┘ │
└─────────────────────────────────────────────────────────────────┼────────────┘
                                                                  │ Arc clone
┌─────────────────────────────────────────────────────────────────┼────────────┐
│                       echo-agent  (Framework)                   ▼            │
│ ┌────────────────────────────────────────────────────────────────────────┐  │
│ │ ReactAgent                                                              │  │
│ │   execution_mutex                       ┌─ ToolExecutionSubsystem      │  │
│ │   intent_router (non-stream entry only) │  ToolManager / SkillRegistry │  │
│ │   critic                                │  HookRegistry / Sandbox      │  │
│ │   [4 subsystems] ──────────────────────►├─ GuardSubsystem              │  │
│ │   plan_state                            ├─ MemorySubsystem             │  │
│ │   token_tracker                         │  ContextManager / Store /    │  │
│ │                                         │  SnapshotManager / 2x stores │  │
│ │                                         └─ ApprovalSubsystem           │  │
│ │ AgentRunSnapshot ── O(1) Arc clone for tokio::spawn 'static            │  │
│ │   run_core_loop  ── single entry, split into phase functions:          │  │
│ │     prepare_turn → run_compact → run_think → run_tools/verify_answer   │  │
│ │     → finalize_completed_run | finalize_no_response | max_iterations   │  │
│ └────────────────────────────────────────────────────────────────────────┘  │
│                            │                          │                      │
│                            ▼ (every iteration)        ▼ (every turn end)    │
│ ┌──────────────────────────────────┐ ┌────────────────────────────────────┐ │
│ │ RuntimeStateStore                 │ │ ConversationStore                  │ │
│ │  AgentCheckpoint (msgs+plan+      │ │  StoredMessage                     │ │
│ │   active_skills+blocked_reason)   │ │  (用户可见 transcript projection)   │ │
│ │  + TaskNode DAG                   │ │                                    │ │
│ └──────────────────────────────────┘ └────────────────────────────────────┘ │
│                                                                              │
│ Store (long-term KV, Keyword/Semantic/Hybrid RRF)                            │
│ ContextManager  ── prepare/compress/promote ──► protected_marker = "<skill_… │
│ SkillRegistry   ── catalog → activate → resources                            │
│   ├─ Path 1: tool `activate_skill` → <skill_content> wrap → 受保护          │
│   └─ Path 2: IntentRouter classify → raw Role::System → ⚠️ 不受保护        │
│ echo-orchestration (TaskExecutor / Workflow Graph / 独立的 CheckpointStore)  │
└──────────────────────────────────────────────────────────────────────────────┘
```

## 阅读顺序

按依赖顺序阅读：01 → 02 → 03 → 04 → 05 → 06 → 07。每篇都能独立阅读，必要前置概念会用一两句话简介并链接到详讲文件。

| 文档 | 一句话摘要 |
|------|----------|
| [01-runtime.md](./01-runtime.md) | ReactAgent 单核心循环、双入口、`execution_mutex`、4 子系统、`AgentRunSnapshot` Arc 组合、phase 函数拆分、IntentRouter 仅非流式入口生效、Verifier 三个集成点 |
| [02-task-planning.md](./02-task-planning.md) | TaskNode DAG、**三层 Checkpoint 概念辨析**（`RuntimeStateStore` vs `CheckpointStore` vs `WorkflowCheckpointStore`）、任务工具集（含 `"plan"` tool 空缺）、`echo-orchestration` 概览、检查点触发条件、任务 DAG 水合 |
| [03-subagent.md](./03-subagent.md) | `AgentRole` 当前仅在 `TaskExecutor` 生效、SubAgent 三模式（Sync / Fork / Teammate）、`IsolatedSubAgentConfig` 隔离矩阵、`AgentPool` 的 `try_lock` 探测算法 + `__background__` 永不驱逐、capability flags 对照表 |
| [04-memory.md](./04-memory.md) | 三层架构（`Store` / `RuntimeStateStore` / `ConversationStore`）、4 个内置记忆工具、`SnapshotManager`（内存回滚）vs `RuntimeStateStore`（持久化）、`UnifiedMemory` 的命名空间不一致与默认 store=None、`InstructionProvider` 三层 .md、`auto_memory` vs `BackgroundReviewer`、`TieredMemory` 不存在的实情 |
| [05-compression.md](./05-compression.md) | `ContextManager` 字段与流程、`prepare()` 触发条件（`compress_threshold_ratio` 是另一条 pre-think 路径）、三种压缩器、`protected_markers` 的 `try_lock` 静默跳过陷阱、完整压缩流程（horizon → split → compress → merge → promote → sanitize → reinject）、Token 预算与 `CalibratedTokenizer` |
| [06-skills.md](./06-skills.md) | `Skill` trait + `SkillRegistry`、三级渐进披露、SKILL.md frontmatter 全集（含 legacy）、**两条激活路径产物不对称**（重点）、`SkillsHub` vs `SkillRegistry` 是不同概念、内置 11 个 file-based skills、`allowed-tools` 是过滤白名单、`skill_telemetry` 无 runtime 写入点 |
| [07-cross-cutting.md](./07-cross-cutting.md) | 命名空间约定一览、lifecycle hooks 全景、**9 项已知陷阱清单**（汇总各篇的 ⚠️）、与 `echo-agent/docs/{en,zh}/` 的引用关系矩阵 |

## 关于"为什么不叫 architecture.md"

`echo-agent-cli/docs/architecture.md` 是顶层产品架构图（CLI 工程 → app-core → echo-agent 三层）；本套文档是组件级深度剖析。两者互补：

- 想知道 CLI 怎么启动、TUI/GUI 入口怎么和 AgentRuntime 接 → 看 `architecture.md`。
- 想知道 ReactAgent 内部怎么把一段对话从消息推到 LLM 再到 final_answer、`SkillRegistry` 内部状态如何变迁 → 看本套。

`runtime-architecture-audit.md` 是早期审查报告（已记录 Checkpointer 已移除、SkillGateway 已删除等），保留为历史快照；本套以**当前代码**为权威。

## ⚠️ 待跟进列表速查

九项已知陷阱完整清单与代码锚位于 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单)：

1. `"plan"` tool 空缺（常量 + 检查存在但无生产注册）
2. Skill 激活路径分裂（IntentRouter 路径产物不受压缩保护）
3. `UnifiedMemory` 命名空间不一致（`["agent","memories"]` vs `[agent_name,"memories"]`）
4. `UnifiedMemory` 默认 store=None（产品层 remember/recall 失败）
5. `skill_telemetry` 模块 + 类型存在但无 runtime 写入点
6. `AgentRole` 在 `ReactAgent::run_core_loop` 中无效（仅 `TaskExecutor` 分支）
7. `protected_marker` `try_lock` 静默跳过（锁争用时 skill 内容可被压缩）
8. `compress_threshold_ratio` 未接入 `ContextManager::prepare()`（是另一条 pre-think 路径）
9. `SkillsHub` CLI 子命令各自 `new()`（与 AppState 共享态可能漂移）

## 文档维护约定

- 每个论断必须有 `file_path:line` 锚；line 号可能漂移，原则是"宁可锚精确到 ±20 行也别只贴文件名"。
- 跨文件引用用相对路径；引用 `echo-agent/docs/{en,zh}/` 章节同样用相对路径或全路径，避免歧义。
- 新发现的陷阱（⚠️）必须同时写到对应章节 + `07-cross-cutting.md §3`。
- 模型名称仅使用 `CLAUDE.md` 允许列表中的型号。
- 不写费用 / 价格 / token 成本估算。
