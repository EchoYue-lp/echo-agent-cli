---
name: deep-research
description: 深度调研——多源搜索、交叉验证、生成带引用的综合报告
metadata:
  category: research
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [research, search, verification, report]
triggers:
  - 调研
  - research
  - 深度研究
  - 综合报告
  - 多源验证
allowed-tools: [shell, read_file, read_artifact, apply_patch, web_search, web_fetch]
---
# Deep Research

Use for questions whose answer depends on multiple sources, conflicting evidence, or a reproducible search trail. Do not invoke it for a fact that one authoritative source can answer.

## Contract

- Define the question, audience, decision, date sensitivity, jurisdiction/domain, and what would count as sufficient evidence.
- Decompose by claim or uncertainty, then search independent lines in parallel. Prefer primary and authoritative sources; use secondary sources to discover context or disagreement.
- Read the underlying source before citing it. Record publication/update date, scope, methodology, conflicts, and whether it directly supports the claim.
- Cross-check central claims, numbers, dates, and quotations. Treat absence of evidence, search-result snippets, and repeated syndication as weak evidence.
- Use a retrieval budget: stop when the core request is supported and remaining searches would only add repetition. Expand only for a missing material fact or an explicitly exhaustive request.
- Synthesize by claim, not by source. Separate consensus, disagreement, inference, and unknowns; include limitations and counterevidence.

Deliver the supported conclusion first, followed by decisive evidence and source links/citations in the active tool's format. State search scope and unresolved gaps.
