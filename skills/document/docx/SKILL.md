---
name: docx
description: Word 文档创建、编辑、批注和修订标记
metadata:
  category: document
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [word, document, office]
  requires-binaries: "soffice"
  requires-python-packages: "defusedxml, lxml"
triggers:
  - Word
  - docx
  - 修订
  - 批注
  - 公文
  - word 文档
  - .docx
allowed-tools: [bash, read, write]
hooks:
  UserPromptSubmit:
    - matcher: "\\.docx"
      hooks:
        - type: activate_skill
          skill: docx
          reason: 检测到 .docx 文件路径
---
# DOCX Skill

Create, edit, and analyze Word documents (.docx) with support for tracked changes, comments, formatting, and text extraction.

## Features

- Create new Word documents from Markdown or plain text
- Edit existing documents: add/remove text, apply formatting
- Add comments and tracked changes (revision marks)
- Extract text and structure from documents
- Convert between formats

## Requirements

- LibreOffice (`soffice`) for document rendering
- Python packages: `defusedxml`, `lxml` (auto-installed via `uv run --script`)
