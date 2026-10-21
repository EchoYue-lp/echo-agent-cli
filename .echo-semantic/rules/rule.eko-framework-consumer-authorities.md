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
observed_at: source:fa8844cd92e0841468a1ba360fe64f390b7a0c49927935ac89731cf51e642989
behavior_refs:
- behavior.background-review-caller-owned-settlement
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
code_refs:
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
- src/cli/channels/input_pump.rs
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
evidence_refs:
- evidence.latest-framework-consumer-integration
- evidence.cancellation-observation-consumer
finding_refs: [finding.cli-upstream-dependency-risks]
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

## 证据

EKO ADR 0043-0046 与框架 ADR 0058/0065/0069 是既有机制的设计依据；本次 CF988 的当前执行证据限于共用 Evidence 明确记录的三个 focused case 及独立复审。历史门禁不替代当前完整 workspace、Clippy/GUI/前端适用门禁，最终远端 CI 与主分支交付仍待完成。

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

EKO 的 ConfiguredModel/ModelProviderConfig 与既有 resolver 仍拥有产品选择、endpoint 和 credentials。主 Agent 与两类 Subagent 的已知 config/client 配对通过框架完整 builder 原样传入；框架唯一拥有 profile 准备与 client/config 同步安装，不添加应用 cfg cache、第二个 resolver 或构造时 async generation。无 config 的 opaque client 仍保留旧语义，运行时 model generation/critic/继承发布权威不改。此处仅记录既有模型构造合同的薄适配；原17反例和最终门禁尚需新 framework main 上的实际 AFTER，宽 needs_review/stale 状态不关闭。


2026-10-21 的显式新三项决定使当前应用policy只接受绑定d2e7/e46的精六项；原三项保持版本/字段/事件期限。原officialscanner1与独立精风险gate0必须分列，不可传播为framework/SDK例外，未知类别、身份/输入漂移和任意漏洞/yanked仍拒。现有Decision/Finding/Evidence各自保存人的范围、risk_accepted状态和真实扫描/工程限制；Rule仍needs_review，宽义务不因窄checkpoint关闭。
