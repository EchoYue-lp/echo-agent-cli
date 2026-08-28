# EKO 应用当前状态（MASTER-PLAN）

> 本文是 `echo-agent-cli` 应用层的跨会话事实源，只记录 EKO 的权威路径、当前阶段状态、未完成工作和验收入口。
> framework 公共 API 与实现事实归 `echo-agent`；官网同步归 `echo-website`。历史实施日志保留在 Git 历史和带日期的专项文档中。
> Last updated: 2026-08-28。

## 产品不变量

- EKO 是本机个人助理，不套用线上多租户权限模型；CLI 不启用 SQLite。
- 产品模型是 `TaskRun -> PlanTask -> SubagentRun`。`TaskStatus` 是执行权威，Todo 只是只读 UI 投影，plan 是可编辑/可审阅 artifact。
- GUI、TUI、CLI/JSONL、channel、cron/background 共享 app-core 能力；transport/renderer 不拥有第二套运行状态。
- framework 提供通用 turn/event、Task DAG、Subagent、Tool、Store、MCP/LSP/channel 和 checkpoint 原语；EKO 负责 workspace、DomainProfile、文件/工件、review/worktree、pool、产品 policy 与 surface 投影。
- 所有取消、失败、部分副作用和恢复以持久事实为准；不得重复 terminal 或把不确定写入自动重放。内部执行角色统一称 `Subagent`，不得新增 `worker` 命名。

## Child 基线

| 项目 | 当前 SHA | 状态 |
| --- | --- | --- |
| framework `echo-agent` | `302453b174086c3795dc026d16eeb668ecc66bed` | `main` 与 `origin/main` 对齐；CLI 通过相对路径 `../echo-agent` 消费。 |
| application `echo-agent-cli` | `d09f11c7878474d0e01ba2562309d5890e369554` | `main` 与 `origin/main` 对齐；F2-F5 已合流。 |

F2-F5 合流、测试卫生和适用完整门禁证据见顶层 [`plan_03`](../../docs/supreme/plans/2026-08-28T0013-项目未完成工作收敛/plan_03_F5收口完整验证主分支合并与资源清理.md)。本地 child SHA 不等于最终 release：10k/100k、长时 soak、人工 GUI、远端 CI 和 push/release 仍未执行。

## 阶段状态

| 阶段 | 状态 | EKO 应用侧结论 |
| --- | --- | --- |
| F0 characterization | Complete | 已进入 `src/main.rs`，作为各入口回归基线。 |
| F1 receipt/admission | Complete | Persisted/Accepted/Drained/TurnSettled 已由 app-core 与 framework 合同承载。 |
| F2 Task/Plan/Todo authority | Complete | revisioned TaskRun graph、`TaskStatus` 和 Todo projection 已切换到唯一 authority；ADR [0015](./adr/0015-task-graph-status-authority.md)。 |
| F3 Agent control tools | Complete | `agent_list/inspect/message/followup/wait/interrupt` 是薄 adapter，复用 router、SubagentControl 和 TaskRuntime；ADR [0016](./adr/0016-agent-control-tools.md)。底层 bounded query 仍待 R1/P0。 |
| F4 删除 InteractionMode | Complete | 生产代码、DTO、prompt、GUI/TUI/CLI/channel mode surface 已删除，不得改名恢复。 |
| F5 Agent/Subagent lifecycle | Complete | lifecycle characterization、生产修复、panic-lint hygiene 和适用完整门禁均已完成。 |
| F6 cursor/recovery/surface parity | **Partial** | 已有 cursor token、multi-target wait、boot reconcile、router restart 测试和静态 surface matrix；跨重启 Conversation/TaskSubagent cursor、cold address、workspace switch/delete、统一 fixture 与 stranded-resource 验收未闭环。 |
| R0 boundary audit | Complete | 顶层审计覆盖 151 个 app-core Rust 文件，纯只读；见 [`current-framework-application-boundary-audit`](../../docs/2026-08-28-current-framework-application-boundary-audit.md)。 |
| R1 framework-first migration | Not started | 19 个 `Migrate/converge` 与 8 个 `Conditional` 候选尚未切换；应用 adapter 必须保持无损且删除被替代主路径。 |
| R2 examples convergence | Inventory only | 64 个 framework examples 的 disposition 已记录，CLI 只消费稳定 facade，不拥有 examples 迁移。 |
| R3 docs/website | Not started | 等 R1/R2 API/examples 稳定后，再由各 child owner 同步文档和 website manifest。 |
| G Final Integration/Release | Not started | 完整三仓门禁、性能/soak、人工 GUI、远端 CI、website 和 child-first 发布均后置。 |

## 当前权威路径

| 语义 | 唯一应用入口 |
| --- | --- |
| bootstrap/config/pool | `echo-agent-app-core/src/runtime.rs`、`infra.rs`、`agent_pool.rs` |
| workspace host/resource | `echo-agent-app-core/src/workspace/runtime.rs` 与 `state.rs` |
| chat turn/event projection | `chat_driver.rs`、`foreground_turn.rs`、`chat_event_log.rs` |
| Conversation routing/inbox | `agent_router.rs`；不在 surface 解析地址或自建 mailbox |
| Agent control | `agent_control.rs`；只做 discriminator、revision、attempt、generation 校验和既有 service 调用 |
| TaskRun graph/status | `tasks/task_runtime/`、`run_authority.rs`；framework `RuntimeDagExecutor` 是 DAG 内核 |
| Task/Subagent control | `tasks/task_runtime/subagent_control.rs`；attempt-scoped guidance/interrupt/receipt |
| boot recovery/admission | store-scoped reconciler、foreground owner、BackgroundTaskService 和 extension control split |
| shared app services | `AppState`、`TaskRuntimeBlockingAdapter`、`ProductDataIoService`；GUI/headless 不得各建 authority |
| GUI IPC/projection | `src/tauri/commands/` 与 `web-frontend/src/`，只传 DTO、事件和 typed receipts |

## F6 实施入口与退出门

F6 owner 是 app-core；framework 只接收通用 event/cursor/checkpoint 需求。实现前先确认定义、注册和真实可达性，避免第二套 store、reducer、DAG loop 或地址解析。

1. 使用同一 deterministic fixture，验证 Conversation 与 TaskSubagent cursor 的 append、wait、消费、restart、resume 和 terminal 去重。
2. 覆盖 cold/unloaded address、workspace generation switch/delete、router restart、boot reconcile、orphan command cell、receipt/lease/handle 清理。
3. 让 GUI、TUI、CLI/JSONL、channel 共用 fixture 和事件合同，比较 identity、error、artifact、HITL、cancel 和终态投影。
4. 删除 surface-local 推断；generic terminal/retry/settlement 只能来自 framework 事件和 app 的单一 receipt owner。

F6 只有在无重复 terminal、无 stranded receipt/handle、跨重启可恢复、删除无残留且五入口 fixture 全绿时才可标记 Complete。

## R1 应用侧迁移规则

- framework producer 先改，CLI adapter 后切换；adapter 只做类型转换、metadata、产品 policy/hook 和 framework service 调用。
- 优先审查 turn/event、tool/artifact、Task runtime adapter、bootstrap、plugin/memory generation 与 Tauri command composition。workspace、DomainProfile、review/worktree、文件权威和 UI/TUI/CLI/channel 投影继续留在应用层。
- 每个切片必须有 framework 独立门禁、CLI round-trip/contract tests，并在新路径真实可达后删除旧实现；不得以“以后删除”长期保留双 authority。
- Agent control 的 `list_events`/`list_subagent_runs` 当前仍先返回完整向量，adapter 只在截断前做 exact-target 过滤。真正下推 bounded query 属于 R1/P0，不能把当前 adapter 截断称为完成。

## R2/R3/G 应用交付边界

- R2 的 examples owner 在 `echo-agent`；CLI 仅验证 public facade 与应用文档链接，不把 EKO workspace/UI/DomainProfile 带入 framework examples。
- R3 的 EKO 正式文档维护在本目录 `architecture.md`、`features.md`、ADR 与相关 architecture 子目录；framework 双语 API 文档维护在 `echo-agent`；website manifest 等 child SHA 稳定后再更新。
- G 统一执行三仓适用门禁、fault matrix、10k/100k release 性能、10 分钟/1 小时/最终 2 小时 soak、人工 GUI、远端 CI 和 child-first push/release。任何阻塞、中断或未执行命令必须保留真实状态。

## 证据索引

- 应用/框架边界：[`../../docs/2026-08-28-current-framework-application-boundary-audit.md`](../../docs/2026-08-28-current-framework-application-boundary-audit.md)。
- examples inventory：[`../../docs/2026-08-28-framework-examples-inventory.md`](../../docs/2026-08-28-framework-examples-inventory.md)。
- 交互收敛路线：[`../../docs/2026-08-26-agent-interaction-convergence-plan.md`](../../docs/2026-08-26-agent-interaction-convergence-plan.md)。
- 应用架构与功能：[`architecture.md`](./architecture.md)、[`features.md`](./features.md)、[`persistence.md`](./persistence.md)。
- TaskRuntime bounded projection：[`ADR 0008`](./adr/0008-taskruntime-bounded-query-projections.md)；async/IPC boundary：[`ADR 0009`](./adr/0009-taskruntime-async-io-and-ipc-boundary.md)。
- F6 boot/inbox authority：[`ADR 0011`](./adr/0011-boot-inbox-recovery-authority.md)；extension authority：[`ADR 0012`](./adr/0012-extension-control-authority.md)。

## 文档与提交约束

本文不复制 framework 正式 API 说明，不把历史计划当执行授权。架构变更需在所属 child 建 ADR；跨仓先提交 framework，再提交 CLI，再同步 website，最后更新 superproject gitlink。所有提交显式关闭 GPG 签名：

```text
git -c commit.gpgsign=false commit -m "..."
```

提交前按 `AGENTS.md` 执行与改动匹配的 fmt、clippy、workspace tests、no-default/feature matrix、GUI/frontend 和 Markdown/link 检查。未执行的长时、性能、人工或远端门禁不得写成通过。
