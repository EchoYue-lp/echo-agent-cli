---
schema_version: 1
id: map.eko-framework-consumer-settlement
kind: capability_map
title: EKO framework consumer settlement
risk: high
observed_at: source:bd75b49f18a60ada507be1bb1609d9a9e4e52b6f4def9101a859d6f66cc231aa
boundary_refs:
- boundary.eko-framework-consumer-settlement
behavior_refs:
- behavior.background-review-caller-owned-settlement
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
rule_refs:
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.latest-framework-consumer-integration
- evidence.background-review-entry-repair
- evidence.background-review-entry-verification
- evidence.cancellation-observation-consumer
finding_refs:
- finding.background-review-entry-settlement
- finding.cli-upstream-dependency-risks
audit_refs:
- audit.background-review-entry-independent-rereview
- audit.cancellation-observation-time-lifecycle
- audit.cancellation-observation-data-durability
- audit.cancellation-observation-failure-concurrency
related_map_refs:
- map.eko-conversation-collaboration
scenarios:
  independent-linux-validation-budgets:
    status: mapped
    source_refs:
    - .github/workflows/rust-ci.yml
    evidence_refs:
    - evidence.linux-validation-budgets
  compression-product-policy:
    status: mapped
    source_refs:
    - echo-agent-app-core/src/config.rs
    - echo-agent-app-core/src/manual_compression.rs
    - echo-agent-app-core/src/tasks/task_runtime/compact_context.rs
    - src/cli/cmd_impls/context.rs
    - src/cli/channels.rs
    - src/tui/commands.rs
    - src/tui/events.rs
    - src/tauri/commands/panels.rs
    - web-frontend/src/api/endpoints.ts
    - web-frontend/src/types/api.ts
    - web-frontend/src/components/compress/CompressPanel.tsx
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.compression-product-policy
  background-review-admission-and-recovery:
    status: mapped
    source_refs:
    - echo-agent-app-core/src/evolution/background_review_owner.rs
    - echo-agent-app-core/src/evolution/review_integration.rs
    - echo-agent-app-core/src/runtime.rs
    - echo-agent-app-core/src/workspace/runtime.rs
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.latest-framework-consumer-integration
    - evidence.background-review-entry-repair
    - evidence.background-review-entry-verification
  review-surface-result:
    status: mapped
    source_refs:
    - src/tauri/commands/panels.rs
    - src/cli/cmd_impls/evolution.rs
    - src/tui/events.rs
    - web-frontend/src/api/endpoints.ts
    - web-frontend/src/components/evolution/EvolutionPanel.tsx
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.latest-framework-consumer-integration
  skill-and-plugin-lifecycle:
    status: mapped
    source_refs:
    - echo-agent-app-core/src/evolution/review_integration.rs
    - echo-agent-app-core/src/extension_control/policy.rs
    - echo-agent-app-core/src/plugin_runtime/runtime.rs
    - echo-agent-app-core/src/agent_pool/generation.rs
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.latest-framework-consumer-integration
  managed-conversation-and-workspace-retirement:
    status: mapped
    source_refs:
    - echo-agent-app-core/src/conversation_archive.rs
    - echo-agent-app-core/src/conversation_deletion.rs
    - echo-agent-app-core/src/managed_conversation.rs
    - echo-agent-app-core/src/state/app_state.rs
    - echo-agent-app-core/src/state/workspace.rs
    - echo-agent-app-core/src/workspace/runtime.rs
    - echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs
    - src/cli/channels.rs
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.latest-framework-consumer-integration
  scheduler-and-task-claim-adaptation:
    status: needs_review
    source_refs:
    - echo-agent-app-core/src/state/config.rs
    - echo-agent-app-core/src/runtime.rs
    - echo-agent-app-core/src/state/app_state.rs
    - echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
    - echo-agent-app-core/src/tasks/task_runtime/run_authority.rs
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.latest-framework-consumer-integration
    - evidence.cancellation-observation-consumer
    unknown: 原Handle/Root两case及随后原九具名strong在public3ded已实际通过、零warning；cron E2E、其它B义务和最终Fmain消费者仍待，不将有限结果推广整个场景。
    next_step: 保原sameowner；完成已列B强例、最终公开依赖及适用合前门禁。
  shared-turn-observation-retains-owner:
    status: needs_review
    source_refs:
    - echo-agent-app-core/src/chat_driver.rs
    - echo-agent-app-core/src/foreground_turn.rs
    - echo-agent-app-core/src/conversation_input.rs
    - echo-agent-app-core/src/tasks/task_runtime/continuation.rs
    behavior_refs:
    - behavior.shared-turn-observation-retains-owner
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.cancellation-observation-consumer
    unknown: 历史f0a结果保留；public3ded/CLI2203随后原九具名strong已9pass/0fail/0ignore、零warning，另原Handle/Root两例已过；其它未覆盖B义务、TUI/GUI、最终Fmain与CLI
      cron E2E未验。
    next_step: 保留原九有限fixture边界，完成其余适用B验证、CLI cron E2E和最终公开F消费者，再执行正式门禁。
  task-unknown-owner-admission-fence:
    status: needs_review
    source_refs:
    - echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
    - echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
    - echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
    - echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
    - echo-agent-app-core/src/scheduler/runner.rs
    behavior_refs:
    - behavior.task-unknown-owner-admission-fence
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.cancellation-observation-consumer
    unknown: 历史f0a结果保留；public3ded/CLI2203随后原九具名strong已9pass/0fail/0ignore、零warning，另原Handle/Root两例已过；其它未覆盖B义务、TUI/GUI、最终Fmain与CLI
      cron E2E未验。
    next_step: 保留原九有限fixture边界，完成其余适用B验证、CLI cron E2E和最终公开F消费者，再执行正式门禁。
  analysis-original-owner-settlement-retry:
    status: needs_review
    source_refs:
    - echo-agent-app-core/src/analysis.rs
    - echo-agent-app-core/src/product_data_io.rs
    behavior_refs:
    - behavior.analysis-original-owner-settlement-retry
    rule_refs:
    - rule.eko-framework-consumer-authorities
    evidence_refs:
    - evidence.cancellation-observation-consumer
    unknown: 历史f0a结果保留；public3ded/CLI2203随后原九具名strong已9pass/0fail/0ignore、零warning，另原Handle/Root两例已过；其它未覆盖B义务、TUI/GUI、最终Fmain与CLI
      cron E2E未验。
    next_step: 保留原九有限fixture边界，完成其余适用B验证、CLI cron E2E和最终公开F消费者，再执行正式门禁。
---

# EKO framework consumer settlement

## 能力范围

覆盖最新 echo-agent 下 EKO Background Review、Skill/Plugin、Managed conversation、workspace 退休、Scheduler 与 Task claim 的消费者结算边界；Side Conversation 仍由既有 GUI-only 能力图负责。

## 入口与输出

GUI、TUI、CLI 共享普通 Review 的 app-core owner；GUI/Tauri 额外投影 Review operation ID 和只读收据。其它入口保持既有普通 conversation/Task/Plugin 行为，不增加 Side Conversation 专用合同。

## 行为关系

框架保留通用惰性 Review、Memory/Skill journal、Managed transcript 与 Plugin 代际 CAS；EKO 保留 workspace generation、产品审批、Inbox、聚合删除、surface 投影和 shutdown 编排。

## 状态与数据流

Review 先记录准入和 Memory 基线，再 poll 惰性 handle；先持久化 outcome，再更新 Inbox，最后记录 terminal。Managed 删除复用既有 tombstone 保存 exact epoch 请求；跨 workspace handoff 用既有 archive 文件保存意图与完成回执，并以两端身份锁和持久准入 fence 保护恢复。Skill 写入只通过 framework mutation authority。

## 策略来源与优先级

顶层及 CLI AGENTS、EKO ADR 0043-0046、框架 ADR 0058/0065/0069 和当前框架源码共同确定分层。EKO 对 Plugin Error diagnostic 采用比框架更严格的整代拒绝；开发期不保留旧代际兼容路径。

## 生命周期与失败路径

覆盖 Review 准入失败、取消、shutdown、结果写入后投影失败、重启恢复与旧 Memory key 归因；Managed conversation 创建/删除和同路径 workspace ABA；Plugin 激活失败关闭及跨池发布。

## 权限与敏感信息

本地 EKO 不增加线上服务权限门控。Review outcome 只保存在工作区本地收据 journal，用户凭据不写日志；文件删除和代际退休必须使用稳定身份与可观察错误。

## 用户侧投影

GUI Review 显示 `persisted` 的 true/false/unknown 状态并返回 operation ID；普通 Review 能力仍在 TUI/CLI。Side Conversation 创建与树形导航继续只在 GUI 暴露。

## 场景处置清单

上述五类场景已映射源码、ADR、Rule 和工程测试，并通过独立复审；CLI 最终远端 CI 与主分支交付仍需单独结算。

## 未展开项

框架 API 新增、生产安装验证、真实 Tauri 窗口视觉验收和 Issue #38 远端关闭不由本地图的源码快照单独证明。

## B 消费增量当前性

仅新增三个needs_review场景；旧独立处置不改。当前dirty源身份不证明行为验收，既有Background Review证据不自动关闭新B义务。


Scheduler/Task claim原场景因B关闭语义受影响而转needs_review；旧mapped工程来源保留，新增引用仅原config/runtime同owner及共用Evidence，不新建平行能力或删旧义务。其它三个B场景继续needs_review，历史观察仍绑定5cd9。


原九strong名单现已在CLI2203/public3ded实际9/0/0，并获原raw/输入守卫独立有限复核；当前不再标该名单从未执行。测试内晚fact注入、asyncflow与OS effect差别等限制见共用Evidence。此更新只同步当前事实，不刷新未变source摘要、不改变needs_review或关闭宽义务。


## CF988 当前任务 checkpoint

CF988三原focused各1pass，后继具名cron1pass及primary冻结副本surface两个独立red→after/正常普通控制共6pass均见共用Evidence。17file独审和测试非语义增量C0/I0/M0，all-targetClippy零warning通过。九例保3ded历史身份，不扩为CF988重跑。完整workspace/GUI、JSONL/精main CI待完成；三B场景needs_review、三lens stale、仓库闭合open保留。三依赖由人接受当前精版本风险并延期升级，Finding为risk_accepted而非resolved；精确门禁仍待验证，不冒原audit0。
