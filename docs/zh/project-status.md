# EKO 应用当前状态（MASTER-PLAN）

> 本文是 `echo-agent-cli` 应用层的跨会话事实源，只记录 EKO 的权威路径、当前阶段状态、未完成工作和验收入口。
> framework 公共 API 与实现事实归 `echo-agent`；官网同步归 `echo-website`。历史实施日志保留在 Git 历史和带日期的专项文档中。
> Last updated: 2026-08-31。

## 产品不变量

- EKO 是本机个人助理，不套用线上多租户权限模型；CLI 不启用 SQLite。
- 产品模型是 `TaskRun -> PlanTask -> SubagentRun`。`TaskStatus` 是执行权威，Todo 只是只读 UI 投影，plan 是可编辑/可审阅 artifact。
- GUI、TUI、CLI/JSONL、channel、cron/background 共享 app-core 能力；transport/renderer 不拥有第二套运行状态。
- framework 提供通用 turn/event、Task DAG、Subagent、Tool、Store、MCP/LSP/channel 和 checkpoint 原语；EKO 负责 workspace、DomainProfile、文件/工件、review/worktree、pool、产品 policy 与 surface 投影。
- 所有取消、失败、部分副作用和恢复以持久事实为准；不得重复 terminal 或把不确定写入自动重放。内部执行角色统一使用 `Subagent` 术语。

## Child 基线

| 项目 | 当前 SHA | 状态 |
| --- | --- | --- |
| framework `echo-agent` | `6499d05` | typed delivery/config/usage/task/Subagent/LLM API、统一 `DeliveryTransition`、`LlmTimeouts`、三协议 SSE transport、直接 ReactAgentBuilder/RunStore/EvalRunner 合同与双语 framework 文档已提交。 |
| application `echo-agent-cli` | current typed ledger baseline | AgentRouter 直接使用已文档化的 typed framework ledger；旧 app projection/reducer 与 legacy wire/checkpoint codec 已删除。 |
| website `echo-website` | current reviewed projection | DeliveryLedger framework/application projection、legacy wire boundary ADR 与 source-aware docs manifest 按当前 reviewed CLI revision 同步。 |

F2-F5 合流、测试卫生和适用完整门禁证据见顶层 [`plan_03`](../../../docs/supreme/plans/2026-08-28T0013-项目未完成工作收敛/plan_03_F5收口完整验证主分支合并与资源清理.md)。本地 child SHA 不等于最终 release：顶层 gitlink 已在本地提交，人工桌面 GUI、远端 CI 和 push/release 仍未闭合。Framework 能力归属修订见顶层 [`framework capability placement audit`](../../../docs/2026-08-30-framework-capability-placement-audit.md)。

## 阶段状态

| 阶段 | 状态 | EKO 应用侧结论 |
| --- | --- | --- |
| F0 characterization | Complete | 已进入 `src/main.rs`，作为各入口回归基线。 |
| F1 receipt/admission | Complete | Persisted/Accepted/Drained/TurnSettled 已由 app-core 与 framework 合同承载。 |
| F2 Task/Plan/Todo authority | Complete | revisioned TaskRun graph、`TaskStatus` 和 Todo projection 已切换到唯一 authority；ADR [0015](./adr/0015-task-graph-status-authority.md)。 |
| F3 Agent control tools | Complete | `agent_list/inspect/message/followup/wait/interrupt` 是薄 adapter，复用 router、SubagentControl 和 TaskRuntime；bounded query 已下推。ADR [0016](./adr/0016-agent-control-tools.md)。 |
| F4 删除 InteractionMode | Complete | 生产代码、DTO、prompt、GUI/TUI/CLI/channel mode surface 已删除，不得改名恢复。 |
| F5 Agent/Subagent lifecycle | Complete | lifecycle characterization、生产修复、panic-lint hygiene 和适用完整门禁均已完成。 |
| F6 cursor/recovery/surface parity | Complete | Conversation/TaskSubagent cursor restart、cold address、workspace switch/delete、boot reconcile、terminal exactly-once 与五入口共享 fixture 已闭环；ADR [0017](./adr/0017-f6-cursor-recovery-surface-parity.md)。 |
| R0 boundary audit | Complete | 顶层审计覆盖 151 个 app-core Rust 文件，纯只读；见 [`current-framework-application-boundary-audit`](../../../docs/2026-08-28-current-framework-application-boundary-audit.md)。 |
| R1 framework-first migration | Complete | turn、TaskRuntime、artifact、bootstrap、diff、plugin、memory、tool-control、background 与测试/legacy/dead-shim 收敛已完成；无第二 authority。顶层 boundary audit 记录逐项 SHA。 |
| R2 examples convergence | Complete | `echo-agent-learning` 统一承载 43 个教学/组合 demos、13 个 Rust 学习章节和 21 个 executable contracts；panic/UTF-8/facade contracts 全绿。 |
| R3 docs/website | Complete / source sync current | framework 正式双语文档、learning 文档、website manifest/discovery/build 已同步；website manifest 按当前 reviewed CLI revision 绑定 framework `6cf8987`，source-aware check 已通过。 |
| R4 app-core global modularization | Complete | `state`、TaskRuntime store/executor、router/chat log/pool、extension/plugin/infra 已按 authority 物理拆分；`api` facade 已建立并迁移五 surface、examples 和 integration tests。Rust、frontend 与 website docs/discovery/site gates 已通过；代码提交为 `0e762ab`，framework 为 `125ea5f`。ADR [0025](./adr/0025-app-core-global-modularization.md)。 |
| Framework capability placement correction | Complete | 通用 primitive 不再以第二消费者作为准入门槛；首批 AgentPool/AgentRouter/ChatEventLog/Plugin/Extension 候选 disposition 见顶层 audit。 |
| AgentPool keyed admission kernel | Complete | framework `KeyedExecutionAdmission` 已承载 opaque-key lease、per-key process permit、retirement、close/wait；EKO 保留 Agent cache、capacity class、workspace、plugin/model/tool policy。 |
| AgentRouter generic delivery ledger contract | Typed API complete / development schema reset | framework `DeliveryLedger<Route, Payload>` 与 `DeliveryTransition` 已成为唯一 lifecycle/projection/retry authority；EKO 直接使用 `AgentAddress` 与 `AgentMessage`，文件布局、supervisor、live/cold runtime、wake、retirement 和产品 policy 仍由应用负责。 |
| 当前产品 schema 权威 | Complete | 只有 `learned-rules.md`、`.eko/workspace.json`、原样 cron prompt 与 `TaskRuntimeStore::new()` 定义当前产品输入。retired 文件原样保留但不解释；journal 重建与保守 Git 清理继续作为数据保护机制。ADR [0028](./adr/0028-current-product-schema-authority.md)。 |
| Framework-native Agent/model config | Complete | EKO 直接保存 framework `AgentSettings`；顶层 model 只含 `default_model_id`；`ConfiguredModel` 与 `ModelProviderConfig` 是唯一 model/connection 权威；统一 typed resolver 返回显式 selection error。ADR [0029](./adr/0029-framework-native-agent-and-model-config.md)。 |
| Subagent 提示词统一编译 | Complete | builtin/plugin 角色使用同一个 EKO compiler；compiled typed messages 是执行权威，有效工具/workspace 按 invocation 解析，结构化历史只过滤一次，TaskRuntime 复用 framework JSON framing。 |
| 确定性 CommandCell watch | Complete / static gates passed | framework `CommandCellWatcher` 是唯一 retained polling driver；EKO 保留 durable owner/Ready/delivery/ack policy。模型驱动的 `awaiter` Subagent 与 provider summary 已删除。ADR [0031](./adr/0031-deterministic-command-cell-watch.md)。 |
| Enabled Skill 运行时权威 | Complete / 简化重构 | `enabled-skills.json` 仍是 bundled Skill 的注册权威（0032 核心立场保留）；durable 结算状态机（generation CAS、operation identity 去重、repair debt 重放）已整体移除，五个变更路径改为直写直同步，坏配置回退默认集（fail-open），framework `activate_skill` 对未注册 skill 显式报错。ADR [0032](./adr/0032-enabled-skill-runtime-authority.md)（部分取代）、[0036](./adr/0036-skill-policy-simplification.md)。 |
| Skill catalog 收缩与官方标准化 | Complete / merge pending | 捆绑 `SKILL.md` 只用 agentskills.io 官方字段（无任何私有扩展命名空间），`allowed-tools` 为空格分隔字符串，Skill 文件不携带私有 Hook sidecar，LLM routing 为 description-driven；catalog 2026-09 再次收敛 39 → 24（删 4 个通用能力型与 11 个 vendored Anthropic 示例，默认启用 8 → 5、methodology baseline 常驻注入 4 → 1 仅 verification-before-completion）；builtin 根改为运行时解析（`$EKO_SKILLS_ROOT` → Tauri resource dir → 源码树），TUI 新增 `/skill`、GUI 新增 Activate 按钮实现用户可调用；install 识别 Agent Plugins 1.0 包（仅 skills 面）。ADR [0033](./adr/0033-skill-catalog-contraction-and-official-frontmatter.md)、[0036](./adr/0036-skill-policy-simplification.md)。 |
| 统一 turn-run 绑定 | Feature branch / final gates pending | 每个 store-backed turn 急切绑定 TaskRun；typed provenance 保护 planless direct run，boot/quiet-wake 原子结算，五入口 user steer 使用 exact run binding，无 plan conversation run 不污染任务 UI。ADR [0037](./adr/0037-unified-turn-run-binding.md)。 |
| G Final Integration/Release | Conditional / current Phase 3 gates pending | 历史版本的 10k/100k、600s、one-hour/final-two-hour、CLI/framework/GUI/website 门禁仍绑定旧 SHA；当前 framework typed deferral 与 AgentRouter focused checks 已通过，但完整 workspace、frontend、GUI、soak、人工 GUI、远端 CI 和 release 尚未重新验证。 |

## 当前权威路径

| 语义 | 唯一应用入口 |
| --- | --- |
| bootstrap/config/pool | `echo-agent-app-core/src/runtime.rs`、`infra/`、`agent_pool/` |
| AgentPool execution admission | framework `echo_agent::agent::admission::KeyedExecutionAdmission`；EKO `AgentPoolAdmission` 只做错误与 receipt 转换 |
| workspace host/resource | `echo-agent-app-core/src/workspace/runtime.rs` 与 `state/` |
| chat turn/event projection | `chat_driver.rs`、`foreground_turn.rs`、`chat_event_log/` |
| Conversation routing/inbox | `agent_router/` -> `echo_agent::delivery::DeliveryLedger<..., AgentAddress, AgentMessage>`；不在 surface 解析地址或自建 mailbox |
| Subagent outcome/evidence | framework `SubagentOutcome` 直接持久化并渲染；EKO 只在同一值外围增加 task identity 与 review metadata，不再定义重复结果 DTO |
| Agent control | `agent_control.rs`；只做 discriminator、revision、attempt、generation 校验和既有 service 调用 |
| TaskRun graph/status | `tasks/task_runtime/`、`store/`、`run_authority.rs`；framework `RuntimeDagExecutor` 是 DAG 内核 |
| Task/Subagent control | `tasks/task_runtime/subagent_control.rs`；attempt-scoped guidance/interrupt/receipt |
| boot recovery/admission | store-scoped reconciler、foreground owner、BackgroundTaskService 和 extension control split |
| shared app services | `AppState`、`TaskRuntimeOperation`、`ProductDataIoService`；GUI/headless 不得各建 authority；surface imports 走 `api/` |
| GUI IPC/projection | `src/tauri/commands/` 与 `web-frontend/src/`，只传 DTO、事件和 typed receipts |

## F6 验收结论

F6 已由 app-core 的 executable fixtures 证明：跨重启 cursor 可恢复，cold/unloaded address 与
workspace generation/delete fail closed，boot reconciliation 不遗留 receipt/handle，同一 canonical
fixture 在 GUI、TUI、CLI、JSONL、channel 不产生 surface-local terminal 推断。

## R1 收敛结论

- Framework 持有 canonical turn receipt、typed artifact、immutable prepared plugin generation、Task
  timeout/dependency 原语；EKO 只保留 workspace、pool、review/worktree、文件权威和 surface policy。
- `ApplicationServices` 是 GUI/headless/soak 的统一 composition owner；Tauri diff/workflow/tool commands
  均为薄 product integration。
- Plugin 与 hot-memory 都按 generation 一次 prepare、一次 publication，并覆盖 primary、existing pool
  与 future pool；旧 additive refresh、legacy importer、dead shims 与字符串扫描合同已删除。

## R3/G 应用交付边界

- R3 的 EKO 正式文档维护在本目录 `zh/` 与 `en/` 镜像树；framework 双语 API 文档维护在 `echo-agent`；website manifest 等 child SHA 稳定后再更新。
- G 统一执行三仓适用门禁、fault matrix、10k/100k release 性能、10 分钟 deterministic soak、1 小时 real-product acceptance、最终 2 小时 real-product acceptance、人工 GUI、远端 CI 和 child-first push/release。`lh6_product_soak` 的一小时门必须显式使用 `--acceptance-tier one-hour` 且不少于 3600 秒；未指定 tier 的非 probe 仍是 `final-two-hour` 且不少于 7200 秒。`--probe` 只产生 `probe_passed`，不得作为 acceptance。任何阻塞、中断或未执行命令必须保留真实状态。

## 证据索引

- 应用/框架边界：[`current-framework-application-boundary-audit`](../../../docs/2026-08-28-current-framework-application-boundary-audit.md)。
- examples inventory：[`framework-examples-inventory`](../../../docs/2026-08-28-framework-examples-inventory.md)。
- 交互收敛路线：[`agent-interaction-convergence-plan`](../../../docs/2026-08-26-agent-interaction-convergence-plan.md)。
- 应用架构与功能：[`architecture overview`](./architecture/overview.md)、[`features`](./features.md)、[`persistence`](./architecture/persistence.md)。
- TaskRuntime bounded projection：[`ADR 0008`](./adr/0008-taskruntime-bounded-query-projections.md)；async/IPC boundary：[`ADR 0009`](./adr/0009-taskruntime-async-io-and-ipc-boundary.md)。
- F6 boot/inbox authority：[`ADR 0011`](./adr/0011-boot-inbox-recovery-authority.md)；extension authority：[`ADR 0012`](./adr/0012-extension-control-authority.md)。

## 文档与提交约束

本文不复制 framework 正式 API 说明，不把历史计划当执行授权。架构变更需在所属 child 建 ADR；跨仓先提交 framework，再提交 CLI，再同步 website，最后更新 superproject gitlink。所有提交显式关闭 GPG 签名：

```text
git -c commit.gpgsign=false commit -m "..."
```

提交前按 `AGENTS.md` 执行与改动匹配的 fmt、clippy、workspace tests、no-default/feature matrix、GUI/frontend 和 Markdown/link 检查。未执行的长时、性能、人工或远端门禁不得写成通过。
