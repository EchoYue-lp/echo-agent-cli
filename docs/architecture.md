# EKO 架构说明

本文描述 `echo-agent-cli` 当前生产架构。框架内部的 ReAct、tool、memory、MCP、LSP、
workflow 等通用实现，以 `echo-agent` 仓库自己的文档和公开 API 为准。

## 产品边界

EKO 是运行在用户机器上的本地个人助理。GUI、TUI、CLI/JSONL 和 channel 是同一
Agent 能力的不同输入/渲染适配层，不是不同产品版本。

| 层                           | 职责                                                                                                            |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `echo-agent`                 | ReAct、模型协议、tools、DAG、Subagent、memory/store trait、MCP/LSP、workflow 通用机制                           |
| `echo-agent-app-core`        | EKO runtime、workspace、conversation、TaskRuntime 文件投影、AgentPool、HITL、Plugin、Browser、分析/研究产品策略 |
| `src/cli` / `src/tui`        | CLI/REPL/TUI 输入、命令和渲染                                                                                   |
| `src/tauri` / `web-frontend` | typed Tauri IPC、GUI 状态投影和交互                                                                             |
| `src-tauri`                  | 桌面进程入口、窗口和平台能力                                                                                    |

EKO 专属的 workspace identity、GUI 投影、worktree、review policy、资源预算和删除策略
留在应用层。可以跨产品复用的 task graph、状态迁移、模型协议和 tool contract 留在框架。

## 运行时所有权

```text
GUI / TUI / CLI / JSONL / Channel
                  |
                  v
       echo-agent-app-core services
  AppState / AgentRuntime / drive_chat / TaskRuntime
                  |
                  v
       echo-agent framework primitives
 ReactAgent / tools / stores / DAG / Subagent / MCP
```

### 进程级所有者

`AgentRuntime::bootstrap` 建立所有 surface 共享的模型、HITL、prompt、MCP、Plugin、
Browser 和基础 Agent 资源。GUI 通过 `AppState` 持有这些资源；TUI、CLI 和 channel
使用同一 app-core 类型与 shutdown 顺序。

进程级唯一服务包括：

- `ForegroundTurnControl`：前台 turn admission、exact cancel 和 typed settlement。
- `AgentRouter`：跨 workspace/conversation endpoint、durable inbox 和 Agent group。
- `PluginRuntimeService`：Plugin 发现、候选 staging、runtime rewire 和偏好持久化。
- `McpConfigRuntime`：用户 `mcp.json` 的唯一写入与连接 reconciliation。
- `BrowserRuntime`：托管 Chromium/Chrome backend 与 browser event projection。
- `WorkflowService` / `StructuredExtractionService`：EKO catalog、显式 runtime address、
  typed outcome 与 surface command adapter；Graph/`extract_json` 执行仍由 framework 拥有。

### Workspace runtime

`WorkspaceRuntimeRegistry` 是已加载 workspace host 的唯一进程级 owner。每个
`WorkspaceRuntimeHost` 绑定不可变 workspace ID 和根目录，并准备一组一致的文件资源：

- `FileConversationStore`
- `FileRuntimeStateStore`
- `FileStore` memory
- `ConversationDeletionService`
- workspace `TaskRuntimeStore`

`WorkspaceExecutionRuntime` 在 host 内惰性创建，持有该 workspace 的 primary Agent、
`AgentPool`、TaskRuntime、ReviewIntegration、Plugin/MCP receipts。切换 GUI focus 不会重绑
已经接受的运行。

当前 runtime reliability 工作正在把剩余 Tauri 查询、控制、事件、恢复和删除路径都
收敛到显式 workspace/conversation identity；详见
[`design/specs/runtime-reliability.md`](../design/specs/runtime-reliability.md)。

## 对话数据流

所有 surface 最终进入 `drive_chat`/`drive_chat_turn`：

```text
input
  -> PreparedUserTurn (附件/长文本规范化)
  -> ForegroundTurnControl admission
  -> workspace runtime snapshot
  -> AgentPool conversation Agent
  -> framework streaming execution
  -> ChatSink typed events
  -> transcript/checkpoint/tool projection
  -> TurnOutcome settlement
```

关键约束：

1. 同一 workspace conversation 同时最多一个用户 foreground turn。
2. surface 是来源和渲染元数据，不是并发隔离维度。
3. accepted turn 使用一次解析得到的 workspace runtime，不能在执行中再次读取 UI focus。
4. framework conversation/checkpoint 内容是权威；前端 store 只维护可重建投影。
5. terminal settlement 由 app-core 产生，文本事件或组件卸载不能提前释放 busy 状态。

## TaskRun 数据流

产品模型固定为：

```text
TaskRun -> PlanTask -> SubagentRun
```

`TaskPlan` 是可编辑、版本化 artifact；`TodoItem` 是 UI 投影。它们不拥有独立 store 或
执行器。framework 提供 `task_create/task_update/task_list` 和通用 DAG 机制，EKO 增加
`task_execute`、文件投影、workspace policy、review、worktree 和 surface 控制。

`TaskRuntimeStore` 以 `events.jsonl` 为权威事件账本，run/plan/todo/result/checkpoint 是
可恢复投影。claim、revision、attempt 和 Subagent result 都带稳定 identity；执行前必须
通过 claim，旧 attempt 不得覆盖新 revision。

长程任务在同一 TaskRun 上增加 RunTurn continuation、Goal/Requirement/Evidence、budget、
provider retry 和 boot admission，不建立第二套 task graph。后台 command cell 也只作为
TaskRun 外部命令的 durable owner，不替代 PlanTask/Subagent。

## 文件持久化

EKO 启动时把 framework 用户数据根设置为 `~/.eko`，也可用 `EKO_DATA_DIR` 覆盖。
应用不启用 SQLite。

```text
~/.eko/
  config.yaml
  mcp.json
  hooks.yaml
  skills/
  enabled-skills.json
  plugins/
  workspaces/
    <workspace-id>/
      .eko/
        workspace.json
        sessions/
        conversations/
        memory/
        evolution/
        tasks/
        traces/
        artifacts/
          user-input/
        uploads/
        data/
        papers/
        logs/
```

所有 path 都应通过 `echo_agent::paths` 或 `WorkspaceLayout` 解析。不得在应用代码新增
硬编码 `~/.echo-agent`，也不得给 EKO 引入 `SqliteStore`/`SqliteConversationStore`。

## 扩展与专业能力

- Provider/模型：Provider 保存连接与认证，模型保存协议、输入模态和上下文参数。
- MCP：用户配置与 Plugin receipt 共享 name ownership，用户配置优先。
- Plugin：根 `plugin.json` 加固定组件目录，候选完整验证后才替换 live generation。
- Skill：内置和用户 Skill 都通过 framework loader，SkillsHub 负责产品安装/启停/同步。
- 分析/研究：计划、脚本、数据、source/evidence/review/report 都保存为可检查 artifact。
- Memory/evolution：workspace-bound layered memory 和 Review Inbox 是应用策略，写入需要
  可追溯证据。

完整的已实现能力与代码入口见 [功能总览](./features.md)。

## 不变量

- GUI、TUI、CLI/JSONL、channel 功能对等，差异只在 transport 和 renderer。
- plan approval 不进入 TaskRun 状态机；plan 是 artifact，批准由 prompt/permission 驱动。
- current workspace 只表示 UI focus，不是已接受 operation 的路由依据。
- Task DAG、重试、取消、revision 语义只能有一个权威实现。
- EKO 使用文件/内存持久化，不启用 SQLite。
- 只有 Subagent 术语，不建立第二种执行角色概念。
