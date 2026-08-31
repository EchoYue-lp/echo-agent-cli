---
name: web-artifacts-builder
description: 构建复杂的多组件 HTML 应用——使用 React、Tailwind、shadcn/ui
allowed-tools: shell read_file read_artifact apply_patch
metadata:
  category: design
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: web, react, tailwind, html
---
# Web Artifacts Builder

Build a self-contained interactive web artifact that works immediately and communicates its subject through real data, controls, and states.

Use the repository's existing stack when present; otherwise prefer React + TypeScript with the lightest dependencies needed. Design the data model and primary interaction before styling. Include feature-complete controls, useful defaults, reset/export behavior where relevant, and loading/empty/error states.

Do not assume Tailwind, shadcn/ui, or internet-hosted assets are available. Bundle or reference dependencies according to the delivery target. Keep the artifact responsive, keyboard accessible, and free of decorative marketing sections or nested card layouts.

Run the artifact and test the main workflow in a browser at desktop and mobile sizes. Inspect console errors, layout, text overflow, interaction state, and exported output. Deliver the runnable artifact and the URL or file path used for verification.

## Workflow

1. Define the data model and one primary interaction before selecting components or visual treatment.
2. Build a working vertical slice with representative data, then add empty, loading, error, and reset states.
3. Keep dependencies local and pinned; never make the first render depend on an external CDN or undocumented service.
4. Verify keyboard access, responsive layout, console/network health, and export/reset behavior in the target browser.

## Delivery checklist

- A clean checkout can run the artifact using the documented command.
- The main workflow is usable without reading a separate instruction page.
- State transitions and persisted/exported data are inspectable and deterministic.
