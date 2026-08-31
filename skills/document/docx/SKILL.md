---
name: docx
description: Word 文档创建、编辑、批注和修订标记
allowed-tools: shell read_file read_artifact apply_patch
metadata:
  category: document
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: word, document, office
  requires-binaries: soffice
  requires-python-packages: defusedxml, lxml
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
