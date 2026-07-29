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
allowed-tools: [shell, read_file, read_artifact, write_file]
---
# Canvas Design

Create a finished static visual whose hierarchy, typography, composition, and export quality match its real use.

Establish audience, message, dimensions, viewing distance, required copy, brand assets, and output format. Build one strong visual concept with a clear focal point and reading order. Use a deliberate grid, restrained type system, accessible contrast, and imagery that carries information rather than decoration.

For posters and infographics, prioritize scan order and factual integrity. For logos, produce original geometry, test small-size legibility, and avoid confusing similarity to existing marks. Preserve supplied text exactly unless editing is requested.

Render before delivery. Inspect at 100% and target display size for clipping, awkward line breaks, alignment, margins, raster quality, and PDF font/image embedding. Revise until clean. Deliver source plus requested PNG/PDF exports and note any substituted fonts or missing assets.
