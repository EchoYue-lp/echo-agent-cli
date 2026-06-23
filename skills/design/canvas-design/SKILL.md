---
name: canvas-design
description: 创建精美视觉设计——海报、LOGO、信息图等，输出 PNG/PDF
metadata:
  category: design
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [design, poster, logo, infographic]
triggers:
  - 海报
  - logo
  - 设计
  - 信息图
  - poster
  - canvas
allowed-tools: [bash, read, write]
---
# Canvas Design

Create beautiful visual art in .png and .pdf documents. Design posters, logos, infographics, and other visual content.

## Features

- Create visual designs from text descriptions
- Output formats: PNG, PDF
- Support for typography, colors, layouts
- Iterative design refinement

## Requirements

- Font files in resources/ directory (loaded on demand)
