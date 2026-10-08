---
schema_version: 1
id: behavior.background-review-caller-owned-settlement
kind: behavior
status: verified
expectation: human_confirmed
risk: high
primary_focus: time_lifecycle
focus: [state_authority, data_durability, failure_concurrency, contract_evidence]
boundary: boundary.eko-framework-consumer-settlement
observed_at: 6b162b6ab2aa13c8262fd6424ff089a692df70b5
code_refs: [echo-agent-app-core/src/evolution/background_review_owner.rs, echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/workspace/runtime.rs, src/tauri/commands/panels.rs, src/cli/cmd_impls/evolution.rs, src/tui/events.rs]
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.latest-framework-consumer-integration]
finding_refs: []
---

# Caller-owned Background Review settlement

## 重要承诺

EKO 在框架惰性 Review 首次 poll 前持久准入，持有 generation 直到 outcome、Inbox 和 terminal 结算；调用方退出、应用关闭和进程恢复不能让已写入 Memory 完全无人观察。

## 当前行为

app-core ReviewGenerationLease 统一接收 GUI/TUI/CLI 的 BackgroundReviewHandle，记录 workspace generation、ReviewIdentity 和写入前 Memory 指纹；owner 监督任务、取消与结果，重启时对未完成收据执行 journal 恢复和候选幂等补投影。

## 期望行为

关闭准入或收据写入失败不 poll Review。正常 outcome 必须先持久化，再投影 Review Inbox，最后标终态。缺失 outcome 的中断不能伪造候选；旧 key 不得被归因为本次新写入，不可判定时返回 unknown。

## 触发、结果与副作用

GUI、TUI、CLI 用户普通 Review 请求触发；产出框架 ReviewOutcome、EKO typed receipt、可选 EvidenceCandidate。GUI 响应携带 operation ID，read-only 查询暴露 outcome 是否记录及 terminal。

## 失败、重试与恢复

shutdown 关闭准入、abort 后 await 子任务，再核对 Memory；重启先恢复框架 Memory journal，再重放有 outcome 的 Inbox 投影。跨 workspace generation 的未结算收据失败关闭。内存 key 预先存在且未观察到变化时报告归因未知。

## 证据

真实 owner 路径的 Outcome/Terminal append 写盘故障注入、收据恢复、准入失败不 poll、写入后 observer 阻塞取消、旧 key 归因测试，以及本轮 CLI workspace、GUI、JSONL 和前端门禁记录在 Evidence。独立复审已通过；这些证据仍不代替远端合并后 exact main 验证。

## 裁决记录

用户要求以最新 echo-agent 完成 GUI Side Conversation，并在 echo-agent-cli 拥有 Background Review 准入、shutdown、结果结算和恢复；开发期不保留兼容负担。通用 Review 与 Memory journal 留框架，EKO 生命周期与 UI Inbox 留应用。
