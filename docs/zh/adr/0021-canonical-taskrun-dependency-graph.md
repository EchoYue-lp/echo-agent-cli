# ADR 0021：Canonical TaskRun Dependency Graph

## 状态

已采纳，2026-08-29。

## 背景

后台 launcher 和 surface 曾尝试维护跨 TaskRun 的依赖 metadata，形成第二套 DAG、ready
frontier 和状态判断。

## 决策

依赖只能存在于单个 revisioned TaskRun 的 `PlanRevision.tasks[].depends_on`。framework
负责 DAG 校验、claim、ready frontier、retry 和 cancel；EKO 负责文件事实、review/worktree
和执行 policy。后台 launcher 不再轮询或拥有独立依赖图。

## 影响

所有 surface 读取同一 canonical graph，revision CAS 防止旧计划覆盖新计划；跨 TaskRun 编排
必须通过显式产品流程，而不是隐式 metadata 边。
