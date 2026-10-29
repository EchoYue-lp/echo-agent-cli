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

## 当前工具执行投影的归并

GUI 的实时工具摘要与冷重开摘要复用现有状态归并规则。关联以 workspace、conversation、Run、原 owner、call_id 和工具名称为准；实时通知与冷恢复只归并同一详情 ID，重复 call_id 对应多个详情 ID 时保留原歧义，由原 consumer 显示 Unknown。简化的 TaskRuntime 边界也无权从多个详情中选择一个，不借用其它会话或 Run 的同名 ID。历史恢复只接纳所请求 workspace/conversation 的摘要，保留原数组中的执行顺序；仅有 runtime 边界的记录追加在末尾。

没有结束事实的 Unknown 记录不能覆盖真实开始或结果。后端以 Unknown 结束孤儿调用时，已有的结束时间会停止运行中展示，结果仍为未知；它不覆盖真实工具结果，迟到的开始也不能将其改回运行中。不从正文、工具名或详情是否存在推导成功。TaskRuntime 的简化边界可补齐迟到的开始时间，但不能覆盖已有完整 canonical detail 终态。这里只归并呈现，不重新执行工具、不改后端结果或任务结算。

本次新增的 store 回归验证实时、冷恢复、重复和迟到边界；GUI 原生窗口、后端持久化和跨进程恢复仍分别验收。内置终端及 Agent shell 继续保留；独立 TUI/CLI/REPL/JSONL 产品入口已具名退役，历史章节中的入口列表不构成恢复这些入口的要求。
