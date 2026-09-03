# ADR 0012：EKO Extension Control Authority

## 状态

已采纳，2026-08-25；已实现，2026-08-26。Skill durable settlement 状态机部分已由
[ADR 0036](./0036-skill-policy-simplification.md) 取代；统一 Extension authority、mutation
互斥和多 surface 共享合同仍保留。

## 背景

Skills、Hooks、MCP、LSP、Browser 和 Plugin 需要在多个 surface 共享配置、generation 和
settlement。分别维护 registry 或 optimistic UI 会制造第二事实源。

## 决策

`ExtensionControlService` 是 EKO 的 mutation admission，协调 specialist runtime 并把
`enabled-skills.json` 作为唯一 durable desired fact。所有 mutation 先完成 durable commit，
再 fan out；runtime 失败返回 committed-but-degraded receipt 和 bounded repair debt。GUI、TUI、
CLI/JSONL、channel 使用相同的 typed receipt。

## 影响

重复 operation 可幂等恢复，旧 generation 不能覆盖新 generation。Skill parser、MCP/LSP
协议和 Plugin preparation 继续复用 framework authority，EKO 不建立第二 store。
