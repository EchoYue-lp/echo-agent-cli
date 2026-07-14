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

Create or edit a Word document while preserving its structure, styles, revision semantics, and intended print layout.

## Contract

- Inspect the existing document, template, styles, sections, headers/footers, tables, fields, comments, and tracked changes before editing. Preserve unrelated formatting and metadata.
- Use structured DOCX APIs/XML rather than treating the file as plain text. For redlines, represent additions/deletions and comments as real Word revisions when requested.
- Reuse paragraph/table styles and theme fonts. Avoid manual formatting that creates visually similar but structurally inconsistent content.
- Preserve links, numbering, cross-references, page breaks, and accessibility information where possible. Do not silently accept/reject existing revisions.
- After every material edit, render the DOCX with LibreOffice and inspect page images for overflow, broken tables, orphan headings, missing fonts, and header/footer changes.

Deliver the final `.docx` and report any unsupported feature, font substitution, unresolved revision, or layout risk. A file that opens is not sufficient verification; the rendered pages must be checked.
