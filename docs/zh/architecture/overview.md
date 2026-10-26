# EKO 架构说明

本文区分 `echo-agent-cli` 当前生产架构与 [ADR 0050](../adr/0050-gui-product-convergence.md)
已批准的收敛目标。ReAct、tool、memory、MCP、workflow 等通用机制归框架；EKO 的 LSP 消费路径已移除；框架 A2A/LSP API 仍保留，等待两个消费者验收。框架公开合同继续由 `echo-agent` 自身维护。

## 产品边界

EKO 是运行在用户机器上的本地个人助理。已批准目标只有 GUI 产品主入口，继续保留 IM 消息与
cron 后台触发。GUI 内置终端、Agent shell、桌面启动参数和独立 SDK Host stdio 均继续保留。

当前基线仍包含 TUI、REPL、CLI/JSONL 和 channel 适配。迁移期间共享服务继续拥有权威；
旧的终端入口持续扩张与对等目标已被替代。实际删除终端入口必须等 GUI 接管 IM 生命周期、
cron 与共享恢复并验收后进行。本次文档改动不删除运行入口或协议。

| 层                           | 职责                                                                                                            |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `echo-agent`                 | ReAct、模型协议、tools、DAG、Subagent、memory/store trait、MCP/LSP、workflow 通用机制                           |
| `echo-agent-app-core`        | EKO runtime、workspace、conversation、TaskRuntime 文件投影、AgentPool、HITL、Plugin、Browser、分析/研究产品策略 |
| `src/cli` / `src/tui`        | 当前待退役的终端适配；先迁移 IM handler 与桌面共享依赖                                                                                   |
| `src/tauri` / `web-frontend` | typed Tauri IPC、GUI 状态投影和交互                                                                             |
| `src-tauri`                  | 桌面进程入口、窗口和平台能力                                                                                    |

EKO 专属的 workspace identity、GUI 投影、worktree、review policy、资源预算和删除策略
留在应用层。可以跨产品复用的 task graph、状态迁移、模型协议和 tool contract 留在框架。

## R4 App-Core Modules and Facade

R4 keeps the framework/app split above and makes it auditable in the physical
tree. `echo-agent-app-core::api` is the supported import boundary for CLI, TUI,
Tauri, channel, examples and integration tests. It is a re-export facade only;
it does not introduce a second runtime, store, DAG traversal, retry loop,
terminal reducer or publication registry. Implementation modules are
crate-private; callers use the facade rather than relying on internal paths.

The former aggregate files are now directory facades with authority-oriented
source files. `state` owns configuration, connection/storage DTOs,
workspace/delivery scope and the `AppState` aggregate. `tasks::task_runtime::store`
owns the EKO file journal, plan/run projections, recovery and workspace
supervisor; `tasks::task_runtime::executor` owns only resource, dispatch,
review, unattended and event adapters around framework
`RuntimeTaskService`/`RuntimeDagController`. `agent_router`, `chat_event_log`,
`agent_pool`, `extension_control`, `plugin_runtime` and `infra` follow the same
address/inbox, journal/projection, admission/generation, policy/publication and
factory/store/diagnostic boundaries.

AgentPool admission is now backed by the framework
`echo_agent::agent::admission::KeyedExecutionAdmission`, which owns opaque-key
leases, per-key process permits, retirement fences, close and idle waits. EKO
retains Agent creation, capacity classes, cache eviction, workspace generation,
ToolControl, Plugin/MCP/model publication and product receipt mapping.

The split is namespace-preserving and item-boundary based, so JSON/JSONL,
serde/TS bindings, file layout, error codes and five-surface behavior do not
change during that split; this is not a requirement to expand retired product targets. A product-neutral framework primitive does not wait for a second
consumer; placement is decided by standalone semantics, dependency direction,
and independent framework tests, examples, and documentation. A separate EKO
contracts/domain/runtime crate remains a distinct packaging decision and is
created only when dependency isolation, compile measurements, and multiple EKO
consumers justify it. See [ADR 0025](../adr/0025-app-core-global-modularization.md)
and the cross-repository [framework capability placement audit](../../../../docs/2026-08-30-framework-capability-placement-audit.md).

## 运行时所有权

当前实现（终端入口在完成迁移前仍存在）：

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

`ApplicationLifecycleOwner` 在 runtime bootstrap 成功后立即接管进程资源。关闭先同步停止
所有 admission 并广播取消，再等待已接受的 foreground、TaskRun、Agent delivery、pool 和后台
服务。GUI/headless 的失败统一返回 typed aggregate receipt；bootstrap 中途失败使用同一 owner
回滚，不依赖入口维护第二份资源清单。详细取舍见
[ADR 0004](../adr/0004-application-lifecycle-supervisor.md)。

进程级唯一服务包括：

- `ForegroundTurnControl`：前台 turn admission、exact cancel 和 typed settlement。
- `AgentRouter`：跨 workspace/conversation endpoint、durable inbox 和 Agent group。
- `AgentControlService`：模型协作的 routing service。它只接受带 discriminator 的
  `ConversationTarget` / `TaskSubagentTarget`，把 message/follow-up/wait/interrupt 分别
  交给 AgentRouter、事件 journal 或 SubagentControlService；不拥有 mailbox、TaskRun
  graph、重试循环或终态 reducer。六个 `agent_*` 工具由 `AppState` 在共享 ToolManager
  上一次绑定，因而 GUI/TUI/CLI/channel 使用同一 schema、authority 和 fail-closed 校验。
- `PluginRuntimeService`：保存 framework `PreparedPluginSet` 与 apply receipt，负责 EKO
  Subagent/monitor/theme/style policy、target publication 和偏好持久化；portable component
  的解析、apply 与 rollback 只走 framework authority。
- `McpConfigRuntime`：用户 `mcp.json` 的唯一写入与连接 reconciliation。
- `BrowserRuntime`：托管 Chromium/Chrome backend 与 browser event projection。
- `ExtensionControlService`：在当前 workspace generation 上协调 Skills、Hooks、MCP、
  Plugin 和 Browser，是所有产品 surface 共享的 EKO mutation admission。
- `WorkflowService` / `StructuredExtractionService`：EKO catalog、显式 runtime address、
  typed outcome 与 surface command adapter；Graph/`extract_json` 执行仍由 framework 拥有。

启动恢复只有两个互斥的产品 owner：AppState 恢复普通 conversation continuation，
`BackgroundTaskService` 恢复 global background run。TaskRuntime 文件恢复通过同一个 store-scoped
reconciler 的 owned singleflight 执行并只缓存成功结果；首个 caller 退出不取消恢复。普通 Chat 和
TaskRun 的 command-cell Started 事实都会在 boot 被闭合为 typed Interrupted terminal。

跨会话 live delivery 与 CommandCellWatch handoff 都使用 framework tracked steer receipt。mailbox acceptance
不是消费完成。副作用前先写 DeliveryStarted/EffectStarted，receipt 到达 Accepted/Drained 后分别写
MailboxAccepted/Drained；owner loss 没有 typed terminal 时禁止重放。Agent inbox 使用 framework
segmented journal + checkpointed reducer 作为唯一 sequence/append/projection authority，保留持久化
FIFO frontier、typed durability 与 prepared-batch reconciliation。Conversation/workspace 删除通过
retirement guard 清理对应 inbox。完整取舍见
[ADR 0011](../adr/0011-boot-inbox-recovery-authority.md)。

`watch_cell` 的 exact owner 与 terminal truth 来自 durable TaskRuntime/Chat cell projection；live active
set 只表示本进程仍持有资源。已在 watch 前完成的 retained cell 继续走 framework `CommandCellWatcher`，
立即读取 retry-safe terminal 并发布真实 typed Ready。watcher 持有 observation lease、按 byte cursor
drain，不调用模型也不占用 Subagent capacity。TaskRun 用 durable run identity 加 cell
`turn_id` 校验 resume/continuation current turn；普通 Chat 使用 conversation/root。durable read 前先取得
不暴露内容的 observation lease，live owner 失配时再二读 durable state，从而同时闭合 terminal-history prune 与
snapshot/live owner 窗口，不重跑 cell，也不从 active set 反推 terminal。

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

### Memory generation 与模型 safe point

每个 global/workspace `ReviewIntegration` 拥有一个 generation-bound
`Arc<MemoryLayerManager>` 和一个共享 `HotMemoryProjectionSource`。primary、已有 pool Agent 与
未来创建的 pool Agent 都在每次 pre-model safe point 从同一个 source 读取不可变 snapshot；
memory mutation settlement 只读一次磁盘并原子发布，不逐 Agent 加锁或执行 I/O。

公开 receipt 同时携带 authority scope、workspace generation、SHA-256 content revision、
changed、settled/degraded、当前 primary/pool/future binding 以及 pending repair debt。durable
write 成功但 projection 失败时不回滚事实；下一次 settlement 修复同一 debt。bootstrap 在首次
turn admission 前完成初始发布，workspace retirement 会清空 targets 并 fence 旧 lease。

`/reflect`、remember/forget、evidence、TaskRuntime、Dreaming 与模型工具写入都复用同一个
manager/service；CLI、TUI、GUI、JSONL 和 channel 只是输入与 typed receipt 适配层。完整取舍见
[ADR 0024](../adr/0024-generation-bound-hot-memory-projection.md)。

当前 runtime reliability 工作正在把剩余 Tauri 查询、控制、事件、恢复和删除路径都
收敛到显式 workspace/conversation identity；详见
[`design/specs/runtime-reliability.md`](../../../design/specs/runtime-reliability.md)。

### Workspace product data

文件、研究和分析入口都显式携带 `workspace_id` 与由 workspace `created_at`、
`project_root_revision` 组成的 generation，
并通过 `ScopedWorkspaceControl` 固定同一个 host incarnation 到操作 settlement。研究和
分析使用 `execution_scope.root` 作为 EKO data root；文件浏览器使用 control 解析后的
`project_root`，因此 linked project 不会改变 EKO 自有数据位置。

同步文件 I/O 统一进入 `AgentRuntime` 创建并由 `ApplicationLifecycleOwner` 关闭/等待的
per-application `ProductDataIoService`，不占用 Tokio async executor thread。进程级 semaphore
只提供总容量限制，不拥有 operation 生命周期。phase one 拒绝新的直接 I/O 与 flow；已接纳的
多阶段 producer 通过 shared flow receipt 继续 nested safe-point I/O。caller drop 后既有 owner 仍由
对应 AppState generation 等待到稳定 settlement。
分析运行由 app-owned supervisor 持有 exact workspace receipt；`run` admission 不阻塞
CLI/TUI/channel event loop，`wait/cancel` 使用同一 receipt 完成 framework draining 与 join。
cleanup 失败时 owner 不释放，因此任何 surface 的删除都会一致地返回 busy。
详细方案与取舍见
[Workspace-scoped product-data I/O ADR](../adr/0006-scoped-product-data-io.md)。

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
4. framework `ConversationStore` transcript 与 incarnation-scoped runtime checkpoint 各自权威；
   前端 store 只维护可重建投影。
5. terminal settlement 由 app-core 产生，文本事件或组件卸载不能提前释放 busy 状态。
6. 长程 TaskRun 的多个有限 RunTurn 共享一个 foreground root owner；active turn id 可推进，
   root id、cancel token 和最终 settlement authority 保持不变。完整决策见
   [Foreground continuation ADR](../adr/0005-foreground-continuation-owner.md)。
7. Channel 以 `(channel, conversation, sender)` 生成稳定 product conversation，供 ChatEventLog、
   TaskRun、UI 和 foreground 使用；framework session incarnation 再派生 AgentPool、checkpoint 与
   cache key。timeout/reset 先关闭旧 key admission、等待 foreground/lease settlement 并精确 retire，
   再精确删除旧 runtime lineage 并允许新模型上下文。稳定 transcript 通过 generation ordinal 幂等
   追加，不会注入新模型；产品删除通过 framework lineage helper 回收全部 incarnation 后删除稳定
   transcript。
   完整决策见 [Channel scope parity ADR](../adr/0010-channel-scope-parity.md)。

### Tool result 与 artifact 投影

Framework `ToolResult` 是 tool terminal 的唯一事实，完整 spill 输出通过类型化
`ToolResult.artifact` 传递。EKO 的 `ToolExecutionRepository` 无损持久化 invocation/result，
并在读取或向 channel 暴露 artifact 前验证 registered root、retention、文件身份、大小与摘要。
GUI detail cursor 和 retention/cleanup 仍属于应用策略。GUI、TUI、CLI/JSONL 与 channel 不从
metadata 重建 artifact，也不以本地路径存在性推断另一个 terminal 状态。详细取舍见
[Typed tool artifact projection ADR](../adr/0018-typed-tool-artifact-projection.md)。

## TaskRun 数据流

产品模型固定为：

```text
TaskRun -> PlanTask -> SubagentRun
```

`PlanRevision` 是可编辑、版本化的 TaskPlan artifact；framework `TaskStatus` 是唯一任务执行
状态。`TodoItem` 只在查询边界由 canonical execution 投影，不能反向写入 task graph，也不拥有
独立 store、reducer 或执行器。完整决策见
[Task graph status authority ADR](../adr/0015-task-graph-status-authority.md)。framework 提供
`task_create/task_update/task_list` 和通用 DAG 机制，EKO 增加
`task_execute`、文件投影、workspace policy、review、worktree 和 surface 控制。

`TaskRuntimeStore` 以 `events.jsonl` 为权威事件账本，run/plan/todo/outcome/checkpoint 是
可恢复投影。claim、revision、attempt 和 Subagent outcome 都带稳定 identity。所有 async 文件
Todo/latest summary/completion 等有界热状态进入 checkpoint；无限增长的 Artifact/Review 历史
由同一 `RunAuthority` 增量投影到 Artifact segment 和安全编码的 per-task Review segment，segment
与 cursor 均可删除后从 journal 重建，不形成第二 authority。
操作必须通过 store-owned operation supervisor 进入 bounded async/blocking 边界；Application
shutdown 与 workspace eviction 都会等待已接受 operation，不由 surface caller future 决定其
寿命。operation admission seal 与 reservation 注册线性化，command manager 在 phase one 先关闭
admission并取消进程，terminal projection repair 有界且以 typed debt 报告。执行前必须通过
claim，旧 attempt 不得覆盖新 revision。TUI/CLI resume surface 不预判 journal sequence，统一由
store 原子 resume authority 处理 diagnostic suffix 与 ABA。
Workspace shutdown 越过 idle proof 后保持 Closing，唯一 settlement 被缓存；degraded generation
不会重新开放。所有异步 ChatEvent safe point 都通过 bounded operation boundary，并把 exact workspace I/O
receipt 捕获进 blocking closure，保证 caller drop 与 workspace delete 的顺序。

production DAG 只通过 framework `RuntimeTaskService` 驱动；EKO adapter 只提供产品 policy、
类型转换与 file-journal transaction。完整决策见
[RuntimeTaskService 适配决策](./runtime.md)和
[TaskRuntime async I/O 与 typed IPC ADR](../adr/0009-taskruntime-async-io-and-ipc-boundary.md)。

Store、Journal、Checkpoint、Trace 的产品边界和完整权威矩阵见
[EKO 持久化概念](./persistence.md)。

长程任务在同一 TaskRun 上增加 RunTurn continuation、Goal/Requirement/Evidence、budget、
provider retry 和 boot admission，不建立第二套 task graph。后台 command cell 也只作为
TaskRun 外部命令的 durable owner，不替代 PlanTask/Subagent。

一个用户可见的长程 operation 只由外层 `ForegroundTurnLease` settlement。后续 RunTurn
通过不可 settlement 的 progress handle 更新 current active id，并由 continuation completion
receipt 把 Deferred/Stop 反馈给外层 owner；surface cancel 使用 root id，steer 使用 active id。

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

所有 path 都应通过 `echo_agent::paths` 或 `WorkspaceLayout` 解析。EKO 不扫描或导入
`~/.echo-agent`，也不得给应用引入 `SqliteStore`/`SqliteConversationStore`。
`.eko/workspace.json` 是唯一可读取 workspace manifest。root `.workspace.json` 只在创建时
用于防止误覆盖目录，不会被解析或迁移。

## 扩展与专业能力

- Provider/模型：Provider 保存连接与认证，模型保存协议、输入模态和上下文参数。
- MCP：用户配置与 Plugin receipt 共享 name ownership，用户配置优先。
- Plugin：framework 从根 `plugin.json` 和固定组件目录生成不可变 prepared generation；EKO
  在 captured workspace target 上补充产品组件，完整验证后才替换 live generation。rollback
  使用 exact apply receipt，不重读旧文件。
- Skill：用户安装、当前项目 `.eko/skills/` 与 Plugin generation 提供的外部 Skill 都通过
  framework loader；SkillsHub 负责用户 artifact 的 discovery/install/sync，不拥有第二份
  live registry。
- 分析/研究：计划、脚本、数据、source/evidence/review/report 都保存为可检查 artifact。
- Memory/evolution：workspace-bound layered memory、Review Inbox、shared hot projection 与
  `/reflect` 是应用策略；写入需要可追溯证据，并返回 generation-bound settlement receipt。

完整的已实现能力与代码入口见 [功能总览](../features.md)。

### Extension Control Authority

扩展控制按四层分工：

| 层                 | 唯一职责                                                                             |
| ------------------ | ------------------------------------------------------------------------------------ |
| framework          | Skill/Hook registry、MCP/LSP 协议与通用 manager                                      |
| specialist runtime | Plugin scan/wiring、MCP reconcile、Hook/Browser 实际执行                         |
| EKO app-core       | workspace generation capture、mutation admission、配置文件、生命周期和 typed receipt |
| surface            | 参数转换和 receipt 渲染                                                              |

`enabled-skills.json` 是外部 Skill 启停的唯一持久事实，version 3 只保存 `{enabled}` flat
map。Skill settlement 在 extension mutation 锁内原子写，再向 global seed、已加载
workspace、existing AgentPool 和 future Agent reconcile。target 失败返回带逐 target 错误的
typed `Degraded` receipt，不回滚已经提交的配置，也不保留 generation CAS、operation
identity 或 repair debt。

配置损坏或不可读时回退空集合。文件已写但运行时未同步完成的窗口，由下一次 Skill 操作、
restart 或 workspace load 重新读取 flat policy 并收敛。EKO 不再扫描源码树内置目录、加载
Tauri Skill resource、生成默认启用集或注入方法论 baseline。install/uninstall/sync 的 artifact
结果与 runtime settlement 保持分离，surface 不私下重试。

service 接受 operation 后由应用 lifecycle 持有到 settlement；caller drop 不能取消已经接受的
提交或 fanout。shutdown 先关闭 admission，再等待已接受 operation。完整决策见
[ADR 0012](../adr/0012-extension-control-authority.md)。

version 3 flat policy、`atomic_write`、ProductData-owned `SkillSyncReceipt` 和带
workspace generation 的 target receipt 已进入生产路径。GUI/headless bootstrap 在 Agent
delivery recovery 前调用 on-load reconcile，workspace create/switch settlement 也执行同一
收敛路径。

`ExtensionCommandDispatcher` 提供 Skills/Plugins/MCP/Hooks/Browser 的 surface-neutral
request/receipt。GUI 使用 typed Tauri IPC；JSONL 把 typed `ExtensionReceipt` 写入 canonical
journal/event stream且不进入模型；CLI、TUI、channel 通过同一 app-core service 做文本适配和
terminal settlement。MCP health 按 authority scope 保存，Hook 使用 captured project root，
Browser 继续通过现有入口共用该实现；LSP 消费已退出，不要求扩张待退役终端入口。

Plugin 与 MCP mutation 在 admission 时一次捕获 global seed 和全部 loaded workspace target；
receipt 固定 scope、workspace generation、previous/candidate prepared generation、typed
settled/degraded status 与 bounded diagnostics；Tauri/frontend 只做无损 wire projection。
fanout 不再从 `AppState` 重新枚举 follower。cold workspace 从 authority 当前 framework
generation/revision 起步，再加入该 target 的 Project/Local overlay；monitor scheduler key 带
target scope。旧 `.lsp.yaml` 仅保留退休诊断，不触发语言服务或 Plugin 重载。
详见 [ADR 0022](../adr/0022-framework-prepared-plugin-generations.md)。

普通 Chat 的 framework envelope 由 app-core sink 做 durable journal 与 surface 投影，
但通用 turn summary 不在应用重复折叠。`AgentTurnDriver` 返回的 `TurnReceipt` 是 typed
terminal、provider usage、context compaction 次数、final answer/message identity、sequence
和 elapsed time 的唯一权威，并贯穿 chat、continuation 与 TaskRuntime lifecycle；EKO 不再
定义 `ChatTurnOutcome` 包装，Task/Webhook 只在最终边界读取所需字段。
`ChatEventLog` 继续只拥有 workspace stream identity、语义 retention pin 与产品恢复事实，
底层 sequence、segment、integrity、durability 和 pruning 复用 framework journal。

## 不变量

- 目标入口为 GUI，IM/cron 共享既有核心能力；当前终端入口待迁移验收后退役，不再扩张。
- plan approval 不进入 TaskRun 状态机；plan 是 artifact，批准由 prompt/permission 驱动。
- current workspace 只表示 UI focus，不是已接受 operation 的路由依据。
- Task DAG、重试、取消、revision 语义只能有一个权威实现。
- 通用 turn terminal 与 accounting 只能来自 framework `TurnReceipt`，surface/event sink 不重复推断。
- EKO 使用文件/内存持久化，不启用 SQLite。
- 只有 Subagent 术语，不建立第二种执行角色概念。


## 产品收敛边界

目标数据流为 GUI / IM / cron 进入既有 app-core 服务和 framework Agent / Task / Subagent。
`ConversationStore` 与 TaskRuntime journal 分别拥有会话和任务事实，前端只做投影。
EKO 使用文件/内存，不引入 SQLite；独立框架消费者的存储选项另行判断。

GUI 保留 Side Conversation 导航与并行面板。IM 保留普通 conversation、Subagent 和已有 fork
行为，不新增 Side Conversation 专用 wire contract。终端专属命令可以随入口退出，共享语义
必须由保留的 owner 继续提供。

隐藏 GUI 窗口只改变可见性；真正退出关闭准入并等待生命周期结算。重启沿既有 IM identity、
cron occurrence 和 TaskRuntime 恢复规则处理，不重放结果未知的副作用，不承诺进程退出后继续
运行。GUI 管理和原生生命周期验收仍是后续工作，见[源码处置清单](./runtime.md#产品收敛处置清单)。

撤销 A2A/LSP 必须先让 EKO/SDK 消费者解除依赖，再删框架；新增通用 API 仍先交付框架。
EKO CI 跟踪 framework main，SDK Host 独立精确 pin 不由顶层 gitlink 替代。
MCP、其它合法插件组件、本地终端控制与 SDK Host stdio 保留。此次具名能力退役不授权按应用
调用数量随意删除框架公开 API。


### GUI 视觉基础组件

GUI 沿用 `:root` / `.dark` 语义变量统一表面、辅助文字、控件对比度、边框和层级。
`uiStore` 继续拥有主题状态，插件主题仍通过同一根节点上的 inline 变量覆盖。
Card、状态徽标、开关和品牌图标消费这些变量；状态徽标同时用文字、不同图标和颜色表达。

Agent 与上下文面板统一使用 52px 标题节奏和 32px 图标控件。左栏保持 264px，与既有右栏
尺寸计算一致；context target、resize 偏好与持续挂载的主会话不变。Modal 的遮罩模糊和表面
阴影只属于呈现，portal、嵌套栈、inert、焦点恢复、Tab、Escape 继续由现有实现拥有。
Tauri 模式使用既有 `VITE_EKO_TAURI` 检测标记。

侧栏与设置使用可读标签，并在键盘聚焦时显示行内操作。设置页面沿用原 tab 注册表按需加载；
取消总览页，打开设置直接进入模型页。分类导航使用紧凑行距，内容与导航分别滚动，关闭按钮
统一位于右上角。设置按模型与扩展、会话与记忆、连接与自动化、运行与诊断、高级组织；高级组
默认收起，直接打开其设置项时自动展示该组。等待或加载失败时，导航、关闭控件和主工作区保持挂载；可选择其它设置项，
或刷新应用后重新加载对应模块。

浏览器隔离夹具的主题/布局截图与原生 Tauri 窗口验收分别取证。视觉层不新增 chat、HITL、
任务、终端或插件执行状态，也不引入新组件框架或主题 store。


### LSP 消费边界

primary bootstrap 与 scoped AgentPool 构造不再安装五个 LSP 工具或 manager。
`PluginRuntimeService` 独立保存 project-root 锁，仍在原发布成功/失败收敛点更新根目录。
MCP ownership token、生命周期 rollback、monitor 替换和 parked cleanup receipt 继续由原 owner 结算。

混合 prepared generation 只对已退役 LSP 组件诊断容忍，其它组件失败仍按原策略拒绝。
reload 收据报告退役组件，合法 Skill/MCP/Subagent/monitor/theme/style 继续处理。
完整通用冻结诊断（含 Warning）在既有收据文案中保留原严重程度与来源身份；
EKO 不依赖 LSP 专属 document、字段或方法。晚到文件变化不改变已准备代的收据，
cache invalidate 仍由原框架 integrator 拥有。
旧配置识别仅供诊断；设置保留 lazy 与局部 ErrorBoundary，旧 `overview` 或未知 `lsp` 键回到模型页，旧 typed/invoke
请求通过既有 unsupported/serde 边界明确失败。

## 工作空间与任务导航

工作空间、项目与工程是现有 Workspace 的同一概念。侧栏分别提供“新建任务”“最近任务”和“工作空间”。新建任务在所选工作空间打开普通 Conversation 草稿，不新建目录，也不停止其它会话的后台执行。最近任务按需、有界读取现有工作空间会话，最多同时两次元数据查询；归档和 Side Conversation 仍保留原有呈现入口。最近任务不替代 TaskRun 的执行任务图。

主输入区左下为工作空间选择与权限，右下为真实配置的模型选择。工作空间菜单可搜索已登记项目，并复用原来源表单选择现有本地文件夹、新建文件夹或 SSH 设备与远程目录。远程项目显示已验证执行位置，不以本机产品资料目录冒充。IM 渠道、定时任务、终端、设置、HITL 与执行任务详情继续经原入口访问。大尺寸主输入使用显式 variant，Subagent 输入保持紧凑布局。

对话运行时仍可追加输入。出现“已收到”表示这条输入已保存，输入框会清除本次仍未改动的文字和附件，不必等待模型或工具处理结束；它不表示输入已经执行或任务已经完成。附件准备或输入准入失败会保留原稿，未修改原稿的重试沿用同一次提交。处理中的重复发送不会新建第二条相同提交。切换任务、重新打开会话或编辑了新稿后，迟到结果不会清掉新稿。停止仍使用独立的停止操作。

工作空间与会话 store 及后端生命周期继续拥有事实；导航仅以临时界面意图与已有工作空间 generation 丢弃被新选择取代的返回，不增加第二任务 store、最近任务数据库、权限模型或 SSH owner。

默认桌面首屏使用既有系统字体回退栈，不默认请求远程字体；用户自行配置的主题字体不受影响。
