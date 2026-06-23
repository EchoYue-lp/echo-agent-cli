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

Use when executing implementation plans with independent tasks in the current session.

## Process

1. Read the implementation plan
2. For each task: dispatch a fresh subagent with the task specification
3. Review the subagent's output before proceeding
4. If a task fails, fix and retry before moving to the next
5. After all tasks, run full verification
