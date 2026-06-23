---
name: pptx
description: PowerPoint 演示文稿创建、编辑和模板应用
metadata:
  category: document
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [powerpoint, presentation, office]
  requires-binaries: "soffice"
  requires-python-packages: "python-pptx"
triggers:
  - PowerPoint
  - pptx
  - 演示文稿
  - 幻灯片
  - PPT
allowed-tools: [bash, read, write]
---
# PPTX Skill

Create and edit PowerPoint presentations (.pptx). Support for slide layouts, text formatting, images, charts, and speaker notes.

## Features

- Create presentations from outlines or Markdown
- Apply themes and slide layouts
- Add text, images, charts, and tables to slides
- Edit existing presentations
- Extract content from slides

## Requirements

- LibreOffice (`soffice`) for rendering
- Python: `python-pptx` (auto via `uv run`)
