---
name: webapp-testing
description: 使用 Playwright 进行 Web 应用交互测试——表单填写、截图、UI 验证
metadata:
  category: automation
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [testing, web, playwright, e2e]
triggers:
  - 测试
  - web test
  - Playwright
  - 浏览器测试
  - e2e
allowed-tools: [shell, read_artifact]
---
# Webapp Testing

Test the real user workflow in a browser and leave reproducible evidence, not merely a screenshot of a loaded page.

## Contract

- Confirm the app URL, expected workflow, test data, authentication state, and viewport. Start the local server only when needed and report its address.
- Prefer accessible roles, labels, and stable test IDs over brittle CSS or text-position selectors. Wait for meaningful UI state, not arbitrary sleeps.
- Exercise the full path: initial state, interaction, loading/empty/error states, validation, success state, navigation/back behavior, and persistence where relevant.
- Check browser console and failed network requests. Verify responsive layout at representative desktop and mobile sizes, including clipping, overlap, focus, and keyboard access.
- Capture screenshots at the state that proves each important assertion. For canvas/3D/media, verify rendered pixels or playback in addition to DOM presence.
- Do not perform destructive production actions or use real customer data unless explicitly scoped.

## Delivery

Report the tested URL/viewport, steps, observed result, console/network issues, and artifact paths. Distinguish product defects, test-environment failures, and unverified paths.
