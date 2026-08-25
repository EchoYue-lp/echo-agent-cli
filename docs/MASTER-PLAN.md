# EKO 当前项目状态

Last updated: 2026-08-25

本文是跨会话恢复工作的简短事实源，只记录当前权威路径、未完成工作和下一步。
已完成里程碑不在这里保留实施日志；长期有效能力见 [功能总览](./features.md)。

## 产品不变量

- EKO 是本地个人助理，不套用线上多租户权限模型。
- 产品模型是 `TaskRun -> PlanTask -> SubagentRun`。
- EKO 使用普通文件、JSON/JSONL 和内存，不启用 SQLite。
- GUI、TUI、CLI/JSONL、channel 共享核心能力，仅 transport/renderer 不同。
- plan、script、source、evidence、report 是可检查 artifact，不是隐藏运行时状态。

## 当前权威路径

| 语义                                               | 权威实现                                       |
| -------------------------------------------------- | ---------------------------------------------- |
| ReAct、tools、DAG、Subagent、store traits、MCP/LSP | `echo-agent`                                   |
| EKO bootstrap 与共享服务                           | `echo-agent-app-core/src/runtime.rs`           |
| workspace host 与文件资源                          | `echo-agent-app-core/src/workspace/runtime.rs` |
| conversation Agent 并发                            | `echo-agent-app-core/src/agent_pool.rs`        |
| chat driver 与 terminal outcome                    | `echo-agent-app-core/src/chat_driver.rs`       |
| foreground admission/cancel/settlement             | `echo-agent-app-core/src/foreground_turn.rs`   |
| revisioned TaskRun graph 与文件投影                | `echo-agent-app-core/src/tasks/task_runtime/`  |
| 跨 address 消息和 groups                           | `echo-agent-app-core/src/agent_router.rs`      |
| boot recovery 与 unattended admission              | store-scoped reconciler + product owner split  |
| GUI IPC                                            | `src/tauri/commands/`                          |
| GUI address/view projection                        | `web-frontend/src/`                            |

## 已完成并已文档化

- 流式对话、会话文件存储、附件与长输入 artifact。
- revisioned TaskRun DAG、Subagent results/control、worktree、long-horizon core primitives。
- Tool schema budget、recoverable output、canonical edit、analytics runtime、image input。
- MCP resources、Browser/Chrome、LSP、Terminal、Plugin、Hook、Skill sync。
- 数据分析、学术/医学研究、Zotero/Europe PMC、review/export。
- layered memory、Review Inbox、Curator、rule/Skill promotion。
- dynamic Provider/model/protocol/thinking profile。
- workspace-qualified session/checkpoint/task/browser/control surface，JSONL mode/policy/attachment/HITL。
- Workflow GUI 与统一 structured extraction 的 GUI/TUI/CLI/channel 生产入口。
- Public framework boundary：EKO 配置已由 app-core `EkoConfig` 独立拥有，permission mode 在
  app state/pool/framework 间全程 typed 且 DTO 八变体无损 round-trip；CLI/app-core 直接
  `echo_core` 依赖与源码引用均为 0。产品 data root、Theme/Monitor/OutputStyle、coding
  auto-memory policy 留在应用层，framework 只接收显式路径、tool capability 与 command policy。

Task 2 二次集成检查点基于 framework `e103445`，覆盖显式 LLM 构造补修。该分支暂不合入
CLI `main`；合并前仍须以当时最新 framework/CLI 基线完成最终复验。

这些条目完成后的 design/plan/audit/soak 文档已经删除；需要理解当前行为时以代码、测试、
[架构说明](./architecture.md) 和 [功能总览](./features.md) 为准。

## 活跃工作

### P0 Runtime Reliability

状态：Complete（实现与开发期验收）

规格：[`design/specs/runtime-reliability.md`](../design/specs/runtime-reliability.md)

目标是完成 workspace/conversation 运行时可靠性闭环：

1. 所有 Tauri 查询、控制、恢复、事件、删除使用显式 workspace/conversation identity。
2. 同一 AgentAddress 只有一个 foreground owner；普通输入由 backend durable FIFO 接受。
3. 历史打开、live rebind、boot recovery 不覆盖正在运行的 Agent context。
4. AgentRouter 只在 transcript safe point 后结算 Delivered，并闭环 reply/backoff。
5. 删除先 settle/shutdown/evict，进程级资源上限不会随 workspace 数量倍增。
6. GUI/TUI/CLI/channel fault matrix 与 bounded smoke 全绿；长时 soak 属于项目 Final Integration Gate。

应用提交 `5603958` 已完成 M0-M7 和 M8 自动实现：workspace-scoped IPC/event、durable input
FIFO、restore/rebind、orphan terminal repair、delete/evict、AgentRouter settlement/backoff
以及 process governor 均通过完整自动门禁。M8 的 3x3 smoke 和 36-turn real-provider probe
均为零失败；10 分钟/2 小时与完整人工 GUI 场景在项目全部研发完成后统一执行。

当前收敛阶段进一步固定：TaskRuntime recovery 只缓存成功结果且文件 I/O 走 bounded blocking；
AppState 恢复普通 conversation，BackgroundTaskService 独占 global background launcher；普通 Chat
与 Paused TaskRun 的 orphan command cell 都写入 typed Interrupted terminal。Awaiter/AgentRouter
在副作用前提交 Started，并只在 framework tracked steer 到达 Drained 后结算；Agent inbox 使用
framework segmented journal + checkpointed FIFO frontier。删除使用 retirement guard，已开始但
终态不确定的 attempt 禁止自动重放。见
[ADR 0011](./adr/0011-boot-inbox-recovery-authority.md)。

### Long-Horizon Runtime Closure

状态：Complete；LH0-LH6 已由 framework `afdf3b1`、application
`b125d9d`/`0782a8c`/`cd52171` 完成。P0 的 resolver、address identity、event routing、host
recovery、fault matrix、3x3 smoke 和 real-provider probe 均已收口。

规格：[`design/specs/long-horizon-runtime-closure.md`](../design/specs/long-horizon-runtime-closure.md)

最新审查确认 Goal、RunTurn、provider retry、正式 PlanTask Subagent 控制、
Requirement/Evidence、checkpoint/suffix 投影和 12 小时 deterministic ledger 真实存在；
LH1-LH5 已关闭 CommandCell publication/retention、Started-before-side-effect、普通 Chat 精确地址、
typed terminal/artifact projection、owned repair，以及 Awaiter direct controlled dispatch、exact
interrupt、process Subagent permit、Ready/Acknowledged replay、完整 Provider profile、dedicated
surface projector、global/workspace 普通 conversation boot resume，以及 checkpoint-backed hot
state、运行边界索引、10k/100k 性能门、真实 Agent/Awaiter/surface 故障矩阵和 bounded smoke。
长时间集成测试不再阻塞功能阶段，统一归项目 Final Integration Gate。

### Channel session incarnation and product-data settlement

状态：projection authority 已合入，channel focused compile/fault matrix 全绿；等待正式提交集成与
full workspace/GUI 门禁。

- framework sender-scoped session incarnation 是 channel model context 的唯一 identity；稳定 product
  conversation 继续拥有 ChatEventLog、TaskRun 与 UI 历史。
- timeout/reset 通过 exact workspace generation + runtime-state key obligation 关闭 pool admission、等待
  foreground/lease、回收 checkpoint/transcript，并只在全部成功后 rotate；产品删除最终枚举并回收该
  stable scope 的全部 live/durable incarnation。
- `ProductDataIoService` 按 application generation 拥有附件、压缩、删除、CommandCell、analytics 与
  research blocking I/O。process semaphore 只限流；phase one seal 新 direct/flow admission，已接纳
  producer 通过 nested token 完成 safe point，phase two join 全部 settlement；caller drop 不会分离写入。
- 本阶段不建立第二套 TaskRuntime、session、conversation store 或 deletion authority。最终验收仍需
  正式提交集成、full workspace/GUI/TUI/CLI/channel 门禁和 website 镜像同步。

## 下一步

TaskRuntime async surface 已统一复用进程共享的 bounded `TaskRuntimeBlockingAdapter`，并以
ts-rs typed receipts 替代 GUI mutation 的 `serde_json::Value`/numeric interaction mode。
下一阶段仍需独立完成两项 persistence 收口：一是 journal 已提交但 projection refresh 降级时
返回统一 typed committed outcome；二是把 Todo/Artifact/Completion 当前视图改为 checkpoint
增量索引，并让 10k/100k release gate 覆盖真实 GUI snapshot，而不只测 warm run state。

Long-horizon 的 LH6 代码修复已完成：18 行故障矩阵、可取消 artifact finalize、Started projection
crash window、Awaiter Provider failure durable result、EKO permission-owned shell policy、跨 workspace
Agent execution governor、四 surface real-provider harness 与 truthful ledger 都已进入生产/测试路径。
真实探针完成 3x3、36 Provider turns、3 Awaiter、3 HITL、2 restart，零失败；10 分钟扩展并发
运行也完成 1,827 cells 且零失败。两小时尝试按研发节奏调整在 26.6 分钟主动停止，状态记录为
`interrupted_by_policy`。Task 2 EKO Control Plane 与 Surface 已在
`refactor/eko-control-surface` 完成全自动门禁；按总集成顺序，先等待 Task 1 framework
correctness 合并，再将 Task 2 merge 最新 `main`、恢复标准相对依赖并进入 Task 3/4/5 集成。
10 分钟/2 小时和完整人工 GUI 场景只在项目功能研发全部完成后执行一次 Final Integration Gate。

## 文档生命周期

- 活跃规格只放 `design/specs/`。
- 阶段完成后先把稳定事实合并进 `docs/` 或代码，再删除规格和临时评审记录。
- `.superpowers/` 是本地临时执行产物，不是项目文档或跨会话事实源。
