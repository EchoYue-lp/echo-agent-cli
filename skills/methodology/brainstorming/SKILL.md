---
name: brainstorming
description: 探索用户意图、需求和设计，把想法变成完整设计稿
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [design, planning, workflow]
triggers:
  - 头脑风暴
  - 设计
  - brainstorm
  - 做个方案
  - 设计一下
  - 规划方案
allowed-tools: []
---

# Brainstorming Ideas Into Designs

Help turn ideas into fully formed designs and specs through natural collaborative dialogue.

Use this skill when the user is exploring an idea, requests a design/spec, or when a high-impact implementation choice is genuinely unresolved. Do not turn a clear bug fix, small config change, or already-approved design into a mandatory approval ceremony.

## Checklist

1. **Explore project context** — check files, docs, recent commits
2. **Define outcome and constraints** — ask only questions whose answers materially change the design
3. **Investigate precedent** — for architecture, state, API, or orchestration decisions, research mature implementations and existing repository mechanisms
4. **Compare viable approaches** — include alternatives only when they are meaningfully different
5. **Present a recommendation** — explain trade-offs, ownership boundary, failure behavior, and validation
6. **Get approval when needed** — required for requested design-only work or a choice that changes scope; otherwise proceed within the user's implementation authorization

## Key Principles

- **Narrow questions** - Ask only what cannot be discovered and would materially affect the result
- **YAGNI ruthlessly** - Remove unnecessary features from all designs
- **Explore alternatives** - Always propose 2-3 approaches before settling
- **Incremental validation** - Present design, get approval before moving on
