---
schema_version: 1
id: rule.eko-framework-consumer-authorities
kind: rule
status: needs_review
expectation: human_confirmed
risk: high
primary_focus: state_authority
focus:
- data_durability
- time_lifecycle
- failure_concurrency
- contract_evidence
observed_at: 9bcc1fc052326acc13b4223e64a51fcc867f1ccf
behavior_refs:
- behavior.background-review-caller-owned-settlement
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
code_refs:
- web-frontend/src/stores/taskRuntimeStore.test.ts
- web-frontend/src/components/task/TaskRuntimePanel.css
- web-frontend/src/components/layout/ContextPane.tsx
- echo-agent-app-core/src/tasks/task_runtime/file_shadow.rs
- echo-agent-app-core/src/tasks/task_runtime/file_store.rs
- echo-agent-app-core/src/foreground_turn.rs
- echo-agent-app-core/src/hitl/pairing.rs
- echo-agent-app-core/src/hitl/dispatcher.rs
- echo-agent-app-core/src/hitl/channel_provider.rs
- echo-agent-app-core/src/hitl/mod.rs
- src/tauri/commands/chat.rs
- src/cli/jsonl.rs
- web-frontend/src/types/api.ts
- web-frontend/src/hooks/useTauriChat.ts
- web-frontend/src/hooks/chatEventHandler.ts
- web-frontend/src/stores/chatStore.ts
- web-frontend/src/components/chat/ChatPanel.tsx
- web-frontend/src/components/chat/HitlDialog.tsx
- web-frontend/src/components/task/TaskRuntimeList.tsx
- echo-agent-app-core/src/infra/factory.rs
- echo-agent-app-core/src/agent_pool/tests.rs
- echo-agent-app-core/src/state/reliability_contracts.rs
- echo-agent-app-core/src/tasks/task_runtime/store/journal.rs
- echo-agent-app-core/src/tasks/service.rs
- echo-agent-app-core/src/state/config.rs
- echo-agent-app-core/src/state/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/run.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/tests.rs
- src/cli/repl.rs
- src/cli/channels.rs
- echo-agent-app-core/src/channel_runtime/input_pump.rs
- src/tauri/commands/task_runtime.rs
- src/tui/events.rs
- echo-agent-app-core/src/agent_router/tests.rs
- echo-agent-app-core/src/evolution/background_review_owner.rs
- echo-agent-app-core/src/evolution/review_integration.rs
- echo-agent-app-core/src/conversation_archive.rs
- echo-agent-app-core/src/conversation_deletion.rs
- echo-agent-app-core/src/managed_conversation.rs
- echo-agent-app-core/src/state/app_state.rs
- echo-agent-app-core/src/state/workspace.rs
- echo-agent-app-core/src/extension_control/policy.rs
- echo-agent-app-core/src/plugin_runtime/runtime.rs
- echo-agent-app-core/src/workspace/runtime.rs
- echo-agent-app-core/src/agent_pool/admission.rs
- echo-agent-app-core/src/agent_pool/pool.rs
- echo-agent-app-core/src/analysis.rs
- echo-agent-app-core/src/chat_driver.rs
- echo-agent-app-core/src/chat_resources.rs
- echo-agent-app-core/src/subagent_policy.rs
- echo-agent-app-core/src/conversation_input.rs
- echo-agent-app-core/src/foreground_turn.rs
- echo-agent-app-core/src/product_data_io.rs
- echo-agent-app-core/src/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/continuation.rs
- echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
- echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
- echo-agent-app-core/src/plugin_runtime/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs
- echo-agent-app-core/src/api/mod.rs
- echo-agent-app-core/src/chat_event_log/projection.rs
- echo-agent-app-core/src/chat_event_log/tests.rs
- echo-agent-app-core/src/config.rs
- echo-agent-app-core/src/config_watcher.rs
- echo-agent-app-core/src/infra/diagnostics.rs
- echo-agent-app-core/src/lib.rs
- echo-agent-app-core/src/scheduler/mod.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/events.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/review.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/unattended.rs
- echo-agent-app-core/src/tasks/task_runtime/task_tools.rs
- echo-agent-app-core/src/auto_memory/mod.rs
- src/tauri/commands/panels.rs
- echo-agent-app-core/src/tasks/task_runtime/types.rs
- echo-agent-app-core/src/types/request.rs
- echo-agent-app-core/src/types/response.rs
- examples/lh6_product_soak.rs
- src/cli/cmd_impls/agent_router.rs
- src/cli/cmd_impls/analysis.rs
- src/cli/cmd_impls/research.rs
- src/cli/modes.rs
- src/main.rs
- src/task_run_control.rs
- src/tauri/commands/config.rs
- src/tauri/commands/mod.rs
- src/tauri/commands/scheduler.rs
- src/tauri/desktop.rs
- src/tauri/mod.rs
- src/tauri/state.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/config/ConfigPanel.tsx
- web-frontend/src/components/layout/SettingsDialog.tsx
- web-frontend/src/components/scheduler/SchedulerPanel.tsx
- web-frontend/src/hooks/chatEventHandler.contract.test.ts
- web-frontend/src/hooks/chatEventHandler.test.ts
- web-frontend/src/hooks/useTauriChat.test.tsx
- web-frontend/src/stores/uiStore.ts
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/channel_runtime/management.rs
- echo-agent-app-core/src/channel_runtime/outbound.rs
- echo-agent-app-core/src/channel_runtime/tool_render.rs
- echo-agent-app-core/src/hitl/channel_entry.rs
- echo-agent-app-core/src/scheduler/management.rs
- echo-agent-app-core/src/text_commands.rs
- echo-agent-app-core/src/text_commands/agent_router.rs
- echo-agent-app-core/src/text_commands/analysis.rs
- echo-agent-app-core/src/text_commands/research.rs
- echo-agent-app-core/src/text_commands/task_run_control.rs
- src/tauri/commands/channels.rs
- web-frontend/src/components/channels/ChannelsPanel.test.tsx
- web-frontend/src/components/channels/ChannelsPanel.tsx
- web-frontend/src/components/config/ConfigPanel.test.tsx
- web-frontend/src/components/scheduler/SchedulerHistory.test.tsx
- web-frontend/src/components/scheduler/SchedulerHistory.tsx
- web-frontend/src/components/scheduler/SchedulerPanel.test.tsx
evidence_refs:
- evidence.gui-dialog-presentation
- evidence.latest-framework-consumer-integration
- evidence.cancellation-observation-consumer
- evidence.eko-native-measurement-source-basis
finding_refs: [finding.cli-upstream-dependency-risks, finding.eko-channel-helper-relocation]
---

# EKO framework consumer authorities

## 不变量或唯一权威

框架 Memory journal、SkillMutationAuthority、Managed transcript CAS 和 Plugin publication target 分别拥有通用资源变更；EKO Review receipt、Review Inbox、聚合删除 tombstone、跨工作区 handoff 意图与产品策略不取代这些权威。

## 适用行为

适用于 Background Review 的准入与恢复、Skill 草稿/发布、Plugin 故障关闭、Managed conversation 创建/删除、workspace 退休和各 surface 适配。

## 当前实现

EKO 使用同一 generation-bound Review owner；Skill 使用框架 preview/approval/apply；Managed delete 保留稳定 epoch 请求，未完成 handoff 对两端施加持久准入 fence；Plugin Error diagnostic 整代拒绝，激活失败发布新空代际；旧 Agent 工具与 shutdown future 不形成强引用环。

## 期望行为

任何适配器不能私自拥有第二套 Memory/Skill/Plugin 状态机。失败结算必须报告 unknown、冲突或清理债务，不能把未确认效果包装成成功；重试只用稳定身份，不把新 generation 误认为旧操作。

### Plan07 HITL identity source checkpoint

ADR0053 owns EKO's per-delivery response_target/scope/kind. The original provider future, dispatcher, GUI pending reservation and channel FIFO remain their respective owners; no framework field, third queue, store or terminal authority is added. Providers read the original ForegroundTurnControl and ChannelActiveTurn through Weak observers at the actual request, preserving current continuation turn across framework spawn without a new store or Arc cycle. Missing original owners reject before pending insertion. Original pump leases retain the original active-map removal receipt. Input/Selection/Approval payload constraints are checked under the original pending-map lock before removal, and bare /cancel only returns the target-qualified hint. Direct GUI/resume/browser providers pair inside request; the paired dispatcher entry pairs before cloning. Generic-only legacy registration remains unpaired. The channel target is consumed before ordinary ingress, and a reused request_id cannot consume a successor's waiting item. The journal event's absent optional identity remains absent on serialization; historical requests are never reactivated from it. Frontend state is a projection and replies use the delivered identity rather than current page refs.

This is a narrow source checkpoint on EKO d7/F634a, not completion of Plan07 or renewal of the historical qualifications below. Node before/after and Rust test definitions are separate evidence: Rust tests/export, native controls, live GUI+IM composition, shared ChannelManager/Cron GUI/lifecycle, fresh exact risk gate and main CI remain pending. Rule status stays needs_review; old dependency-risk fingerprints and global closure are not extended.

### EKO Subagent depth policy source slice

ADR0052 (`docs/zh/adr/0052-gui-subagent-depth-policy.md` / `docs/en/adr/0052-gui-subagent-depth-policy.md`) owns EKO's default1/selectable1..3 product policy. `subagent_policy.rs` validates that product value; the shared committed-config projection only supplies new admissions through `chat_resources.rs` or the original TaskRuntime store. The original `TaskRunExecutionProfile` in RunCreated, fold, checkpoint and resume identity remains the sole durable binding. Same-run task insertion, retries and recovery retain it; historical absence is not filled from current settings. Framework child-policy and invocation hard-W remain the generic enforcement authority, including primary Verification's original nonempty PlanTask tool bound. No new store, executor, task CRUD or runtime state machine is introduced.

This is a source-stage observation, not an extension of the historical verified cuts below. Node tests, lint/build and bilingual document parity have actual local results; Rust tests, Rust type export, native GUI and composed-framework qualification remain pending. The original frozen depth source paired e4e and used immutable e713 only as its hard-W field reference; its framework-fix qualification remained pending at that historical cut. Rule status stays needs_review; no old dependency-risk consumer fingerprint, native acceptance or global closure is renewed here.

### Independent application composition source

The new gui-runtime-app-integration source composes these two immutable slices by three-way hunks on SSH28bb and consumes stable F0a181571. Its original v3/depth/SSH receipts remain bound to their original cuts. This application composition has not run Rust, actual exports, native GUI, live dual GUI/IM or fresh risk qualification. The original ConfigState/ChannelManager/SchedulerRunner/TaskRuntime and foreground leases remain the only authorities.

## 证据

EKO 与framework既有ADR继续约束唯一状态权威。当前消费者的完整本地资格见共用Evidence的2026-10-22最终framework main切点：原八项门禁、included factory格式与既有三项风险矩阵全部实际通过。历史CF988及其它focused切点只保原范围；精确合并main的CI/cold单列交付，GUIfeature测试不等于原生窗口/OS验收，Rule整体needs_review不因本次消费者资格升级。

## 裁决记录

框架与应用分层沿仓库 AGENTS 的强制规则：跨产品通用机制在框架，本地 EKO 策略和 UI/文件工作区收据在应用；项目开发期不保留旧 API 兼容。

## B 消费新增不变量

Framework physical receipt/claim CAS唯一；观察不终态化。原Task/analysis/foreground保owner与effect前durable intent；未知拒重复，真实terminal IO债务只同identity重写清理。aggregate普通工作必须真实drain、未知原owner可达、无Strong自环。新增范围尚需当前强tests/aggregate实现，Rule整体needs_review，不撤销旧历史验证也不将旧证据推广到B。


## Scheduler 原 slot 观察

同一应用代际的原SchedulerHandle只在同Mutex内借用shutdown；Err或借用等待取消保slot、onlyOk清。原Root每次poll复观并保unknown依赖，不以loop.is_finished/空slot/旧normal_drained推物理结算。此源与两具名case有界通过，不闭合Rule其余B义务及最终消费者门禁。


## CF988 定向实施补充

共享chat的准备与claim也保既有typed StoreError至ChatTurnError，RecoveryBlocked不能先String化再进入foreground普通Failed；仅普通错误加原context。真实journal重开→同共享入口的before业务fail/ordinary pass及same after两例通过和源码增量独审，只闭此遗漏，不替当前全门禁。三依赖Rust输入变动先拒绝，再经明确授权、独立无消费扩大重评及same AFTER后精确重绑，不能自动继承旧风险接受。

CLI依赖政策只在应用层核对用户当前精确risk_accepted集合；原官方scanner、canonical framework origin helper仍唯一权威。policy绑定当前完整锁及消费者输入，任何新告警/漏洞/yanked/扫描错误或输入漂移均拒绝。原scanner失败和单独风险核对成功必须分别报告，不传播为framework/SDK豁免；全部Rust与GUI门禁保持。

后继primary切片：同一ForegroundTurnSettlementWaiter只对真实Observation保留run/execution，channel notifier传typed错误后不kick；GUI launch准备和执行等待共享原foreground错误结算，观察仅retain，普通错误保持Failed。两个独立行为red→samecase after及4普通/正常控制共6pass，17file独审/非语义增量确认和Clippy零warning通过；完整门禁/宽Rule仍未闭。三依赖精确风险接受由具名Finding/人的Decision引用拥有，不推广为SDK/framework例外或放宽工程门禁。

合法 unresolved physical-owner intent 是精确 run 的待观察事实，批次恢复在原 run 锁内跳过其状态/cell/control 写入并继续无关 run；真实存储/畸形证据错误仍报错。Running/Paused/guidance 共享原证据与精确 ack，不新增恢复器或权威。canonical compose 先绑定原三个 Weak 子组件，最终强 Arc 接管复用同一 helper。existing-run continuation 保留 canonical domain/route/attended/Goal/profile，仍用入口实际 active workspace/conversation/root 经原 immutable validator；trace 用返回原值。三 focused 实际通过及独立复审只支持此有限 checkpoint，不能替完整矩阵、新 cron、依赖政策或 main CI。


## Configured LLM construction adapter

EKO 的 ConfiguredModel/ModelProviderConfig 与既有 resolver 仍拥有产品选择、endpoint 和 credentials。主 Agent 与两类 Subagent 的已知 config/client 配对通过框架完整 builder 原样传入；框架唯一拥有 profile 准备与 client/config 同步安装，不添加应用 cfg cache、第二个 resolver 或构造时 async generation。无 config 的 opaque client 仍保留旧语义，运行时 model generation/critic/继承发布权威不改。此处仅记录既有模型构造合同的薄适配；原17反例已在接受的framework f7bac/newlock完整workspace中具名通过；永久保留known-pair/opaque分界，宽needs_review/stale状态不关闭。


2026-10-21 的显式新三项决定使当前应用policy只接受绑定d2e7/e46的精六项；原三项保持版本/字段/事件期限。原officialscanner1与独立精风险gate0必须分列，不可传播为framework/SDK例外，未知类别、身份/输入漂移和任意漏洞/yanked仍拒。现有Decision/Finding/Evidence各自保存人的范围、risk_accepted状态和真实扫描/工程限制；Rule仍needs_review，宽义务不因窄checkpoint关闭。

## 2026-10-25 应用源码归位观察

当前可读 channel helper 路径随原实现归位 app-core/channel_runtime；原 bin 入口只转发。此路径修正不续期本文件任何历史 Rust、Node、风险接受或 native 资格；历史观察和具名旧快照结论保持原范围。当前合流尚未 Cargo 编译。

旧来源观察 `source:d764d62ffe779ae5fe2b6e6627158291821c4258e700cb6301fb29ade46bc7aa` 保留为历史。当前 observed_at 只绑定归位后的可读来源，不表示旧验证在新合流上续期。

## Finite task-history read adaptation

Original EKO TaskRuntime journals, scope captures and typed controls remain authoritative. The GUI inventory and finite event IPC reuse their read path and the original formal-task visibility predicate; page continuation and selection are read projections, not a state authority. Candidate damage must be explicit, and a finite page or accepted control is not complete collection/physical settlement proof. Task-list source and current evidence are scoped separately from older channel, cancellation and mainline qualifications.

## 2026-10-27 主目录 focused 后继结算

本次源码观察绑定可恢复的 CLI 提交 `9bcc1fc052326acc13b4223e64a51fcc867f1ccf`，Framework 为 `69296a4dea1047be7fb91b7ab06a839ee57dda7e`。原 observed_at `source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46` 保留为历史来源标识；更新观察只修复当前引用的可恢复性，不重标上述历史源码、风险接受或执行资格。Rule 仍为高风险 `needs_review`，原 Finding、状态权威与失败语义不变。

epoch15 在原 TaskRuntime、输入 journal、tracked mailbox 与执行 owner 上完成定向验证：含 channels 配置实际68项、无 channels 配置37项均零失败、零忽略、零编译 warning；GUI caller 和无 channels 生产检查最终自然退出0且零 warning。直接支持 GUI 分页/控制的 Rust 范围仅 `task_history_` 五项与 `task_control_generation_` 三项，不把其余渠道测试合称 GUI 验收。原两项 GUI/IM 投递前持久确认的行为 RED、编译/fixture 错误、19 warning、后继残余 import warning 与早先零 warning 误报撤销都保留，后继绿色不改写原结果。

Root 在同一前端来源取得75文件/580测试、TypeScript 与 Tauri 模式 Vite build、全 ESLint 和格式检查自然0；回执绑定465个输入，不借本次语义文档更新重跑或续期其它产品验收。Rust 制品和日志已在 target 外保全，准确清理自然0，原 target 不存在。上述有限结算见共用 Evidence 的本日补充，不提升宽 Rule 或全项目 closure。

## 2026-10-28 有界原生、历史与测量consumer源码依据

本节只把14个当前consumer路径纳入本Rule已有权威/合同边界，不新增Map、Behavior、运行
owner或统一执行器。既有渠道资产仍只表示其真实channel owner，不扩成输入资源、UI或
性能工具的万能owner。Rule保持高风险needs_review，原四Behavior及旧动态/删除聚合缺口不清。

新增路径由`evidence.eko-native-measurement-source-basis`的独立当前source切点支撑。
本Rule旧observed_at继续表示其历史观察，不证明这些新文件当时已存在；该新source观察不
续期旧运行结果。若正常来源合同拒绝该组合，保留真实失败并由原writer正常绑定，不改工具或
schema/exclude，不填造source/after revision。

### 原生与历史consumer

消息/历史投影从原Framework Message、ChatEventLog或TaskRuntime journal派生，块phase与
execution observation不替代physical receipt或TaskRun/Goal状态。UI只呈现原envelope、原
scope/owner与序号；缺失/冲突不能从当前页面、工具名或普通文本补身份/成功。

PreparedUserTurn的资源沿原producer和ContentPart::ResourceLink/meta传递；Fork复制消费
已有typed关联并核原reader、bytes/SHA与scope，字符范围/哈希只定位正文，不独立授来源。
旧纯文字或未知链接不扫描升级为可信资源。legacy getter当前保持Unknown，不借今天factory/
registry或旧参数签发过去的Builtin来源，pending继续原restore拒绝。新增source依据不把
00旧API回归接入、source审查或原投影代码称完整retention/capture已修或运行通过。

### 测量证据consumer

测量工具只消费既有Store/Journal/TaskRuntime/工具与GUI权威。synthetic workspace的锁、
目录身份和字节清单仅保护测量输入，不成为业务owner。样本绑定source/lock、实际Cargo
argv/features/profile、binary、dataset、protocol及进程身份；单位、durability、工具集合或
条件不同不能直接归为优化收益。失败/拒绝/预览与成功分组，watchdog不是产品SLA；进程
CPU/RSS/block-operation计数不是单操作资源或物理字节吞吐。有效但不完整报告保持不完整，
不能从脚本、测试定义、一次样本或旧绿色推完整测量通过。原Plan08协议和证据读取规则仍属
其本来范围，本节不新建性能执行owner或接受新风险。
