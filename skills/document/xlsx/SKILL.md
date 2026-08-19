---
name: xlsx
description: Excel 电子表格创建、公式计算、数据分析和图表
metadata:
  category: document
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [excel, spreadsheet, office, data]
  requires-binaries: "soffice"
  requires-python-packages: "openpyxl"
triggers:
  - Excel
  - xlsx
  - 电子表格
  - 公式
  - 数据表
allowed-tools: [shell, read_file, read_artifact, apply_patch]
hooks:
  UserPromptSubmit:
    - matcher: "\\.xlsx"
      hooks:
        - type: activate_skill
          skill: xlsx
          reason: 检测到 .xlsx 文件路径
---
# XLSX Skill

Create or edit a workbook whose data, formulas, references, and presentation remain auditable.

## Contract

- Inspect sheets, named ranges, tables, formulas, external links, hidden rows/columns, validation, pivots, charts, macros, and calculation settings before editing.
- Preserve source data and existing formulas unless replacement is explicitly requested. Use formulas for derived values when users need live recalculation; avoid hard-coded results.
- Keep units, dates, number formats, denominators, and missing-value conventions explicit. Validate joins/lookups and reconcile important totals independently.
- Apply restrained formatting that supports scanning: clear headers, frozen panes, filters, sensible widths, semantic number formats, and accessible chart colors.
- Recalculate with an available spreadsheet engine and render representative sheets. Check for formula errors, broken references, clipped content, stale cached values, and chart/source mismatches.

Deliver the final `.xlsx` and report changed sheets, key formulas/checks, recalculation status, external-link or macro limitations, and any cells requiring user review.
