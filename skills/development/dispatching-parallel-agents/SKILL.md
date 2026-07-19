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

Use parallel subagents when two or more bounded tasks are genuinely independent and isolated context will reduce noise or latency. Parallelism is a tool, not a default response to task size.

## When to Use

- Read-only investigations over separate modules, sources, datasets, or hypotheses
- Independent artifact generation with disjoint output ownership
- Reviews that benefit from different evidence lenses

## Process

1. Define one outcome, evidence requirement, boundary, and return format per subagent.
2. Confirm no data dependency, overlapping write target, shared mutable state, or approval sequence.
3. Choose the most specific roles and dispatch only the useful fan-out; keep dependent work local or sequential.
4. Inspect results for failures, duplication, and conflicts. Re-run only the missing line of evidence.
5. Synthesize against the parent goal; do not paste subagent summaries as the final answer.

Avoid parallel dispatch for tiny tasks, tightly coupled debugging, overlapping file edits, or work whose outputs cannot be reconciled reliably.
