# EKO 当前项目状态

Last updated: 2026-08-21

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

当前工作树已有合同测试和部分 scoped IPC/event、delivery retry 路径，但尚未提交，且
M0-M8 的整体验收没有完成。因此本条仍为 In progress，不能把局部类型或 adapter 改动
当成闭环。

### Long-Horizon Runtime Closure

状态：Pending；framework CommandCell LH1 可独立启动，应用 LH2-LH4 依赖 P0 runtime
resolver、address identity、event routing 和 boot reconciler 收敛。

规格：[`design/specs/long-horizon-runtime-closure.md`](../design/specs/long-horizon-runtime-closure.md)

2026-08-21 深度审查确认 M0-M5 的 Goal、RunTurn、provider retry、正式 PlanTask Subagent
控制、Requirement/Evidence 和 checkpoint 内核真实存在，12 小时 deterministic ledger 也
可验证；但普通 conversation boot resume、Awaiter 结果所有权/回传、cell terminal repair、
typed terminal 投影和 hot `get_run_state` 尚未闭环。完成 LH0-LH6 与真实 Agent/Awaiter soak
前，不再把长程产品级验收标为 Complete。

### Surface Parity Cleanup

状态：Pending；在 P0 runtime reliability 收口后执行。

规格：[`design/specs/surface-parity.md`](../design/specs/surface-parity.md)

代码可达性审计确认 Workflow GUI 和通用结构化抽取仍是定义/IPC 已存在但生产 surface
未闭环的功能。必须复用现有 workflow executor 与 framework `extract_json`，补齐统一
app-core 服务和 GUI/TUI/CLI/channel reachability，或删除不再作为产品能力的孤立适配器。

## 下一步

先完成当前 P0 未提交切片的最小相关测试，确认 M0 合同基线和 M1 scoped runtime
resolver 真实切换至少一条 GUI 主路径；framework LH1 可在不依赖应用 identity 的前提下
并行推进。每个后续 milestone 都必须删除被替代的 focus/global adapter，并在本文件更新
已切换权威、剩余重复和下一删除目标。P0 与 long-horizon closure 验收后再执行
surface parity cleanup。

## 文档生命周期

- 活跃规格只放 `design/specs/`。
- 阶段完成后先把稳定事实合并进 `docs/` 或代码，再删除规格和临时评审记录。
- `.superpowers/` 是本地临时执行产物，不是项目文档或跨会话事实源。
