# ADR 0013：Subagent Control Receipt Projection

## 状态

已采纳，2026-08-27。

## 背景

TaskRun 内 Subagent 的 message、follow-up、interrupt 和终态需要同时支持精确 attempt、跨
重启恢复和多入口投影。直接暴露内部控制状态会让 GUI/TUI/CLI 建立不同的 reducer。

## 决策

以 `SubagentControlService` 作为 attempt-scoped authority，使用带 execution、attempt、
revision 和 generation 的 typed receipt。`AgentControlService` 只做 discriminator、bounded
query 和 surface 适配，不拥有新的 mailbox、TaskRun store 或 terminal reducer。

## 影响

所有 surface 获得一致的 message/follow-up/wait/interrupt 语义；迟到旧 attempt 不能污染新
revision，receipt 投影可以从 durable journal 重建。
