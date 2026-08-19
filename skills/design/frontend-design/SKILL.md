---
name: frontend-design
description: 创建高质量前端界面——网站、落地页、仪表板、React 组件
metadata:
  category: design
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [frontend, web, react, ui]
triggers:
  - 前端
  - 界面
  - UI
  - 网页
  - 仪表板
  - landing page
allowed-tools: [shell, read_file, read_artifact, apply_patch]
---
# Frontend Design

Build the actual usable interface in the repository's existing frontend stack and design system.

## Contract

- Inspect the product domain, target user, current routes/components/tokens, and primary workflow before designing. The first screen should perform the requested job, not advertise it.
- Choose information density and composition for the domain: operational tools should optimize scanning and repeated action; expressive products may use richer motion and imagery.
- Use familiar controls and the existing icon library. Include loading, empty, error, disabled, success, overflow, and keyboard/focus states that the workflow naturally requires.
- Keep layouts responsive with stable dimensions for toolbars, boards, media, and dynamic content. Prevent clipping, overlap, layout shift, unreadable text, and nested decorative cards.
- Avoid generic generated-UI defaults such as decorative gradients, oversized marketing copy, one-note palettes, arbitrary pills, and visible instructions explaining the UI.
- Preserve accessibility semantics and contrast. Reuse real or generated visual assets when the subject needs visual inspection.

Run type/build tests and inspect the rendered interface in desktop and mobile viewports. Iterate from screenshots and browser state, not source code alone.
