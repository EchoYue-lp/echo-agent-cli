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
allowed-tools: [bash, read, write]
hooks:
  UserPromptSubmit:
    - matcher: "\\.xlsx"
      hooks:
        - type: activate_skill
          skill: xlsx
          reason: 检测到 .xlsx 文件路径
---
# XLSX Skill

Create and manipulate Excel spreadsheets (.xlsx). Supports formulas, data analysis, charts, pivot tables, and formatting.

## Features

- Create spreadsheets from data (CSV, JSON, etc.)
- Add formulas and cell references
- Create charts and pivot tables
- Apply formatting and conditional formatting
- Read and analyze existing spreadsheets

## Requirements

- LibreOffice (`soffice`) for rendering
- Python: `openpyxl` (auto via `uv run`)
