# ADR 0018：Typed Tool Artifact Projection

## 状态

已采纳，2026-08-29。

## 背景

工具输出可能超过即时事件和模型上下文预算。仅保存摘要或本地路径会让 GUI、TUI、CLI/JSONL
和 channel 对完整 artifact 的可见性不一致。

## 决策

framework `ToolResult.artifact` 是完整 spill 输出的唯一事实。EKO repository 无损保存
invocation/result，并在读取前校验 registered root、retention、文件身份、大小和摘要。各
surface 只投影 typed summary/detail/cursor，不从 metadata 猜测 terminal。

## 影响

长输出可按需加载、分页和恢复；artifact cleanup 与 GUI detail cursor 留在 EKO policy，
不创建第二个 tool result store。
