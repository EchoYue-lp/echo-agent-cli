# ADR 0015：Task Graph Status Authority

## 状态

已采纳，2026-08-28。

## 背景

Task、Plan 和 Todo 曾有相互重叠的状态写入路径，导致 UI 投影可能反向改变执行图，计划
批准也被错误建模为运行时状态机。

## 决策

`TaskRun -> PlanTask -> SubagentRun` 是唯一关系图。`TaskStatus` 和 revisioned task graph
由 framework/runtime authority 驱动，`PlanRevision` 是可编辑 artifact，`TodoItem` 只是只读
查询投影。EKO 只提供 `task_execute`、文件事实、review/worktree 和 surface policy。

## 影响

没有独立 Todo store、plan executor 或 parallel CRUD。计划批准由 artifact、prompt 和 policy
驱动，不扩展 run 状态机；旧 revision 的写入必须 fail closed。

`RunStateSnapshot.tasks` 直接保存 framework `TaskExecution`，不再定义 EKO 镜像类型；`PlanTask`
只在读取 plan specification 和 execution projection 时临时组合，`TodoItem` 仍是单向 UI 投影。
