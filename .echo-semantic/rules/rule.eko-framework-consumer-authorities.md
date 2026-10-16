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
observed_at: source:b1f3c278483f857325923b3cd1a00b06fb514c40e2dedd6e7d39aa356a058b97
behavior_refs:
- behavior.background-review-caller-owned-settlement
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
code_refs:
- echo-agent-app-core/src/state/config.rs
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
evidence_refs:
- evidence.latest-framework-consumer-integration
- evidence.cancellation-observation-consumer
finding_refs: []
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

EKO ADR 0043-0046、框架 ADR 0058/0065/0069、定向故障注入、全 workspace 测试与 Clippy/GUI/前端门禁构成当前源码证据。独立 Review 已通过；CLI 最终远端 CI 与主分支交付仍待完成。

## 裁决记录

框架与应用分层沿仓库 AGENTS 的强制规则：跨产品通用机制在框架，本地 EKO 策略和 UI/文件工作区收据在应用；项目开发期不保留旧 API 兼容。

## B 消费新增不变量

Framework physical receipt/claim CAS唯一；观察不终态化。原Task/analysis/foreground保owner与effect前durable intent；未知拒重复，真实terminal IO债务只同identity重写清理。aggregate普通工作必须真实drain、未知原owner可达、无Strong自环。新增范围尚需当前强tests/aggregate实现，Rule整体needs_review，不撤销旧历史验证也不将旧证据推广到B。


## Scheduler 原 slot 观察

同一应用代际的原SchedulerHandle只在同Mutex内借用shutdown；Err或借用等待取消保slot、onlyOk清。原Root每次poll复观并保unknown依赖，不以loop.is_finished/空slot/旧normal_drained推物理结算。此源与两具名case有界通过，不闭合Rule其余B义务及最终消费者门禁。
