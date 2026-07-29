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
allowed-tools: [shell, read_file, read_artifact, write_file]
hooks:
  UserPromptSubmit:
    - matcher: "\\.pptx"
      hooks:
        - type: activate_skill
          skill: pptx
          reason: 检测到 .pptx 文件路径
---
# PPTX Skill

Create or edit a presentation that works as a live visual narrative, not a document pasted onto slides.

## Contract

- Establish audience, setting, duration, decision/outcome, aspect ratio, brand/template, and required source material.
- Build a narrative arc and give each slide one job. Use concise titles that state the takeaway, then support it with evidence, diagrams, charts, or imagery.
- Reuse slide masters/layouts and theme tokens. Keep type sizes readable, charts honest, tables sparse, and speaker notes separate from on-slide copy.
- When editing, preserve unrelated animations, notes, links, masters, and object alignment. Do not invent metrics, customer claims, or citations to strengthen a story.
- Render the deck with LibreOffice and inspect every slide for clipping, font substitution, overlap, weak contrast, off-canvas objects, and inconsistent margins.

Deliver the final `.pptx`, rendered preview/PDF when useful, and disclose missing assets, font substitutions, or unsupported animation behavior.
