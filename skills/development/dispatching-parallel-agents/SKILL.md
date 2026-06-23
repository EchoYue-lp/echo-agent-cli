---
name: dispatching-parallel-agents
description: 并行派发独立子代理处理多个互不依赖的任务
metadata:
  category: development
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [parallel, subagent, concurrency]
triggers:
  - 并行
  - parallel
  - 同时
  - 多个任务
  - 并发
allowed-tools: [bash]
---

# Dispatching Parallel Agents

When facing 2+ independent tasks that can be worked on without shared state or sequential dependencies, dispatch parallel subagents.

## When to Use

- Multiple independent tasks in a plan
- Tasks that don't share state
- Fan-out operations (search multiple things, check multiple files)

## Process

1. Identify independent tasks — no shared state, no sequential dependency
2. Dispatch each as a separate subagent
3. Wait for all to complete
4. Synthesize results
