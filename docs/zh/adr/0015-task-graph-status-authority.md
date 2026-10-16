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

### 物理结算与无损执行事实

framework 的 exact-claim interruption request 先持久化，再信号通知原 cancellation token。
请求已提交不等于 claim 已结算，也不能发布 TaskRun 终态；最终使用 framework CAS 回执中的
实际状态，包括胜过迟到 dispatch 结果的更强 interruption。

内部 spec/execution join 与可丢弃 checkpoint 保留完整 canonical `TaskExecution`，包括缺失
结果与显式 JSON null 的差异以及 `interruption_requested`。graph commit 和 execution event
通过同一 journal 无损保留这些事实；旧 query checkpoint 必须从 journal 重放。`TaskSpec`、
`TodoItem` IPC 不变。原 physical owner intent 尚未结算时，boot recovery 与重复执行均被拒绝，
不把未知观察转换成新的 Task 状态。


## 影响

没有独立 Todo store、plan executor 或 parallel CRUD。计划批准由 artifact、prompt 和 policy
驱动，不扩展 run 状态机；旧 revision 的写入必须 fail closed。

`RunStateSnapshot.tasks` 直接保存 framework `TaskExecution`，不再定义 EKO 镜像类型；`PlanTask`
只在读取 plan specification 和 execution projection 时临时组合，`TodoItem` 仍是单向 UI 投影。

启动恢复将合法未结算 physical-owner intent 视为该精确 run 的待观察事实：在原 run 锁内保留 run、command cell 及待处理 Subagent 控制事实，继续恢复其它无关历史 run；畸形 intent 和真实存储错误仍报错。批次恢复和单 run 准入共用原证据读取与精确物理确认路径，不允许重放或合成终态收据。 continuation 消费既有 run 的 domain、route、attended mode、Goal 与 execution profile，内部续接指令不是新用户目标；实际 active workspace 与调用方 conversation/root 仍经原不可变身份校验，错配继续拒绝，RunStarted 投影使用返回的原 goal/route。
