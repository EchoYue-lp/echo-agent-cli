---
name: algorithmic-art
description: 使用 p5.js 创建算法艺术——生成式视觉、粒子系统、流场
metadata:
  category: design
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [art, generative, creative, p5js]
triggers:
  - 艺术
  - 生成艺术
  - 粒子
  - generative art
  - p5js
allowed-tools: [bash, read, write]
---
# Algorithmic Art

Create an original generative artwork whose visual concept is expressed through an intentional system, not a random collection of particles.

Define the concept, composition, palette, motion behavior, interaction, and output dimensions before implementation. Choose an algorithm that supports the concept: flow fields, agents, recursion, noise, tiling, cellular systems, or geometry. Expose a small set of meaningful parameters and use a seed when reproducibility matters.

Implement with p5.js or the project's existing canvas stack. Keep animation stable across common frame rates and pixel densities. Provide pause/reset/export controls when useful. Avoid copying a living artist's signature style; translate requested influences into general visual properties.

Run the piece, inspect actual frames at target sizes, and revise empty areas, clipping, contrast, performance, and repetitive artifacts. Deliver runnable source plus a representative exported image or animation and the seed/parameters used.
