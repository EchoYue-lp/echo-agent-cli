---
name: mcp-builder
description: 创建 MCP (Model Context Protocol) 服务器——连接外部 API 和服务
metadata:
  category: automation
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [mcp, server, integration, api]
triggers:
  - MCP
  - mcp server
  - 模型上下文协议
  - 工具服务器
allowed-tools: [shell, read_file, read_artifact, apply_patch]
---
# MCP Builder

Build an MCP server whose tools are predictable, inspectable, and easy for an agent to call correctly.

## Contract

- Inspect the target API, repository conventions, existing MCP configuration, and SDK version before choosing Python or TypeScript. Reuse the project's stack when possible.
- Design tools around user outcomes, not raw endpoint mirrors. Use specific names, concise descriptions, minimal required parameters, enums for closed sets, and structured results with stable error shapes.
- Keep authentication and secrets outside prompts, logs, fixtures, and returned tool content. Validate obvious malformed input, but do not add product-level permission gates that make a trusted local extension unusable.
- Define timeout, retry, pagination, rate-limit, cancellation, and partial-failure behavior. Avoid hidden writes; tool descriptions must disclose material side effects.
- Add tests for schema validation, happy path, upstream errors, empty results, and representative agent calls. Run an MCP inspector or client smoke test when available.

## Delivery

Provide the working server, configuration example without secrets, tool catalog, verification evidence, and any upstream assumptions. Do not claim compatibility with a client or API version that was not tested.
