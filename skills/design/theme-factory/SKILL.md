---
name: theme-factory
description: 为幻灯片、文档、HTML 页面等应用预定义主题/配色方案
metadata:
  category: design
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: theme, color, styling
---
# Theme Factory

Create or apply a coherent theme system that improves hierarchy and consistency without changing the artifact's meaning.

Inspect the artifact type, audience, brand constraints, existing content density, and available fonts. Define semantic color roles, typography, spacing, surfaces, borders, charts, imagery, and state colors rather than a loose palette. Ensure text/background and chart contrast remain accessible.

When applying a theme to an existing artifact, preserve content, structure, formulas, and functional behavior. Replace styles systematically through tokens or master/layout styles where the format supports them. Avoid theme choices that overpower dense operational or analytical content.

Render representative pages/slides/screens after application. Check consistency, overflow, chart readability, print/export behavior, and dark/light assumptions. Deliver the themed artifact and a compact theme specification listing tokens and any font substitutions.

## Workflow

1. Inventory existing styles and identify which values are semantic versus one-off exceptions.
2. Define a small token set for surfaces, text, borders, actions, status, typography, spacing, and charts.
3. Apply tokens through the artifact's supported theme, master, or component mechanism so future edits inherit the system.
4. Render dense and sparse states, including long labels and error/status colors, before finalizing.

## Anti-patterns

- Do not replace every color with one accent hue or use gradients to hide weak hierarchy.
- Do not change copy, formulas, or interaction semantics while applying a theme.
- Do not call a theme complete until print/export and contrast behavior are checked.
