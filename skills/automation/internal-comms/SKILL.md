---
name: internal-comms
description: 撰写内部沟通文档——状态报告、项目更新、周报、FAQ
metadata:
  category: automation
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [communication, writing, status-report]
triggers:
  - 周报
  - 状态报告
  - 项目更新
  - 内部沟通
  - status report
allowed-tools: []
---
# Internal Communications

Produce communication that helps a specific internal audience understand status, make a decision, or take action.

## Contract

- Establish audience, purpose, decision/action, reporting period, owner, and sensitivity. Reuse the organization's template and terminology when available.
- Treat supplied notes, metrics, dates, owners, and status as evidence. Do not invent progress, commitments, root causes, or executive quotes.
- Lead with the outcome: current state, what changed, impact, decision needed, and next owner/date. Separate facts, risks, decisions, and proposals.
- For incidents and postmortems, use a blameless timeline, customer/system impact, contributing conditions, detection/response gaps, and owned corrective actions. Distinguish confirmed cause from hypothesis.
- For meeting notes, record decisions and action items with owners and dates; omit conversational transcript unless requested.

## Quality Check

Verify names, numbers, dates, links, confidentiality, and action ownership. Remove filler, vague status words, and unsupported optimism. Return the finished communication, plus a short list of unresolved placeholders only when required inputs are missing.
