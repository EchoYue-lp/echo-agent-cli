---
name: canvas-design
description: 创建精美视觉设计——海报、LOGO、信息图等，输出 PNG/PDF
allowed-tools: shell read_file read_artifact apply_patch
metadata:
  category: design
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: design, poster, logo, infographic
---
# Canvas Design

Create a finished static visual whose hierarchy, typography, composition, and export quality match its real use.

Establish audience, message, dimensions, viewing distance, required copy, brand assets, and output format. Build one strong visual concept with a clear focal point and reading order. Use a deliberate grid, restrained type system, accessible contrast, and imagery that carries information rather than decoration.

For posters and infographics, prioritize scan order and factual integrity. For logos, produce original geometry, test small-size legibility, and avoid confusing similarity to existing marks. Preserve supplied text exactly unless editing is requested.

Render before delivery. Inspect at 100% and target display size for clipping, awkward line breaks, alignment, margins, raster quality, and PDF font/image embedding. Revise until clean. Deliver source plus requested PNG/PDF exports and note any substituted fonts or missing assets.

## Workflow

1. Write a one-sentence message and identify the primary viewer, viewing distance, and export size.
2. Sketch two composition directions, choose one, and define the reading order before adding decoration.
3. Lock typography, grid, contrast, and image treatment before polishing; keep supplied copy and data unchanged.
4. Export the requested formats, reopen them, and compare the rendered result with the source at full and target scale.

## Failure handling

- If an asset or font is unavailable, use a documented substitute and keep the layout adjustable.
- If copy does not fit, revise hierarchy or line breaks before shrinking type below a readable size.
- If a logo or data mark is ambiguous, request the missing source instead of inventing geometry.
