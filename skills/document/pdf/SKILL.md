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
allowed-tools: [bash, read, write]
hooks:
  UserPromptSubmit:
    - matcher: "\\.pdf"
      hooks:
        - type: activate_skill
          skill: pdf
          reason: 检测到 .pdf 文件路径
---
# PDF Skill

Create, read, and manipulate PDF documents. Supports text extraction, form filling, page manipulation, and format conversion.

## Features

- Create PDFs from HTML/Markdown/text
- Extract text, images, and metadata from PDFs
- Merge, split, rotate, and reorder pages
- Fill PDF forms
- Convert PDFs to images for visual analysis

## Requirements

- LibreOffice (`soffice`) for rendering
- `pdftoppm` (poppler) for image conversion
- Python packages: `pypdf`, `Pillow` (auto via `uv run`)
