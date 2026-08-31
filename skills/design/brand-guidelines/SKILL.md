---
name: brand-guidelines
description: 从用户提供的品牌资产中提取并一致应用品牌规范；用于配色、字体、标志、语气和跨资产一致性检查
metadata:
  category: design
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: brand, design, style-guide
---
# Brand Guidelines

Apply the user's actual brand system consistently across an artifact. Do not assume Anthropic or any other third-party brand unless the user explicitly names it and supplies or authorizes current source material.

## Contract

- Inventory available logos, color tokens, typography, spacing, imagery, iconography, voice, and existing product examples. Treat source files and current guidelines as authoritative.
- Convert the inventory into a compact design decision set: primary/secondary palette with contrast roles, type scale, spacing rhythm, component treatments, image direction, and do/don't rules.
- Preserve logo geometry and clear space. Do not redraw, recolor, distort, or place a mark on low-contrast backgrounds unless the source guidelines permit it.
- When guidelines are incomplete, make the smallest clearly labeled extension that matches existing artifacts; do not invent claims such as "official" colors.
- Verify consistency and accessibility in the rendered output across requested sizes.

Deliver the updated artifact and a concise record of the brand sources and any assumptions used.

## Workflow

1. Collect the supplied brand files and mark each item as authoritative, illustrative, or missing.
2. Extract semantic tokens rather than copying isolated values: surface, text, accent, danger, spacing, type, and imagery roles.
3. Apply the tokens to every reachable state, including hover, focus, disabled, error, and dark/light variants.
4. Record any extension as an assumption with an owner for later approval; do not silently create an official-looking rule.

## Delivery checklist

- Source assets, token values, and font licenses are listed.
- Logo clear space and minimum size are preserved.
- Text and non-text contrast are checked at the requested sizes.
- A short do/don't summary is included for later screens.
