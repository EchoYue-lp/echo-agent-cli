---
name: executing-plans
description: 在独立会话中执行实施计划，带审查检查点
metadata:
  category: development
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [plan-execution, implementation]
triggers:
  - 执行计划
  - execute plan
  - 按计划实施
allowed-tools: []
---

# Executing Plans

Use when an approved or already-authoritative plan must be executed. The plan guides work but does not override current repository evidence or user instructions.

## Process

1. Load the plan and the current repository/task state; identify stale assumptions before editing.
2. Execute tasks according to real dependencies. Parallelize only independent work and preserve task/file ownership.
3. After each task, run its completion check and update status truthfully.
4. If evidence invalidates the plan, stop that branch, explain the deviation, and revise the plan rather than forcing the old design.
5. Run final cross-task verification and synthesize the user-facing outcome, residual risks, and unverified paths.
