---
name: claude-api
description: 构建、调试和优化 Claude API / Anthropic SDK 应用，含 prompt caching 和模型迁移
metadata:
  category: research
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: claude, api, sdk, anthropic, prompt-cache
---
# Claude API

Build or diagnose Claude API integrations from current Anthropic documentation and the application's actual SDK version.

## Contract

- Inspect the dependency lockfile, request/response code, model configuration, auth path, and observed error before proposing changes.
- Verify current model IDs, SDK methods, tool-use schemas, streaming events, prompt-caching rules, token limits, and migration requirements from official Anthropic sources when network retrieval is available. Do not rely on remembered product details.
- Keep migrations narrow: preserve provider behavior, tests, retries, observability, and user-configured model choices unless the task explicitly changes them.
- For tools and structured output, use precise schemas, validate partial/invalid responses, and handle stop reasons and streaming assembly explicitly.
- Never expose API keys in code, logs, examples, or tool results. Distinguish authentication, rate-limit, overload, validation, and application errors.

Run the smallest real or mocked request that proves the change. Report the SDK/model versions, official sources used, verification, and any behavior not exercised.

## Workflow

1. Capture the provider dependency/version, configured model, request builder, streaming path, and credential source.
2. Check current official documentation for the exact model, endpoint, tool schema, cache semantics, and limits involved.
3. Reproduce with a redacted fixture or mock before changing production code; preserve retry and cancellation behavior.
4. Verify complete and partial or invalid responses, then document the tested version and remaining provider assumptions.

Never paste credentials into a prompt, fixture, trace, screenshot, or issue, even when diagnosing an authentication failure.
