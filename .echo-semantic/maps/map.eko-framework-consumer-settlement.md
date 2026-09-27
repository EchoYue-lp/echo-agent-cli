---
schema_version: 1
id: map.eko-framework-consumer-settlement
kind: capability_map
title: EKO framework consumer settlement
risk: high
observed_at: source:caa0bf81fcda8d046037a8a1065adc31b7bb0652059b4e4c8e975d8831e26fc0
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: [behavior.background-review-caller-owned-settlement]
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.latest-framework-consumer-integration]
finding_refs: []
audit_refs: []
related_map_refs: [map.eko-conversation-collaboration]
scenarios:
  background-review-admission-and-recovery:
    status: mapped
    source_refs: [echo-agent-app-core/src/evolution/background_review_owner.rs, echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/workspace/runtime.rs]
    rule_refs: [rule.eko-framework-consumer-authorities]
    evidence_refs: [evidence.latest-framework-consumer-integration]
  review-surface-result:
    status: mapped
    source_refs: [src/tauri/commands/panels.rs, src/cli/cmd_impls/evolution.rs, src/tui/events.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/components/evolution/EvolutionPanel.tsx]
    rule_refs: [rule.eko-framework-consumer-authorities]
    evidence_refs: [evidence.latest-framework-consumer-integration]
  skill-and-plugin-lifecycle:
    status: mapped
    source_refs: [echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/extension_control/policy.rs, echo-agent-app-core/src/plugin_runtime/runtime.rs, echo-agent-app-core/src/agent_pool/generation.rs]
    rule_refs: [rule.eko-framework-consumer-authorities]
    evidence_refs: [evidence.latest-framework-consumer-integration]
  managed-conversation-and-workspace-retirement:
    status: mapped
    source_refs: [echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/conversation_deletion.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, echo-agent-app-core/src/workspace/runtime.rs, echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs, src/cli/channels.rs]
    rule_refs: [rule.eko-framework-consumer-authorities]
    evidence_refs: [evidence.latest-framework-consumer-integration]
  scheduler-and-task-claim-adaptation:
    status: mapped
    source_refs: [echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs, echo-agent-app-core/src/tasks/task_runtime/run_authority.rs]
    rule_refs: [rule.eko-framework-consumer-authorities]
    evidence_refs: [evidence.latest-framework-consumer-integration]
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
