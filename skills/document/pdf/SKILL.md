---
name: pdf
description: PDF 文档创建、编辑、提取和表单处理
metadata:
  category: document
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [pdf, document]
  requires-binaries: "soffice, pdftoppm"
  requires-python-packages: "pypdf, Pillow"
triggers:
  - PDF
  - pdf
  - 导出PDF
  - .pdf
allowed-tools: [shell, read_file, read_artifact, write_file]
hooks:
  UserPromptSubmit:
    - matcher: "\\.pdf"
      hooks:
        - type: activate_skill
          skill: pdf
          reason: 检测到 .pdf 文件路径
---
# PDF Skill

Read, create, or modify PDFs with both content accuracy and visual verification.

## Contract

- Identify whether the PDF is text-based, scanned, form-enabled, signed, encrypted, tagged, or layout-critical before choosing extraction or editing methods.
- Use a PDF parser for structure/metadata and rendered page images for visual truth. OCR output is evidence with uncertainty; verify critical names, numbers, and tables against the page image.
- Preserve page order, size, rotation, bookmarks, links, form field names/values, and existing metadata unless the task requires changing them. Never imply a cryptographic signature remains valid after modification.
- For generated PDFs, use real layout primitives, embedded fonts, stable margins, and accessible reading order when supported.
- Render all changed pages with Poppler and inspect for clipping, missing glyphs, broken images, table overflow, blank pages, and incorrect form placement.

Deliver the final PDF plus any extracted structured data requested. Report OCR limitations, removed signatures, unsupported forms, font substitutions, and the pages visually inspected.
