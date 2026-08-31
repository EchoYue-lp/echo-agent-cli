---
name: webapp-testing
description: 使用 Playwright 进行 Web 应用交互测试——表单填写、截图、UI 验证
allowed-tools: shell read_artifact
metadata:
  category: automation
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: testing, web, playwright, e2e
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

## Workflow

1. Turn the request into a short acceptance table: precondition, action, expected state, and evidence.
2. Start from a clean browser context when possible and seed only non-sensitive test data.
3. Assert visible state and accessible semantics after each meaningful action; capture evidence at the assertion boundary.
4. On failure, preserve the first console error, failed request, screenshot, and reproduction step before retrying.
