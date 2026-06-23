---
name: writing-plans
description: 多步任务先规划再执行——为多步实现任务编写全面的实施计划
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [planning, implementation, architecture]
triggers:
  - 计划
  - plan
  - 规划
  - 实施方案
  - 实现计划
  - 怎么实现
allowed-tools: []
---

# Writing Plans

Write comprehensive implementation plans assuming the engineer has zero context for our codebase and questionable taste. Document everything they need to know: which files to touch, code, testing, docs. Give them the whole plan as bite-sized tasks.

## When to Use

- Multi-step implementation tasks (3+ distinct steps)
- New feature implementation
- Architecture changes
- Anything that spans multiple files

## Plan Structure

1. **Goal** — one sentence describing what this builds
2. **Architecture** — 2-3 sentences about approach
3. **Tech Stack** — key technologies
4. **File Structure** — which files are created/modified and why
5. **Tasks** — each task is independently testable, has exact file paths, complete code, test commands

## Key Principles

- **TDD**: every task starts with a failing test
- **No placeholders**: no TODOs, no "implement later"
- **Bite-sized**: each step is one action (2-5 minutes)
- **DRY + YAGNI**: don't over-engineer
