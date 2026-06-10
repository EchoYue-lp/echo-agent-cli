# EchoAgent 运行时架构审计报告

> 生成日期: 2026-06-09
> 审查范围: echo-agent (框架) + echo-agent-cli (产品)

---

## 一、运行时子系统清单

### 1.1 核心执行引擎

| 组件 | 文件 | 职责 |
|------|------|------|
| `ReactAgent` | `echo-agent/src/agent/react/mod.rs` | Agent 主体，持有 4 个子系统 |
| `AgentRunSnapshot` | `echo-agent/src/agent/snapshot.rs` | O(1) Arc 克隆快照，用于 `'static` 流式执行 |
| `run_core_loop` | `echo-agent/src/agent/react/run/stream_channel.rs` | **唯一执行引擎**，ReAct 循环 |
| `execution_mutex` | `ReactAgent.execution_mutex` | 每 Agent 实例串行化执行 |

### 1.2 四大子系统

| 子系统 | 结构体 | 职责 |
|--------|--------|------|
| **Tool Execution** | `ToolExecutionSubsystem` | ToolManager, SkillRegistry, HookRegistry, MCP, Sandbox, Intervention |
| **Guard** | `GuardSubsystem` | GuardManager (安全检查), AuditLogger, CircuitBreaker |
| **Memory** | `MemorySubsystem` | ContextManager, Store, Checkpointer, SnapshotManager, ConversationStore |
| **Approval** | `ApprovalSubsystem` | HumanInLoop provider, PermissionService |

### 1.3 意图路由

| 组件 | 文件 | 职责 |
|------|------|------|
| `IntentRouter` | `echo-agent/src/intent/mod.rs` | 入口分类：DirectAnswer / SkillRequired / Fallback |
| `KeywordClassifier` | `echo-agent/src/intent/classifier.rs` | 零成本关键词匹配（词边界+权重评分） |
| `LlmIntentClassifier` | 同上 | 语义分类（~500 tokens，LLM 兜底） |
| `ChainedClassifier` | 同上 | 链式分类器：Keyword → LLM |

### 1.4 持久化

| 组件 | 文件 | 职责 |
|------|------|------|
| `RuntimeStateStore` | `echo-agent/src/state/mod.rs` | AgentCheckpoint (消息+计划+技能) + TaskNode DAG |
| `SqliteRuntimeStateStore` | `echo-agent/src/state/sqlite.rs` | SQLite 实现 |
| `Checkpointer` | `echo-agent/src/memory/checkpointer.rs` | 轻量线程状态恢复（仅消息） |
| `SqliteConversationStore` | `echo-agent/echo-state/src/memory/sqlite_conversation.rs` | 对话历史持久化 |

**恢复策略**: `restore_thread_context` 优先使用 RuntimeStateStore（完整恢复），回退到 Checkpointer（消息恢复）。

### 1.5 多 Agent 并行

| 组件 | 文件 | 职责 |
|------|------|------|
| `AgentPool` | `echo-agent-app-core/src/agent_pool.rs` | 多 Agent 实例池，共享资源 Arc 克隆 |
| `SharedResources` | 同上 | LlmClient, ToolManager 等昂贵资源共享 |
| `PooledAgent` | 同上 | 独立 execution_mutex + ContextManager |

**入口覆盖**: GUI ✅ | TUI ✅ (后台任务隔离) | CLI ❌

---

## 二、数据流

```
用户输入
  → IntentRouter.classify()
    → KeywordClassifier (零成本)
    → LlmIntentClassifier (语义兜底, 仅高置信度接受)
  → SkillRequired → activate_skill() → 注入 SKILL.md 指令
  → run_core_loop (ReAct 循环)
    → create_execution_node() [TaskNode: Running]
    → prepare() (上下文压缩)
    → save_runtime_checkpoint() [压缩前]
    → LLM stream → 解析 tool_calls
    → verify_answer() [Critic 自检]
      → 通过 → finish() → update_node(Success)
      → 失败 → 注入反馈 → continue
    → execute_tool_with_policy()
      → Intervention → PreToolUse hooks → Permission → Execute → PostToolUse
    → auto_snapshot()
    → save_runtime_checkpoint() [周期性]
  → final_answer
    → save_runtime_checkpoint()
    → update_node(Success)
    → yield AgentEvent::FinalAnswer
```

---

## 三、已解决的技术债

| 问题 | 状态 | 解决方式 |
|------|------|---------|
| AgentMode 空转 | ✅ 已删除 | modes.rs 删除，内容迁移到 SKILL.md |
| SkillGateway 重复 | ✅ 已删除 | 改用框架 KeywordClassifier |
| 工具执行双路径 | ⚠️ 部分统一 | execute_tool_feedback_raw 走 Pipeline，stream 路径仍内联 |
| 双重持久化 | ✅ 已理清 | RuntimeStateStore 优先，Checkpointer 回退 |
| 4 套 Hook 碎片化 | ✅ 已桥接 | TaskHookBridge + SubagentHookBridge |
| AgentRunner 死代码 | ✅ deprecated | 标记废弃，指向 ReactAgentBuilder |
| CLI mode 残留 | ✅ 已清理 | args.rs + completion.rs |
| 权限模式不一致 | ✅ 已统一 | Tauri IPC 接受 CLI 同名模式 |
| IPC 占位符命令 | ✅ 已清理 | 实现 list_skills/get_skill，其余返回 NotImplemented |
| Verifier 缺失 | ✅ 已实现 | LlmCritic 集成 3 个 final_answer 路径 |
| TaskNode DAG 未用 | ✅ 已接入 | 创建/更新/水合完整生命周期 |
| CheckpointPolicy 缺失 | ✅ 已补全 | 工具错误/压缩前/用户命令触发 |

---

## 四、已知限制

| 限制 | 影响 | 建议 |
|------|------|------|
| 工具执行双路径 | stream 路径缺少 Pipeline 的 OutputGuard/Truncation 阶段 | 中优：统一为 Pipeline |
| Graph Parallel 顺序执行 | ConcurrentWorkflow 实际串行 | 低优：已知限制 |
| AgentPool CLI 未接入 | CLI 无后台任务隔离 | 低优：CLI 单用户场景 |
| ContextAssembler 未集成 | 仅在自定义场景可用 | 已标注为框架 API |
| TUI mode 标签 | 仅状态栏颜色编码，不连接行为 | 已接受（无害） |

---

## 五、与项目目标契合度

| 目标 | 状态 |
|------|------|
| 通用 Agent 框架 | ✅ Agent trait 不绑定领域 |
| Skill 优先 | ✅ Mode 删除，Skill 三级披露 + IntentRouter 自动触发 |
| Coding 场景 | ✅ LSP + Sandbox + ReadBeforeEdit + LoopDetector |
| 数据/学术/医学 | ✅ 8 研究工具 + 专用 Skill + Evidence Medicine |
| 本地 CoWork | ✅ TUI + GUI + HITL + SQLite + 本地 LLM |
| 多对话并发 | ✅ AgentPool (GUI + TUI 后台隔离) |
| 长程任务 | ✅ Checkpoint + TaskNode DAG + Verifier + Resume |
| 对标 Claude Code | ✅ ReAct + Tool + Memory + Sandbox + Critic + Plan |
