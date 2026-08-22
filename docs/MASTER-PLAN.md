# EKO 当前项目状态

Last updated: 2026-08-22

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

这些条目完成后的 design/plan/audit/soak 文档已经删除；需要理解当前行为时以代码、测试、
[架构说明](./architecture.md) 和 [功能总览](./features.md) 为准。

## 活跃工作

### P0 Runtime Reliability

状态：In progress

规格：[`design/specs/runtime-reliability.md`](../design/specs/runtime-reliability.md)

目标是完成 workspace/conversation 运行时可靠性闭环：

1. 所有 Tauri 查询、控制、恢复、事件、删除使用显式 workspace/conversation identity。
2. 同一 AgentAddress 只有一个 foreground owner；普通输入由 backend durable FIFO 接受。
3. 历史打开、live rebind、boot recovery 不覆盖正在运行的 Agent context。
4. AgentRouter 只在 transcript safe point 后结算 Delivered，并闭环 reply/backoff。
5. 删除先 settle/shutdown/evict，进程级资源上限不会随 workspace 数量倍增。
6. GUI/TUI/CLI/channel fault matrix 与 2 小时 soak 全绿后才标记 Complete。

应用提交 `5603958` 已完成 M0-M7 和 M8 自动实现：workspace-scoped IPC/event、durable input
FIFO、restore/rebind、orphan terminal repair、delete/evict、AgentRouter settlement/backoff
以及 process governor 均通过完整自动门禁。最终状态仍为 In progress，因为真实 GUI
验收证据和规格要求的 2 小时多 workspace soak 尚未记录；完成这两项后才能改为 Complete。

### Long-Horizon Runtime Closure

状态：In progress；LH0 已由应用提交 `4ab7407` 完成，LH1-LH6 Pending。P0 的 resolver、
address identity、event routing 和 host recovery 已可直接复用，不再等待未来 cutover；
最终 LH6 仍依赖 P0 GUI/2h soak closeout。

规格：[`design/specs/long-horizon-runtime-closure.md`](../design/specs/long-horizon-runtime-closure.md)

最新审查确认 Goal、RunTurn、provider retry、正式 PlanTask Subagent 控制、
Requirement/Evidence、checkpoint/suffix 投影和 12 小时 deterministic ledger 真实存在；
同时冻结 LH-F01..LH-F13：普通 conversation boot resume、Awaiter owned receipt/回传、
Started-before-side-effect、跨轮 drain retention、普通 Chat cell 精确地址、terminal repair、
typed projection、process governor 覆盖、完整 Provider profile 和 hot `get_run_state` 尚未闭环。
完成 LH0-LH6 与真实 Agent/Awaiter soak 前，不再把长程产品级验收标为 Complete。

### Surface Parity Cleanup

状态：Pending；在 P0 runtime reliability 收口后执行。

规格：[`design/specs/surface-parity.md`](../design/specs/surface-parity.md)

代码可达性审计确认 Workflow GUI 和通用结构化抽取仍是定义/IPC 已存在但生产 surface
未闭环的功能。必须复用现有 workflow executor 与 framework `extract_json`，补齐统一
app-core 服务和 GUI/TUI/CLI/channel reachability，或删除不再作为产品能力的孤立适配器。

## 下一步

Long-horizon 的 LH0 已完成：LH-F01..LH-F13 可运行合同、调用图、18 处生产全扫描、
资源边界、10k release fixture 和历史 soak 证据均已冻结。下一实施切片是 framework
LH1a：async bounded prepare/start、publish-before-run 和 tracked capacity；framework 合并后
再进入 typed wait/shutdown 的 LH1b。P0 runtime reliability 的真实 GUI 验收和 2 小时并发
soak 可独立完成，但必须在 LH6 前记录 artifact、资源峰值与零串扰结果。

## 文档生命周期

- 活跃规格只放 `design/specs/`。
- 阶段完成后先把稳定事实合并进 `docs/` 或代码，再删除规格和临时评审记录。
- `.superpowers/` 是本地临时执行产物，不是项目文档或跨会话事实源。
