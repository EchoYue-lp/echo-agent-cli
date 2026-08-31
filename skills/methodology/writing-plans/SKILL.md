---
name: writing-plans
description: 多步任务先规划再执行——为多步实现任务编写全面的实施计划
metadata:
  category: methodology
  source: superpowers
  upstream-version: 6.0.3
  author: obra
  tags: planning, implementation, architecture
---

# Writing Plans

Write an implementation plan that another capable engineer or agent can execute without reconstructing hidden assumptions. Ground it in the current repository and name the evidence behind important decisions.

## When to Use

- Multi-step implementation tasks (3+ distinct steps)
- New feature implementation
- Architecture changes
- Anything that spans multiple files

## Plan Structure

1. **Goal** — one sentence describing what this builds
2. **Current evidence** — existing mechanisms, constraints, and runtime path
3. **Ownership** — framework vs application boundary and data/state owners
4. **Approach** — API/data flow/state transitions and failure behavior where relevant
5. **Files and tasks** — concrete targets, dependencies, outcome, and verification for each step
6. **Risks/open questions** — only those that materially affect execution

## Key Principles

- **Evidence first**: confirm the capability does not already exist and reference relevant code
- **Testable**: each task has a completion signal; use test-first when it adds regression value
- **Executable**: avoid vague verbs, invented APIs, and unnecessary code dumps
- **DRY + YAGNI**: don't over-engineer
