---
name: subagent-driven-development
description: 使用独立子代理逐任务执行实施计划，任务间审查
metadata:
  category: development
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [subagent, plan-execution, review]
triggers:
  - 子代理
  - subagent
  - 逐任务
allowed-tools: [bash]
---

# Subagent-Driven Development

Use when an implementation plan contains bounded tasks that benefit from isolated worker context and the runtime can enforce write ownership or worktree isolation.

## Process

1. Read the implementation plan
2. For each task, define outcome, targets, allowed side effects, dependencies, and verification; choose a specific role.
3. Dispatch independent read-only tasks in parallel. Serialize or isolate writer tasks according to file ownership.
4. Review the actual diff/artifact and verification, not only the worker's prose. Reject unsupported completion claims.
5. On failure, diagnose whether the task, context, environment, or implementation is wrong before retrying.
6. Integrate results, resolve conflicts, and run the repository's final verification from the authoritative workspace.

Do not delegate a task merely to avoid understanding it, and do not let workers mutate the global plan unless the runtime contract explicitly permits it.
