# ADR 0022：Framework-prepared Plugin Generations

## 状态

已采纳并实现，2026-08-29。

## 背景

Plugin preparation、workspace target、primary Agent、pool Agent 和 future Agent 的发布
曾可能各自刷新，导致 generation 不一致或旧 target 迟到覆盖新 target。

## 决策

framework 负责从根 `plugin.json` 解析、校验和生成不可变 prepared generation。EKO 在一次
捕获的 workspace target 上执行产品组件 publication，并使用 apply receipt 完成原子 fanout
和 rollback。所有现有/未来 Agent 都绑定同一 generation。

## 影响

Plugin 和 Skill/monitor/theme 等产品投影不再重复 preparation；cold workspace 继承 committed
generation，旧 generation 的迟到结果按 identity fail closed。
