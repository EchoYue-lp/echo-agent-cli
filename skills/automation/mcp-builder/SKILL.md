---
name: mcp-builder
description: 创建 MCP (Model Context Protocol) 服务器——连接外部 API 和服务
allowed-tools: shell read_file read_artifact apply_patch
metadata:
  category: automation
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: mcp, server, integration, api
---
# MCP Builder

Build an MCP server whose tools are predictable, inspectable, and easy for an agent to call correctly.

## Contract

- Inspect the target API, repository conventions, existing MCP configuration, and SDK version before choosing Python or TypeScript. Reuse the project's stack when possible.
- Design tools around user outcomes, not raw endpoint mirrors. Use specific names, concise descriptions, minimal required parameters, enums for closed sets, and structured results with stable error shapes.
- Keep authentication and secrets outside prompts, logs, fixtures, and returned tool content. Validate obvious malformed input, but do not add product-level permission gates that make a trusted local extension unusable.
- Define timeout, retry, pagination, rate-limit, cancellation, and partial-failure behavior. Avoid hidden writes; tool descriptions must disclose material side effects.
- Add tests for schema validation, happy path, upstream errors, empty results, and representative agent calls. Run an MCP inspector or client smoke test when available.

## Workflow

1. **Scaffold** → create the server project in the repo's convention (Python `FastMCP` or TypeScript `@modelcontextprotocol/sdk`); pin the SDK version you actually tested.
2. **Define the tool surface** → write each tool's name, description, input schema, and result shape before implementing. Review the list against user outcomes and cut endpoint mirrors.
3. **Implement** → one tool per module/function; shared auth and HTTP client in one place; structured errors (`{error: {code, message, retryable}}`) everywhere.
4. **Test** → schema validation and happy path first, then upstream error, empty result, timeout, and one representative agent-style call.
5. **Smoke-test** → start the server, connect with an MCP inspector or client, call every tool once, and record the evidence.
6. **Document** → configuration example without secrets, tool catalog, and upstream assumptions.

## Failure Handling

- **SDK version mismatch** (API changed between minor versions): pin the last known-good version, adapt calls, and note the constraint in the README instead of silently downgrading behavior.
- **Upstream API errors**: map status codes to stable tool errors; never let a raw stack trace or auth header leak into tool output.
- **Inspector unavailable**: fall back to a client smoke test or a scripted stdio exchange; state which verification was actually performed.
- **Timeouts/rate limits**: honor upstream `Retry-After`, expose `retryable` in the error shape, and let the agent decide.

## Delivery

Provide the working server, configuration example without secrets, tool catalog, verification evidence, and any upstream assumptions. Do not claim compatibility with a client or API version that was not tested.
