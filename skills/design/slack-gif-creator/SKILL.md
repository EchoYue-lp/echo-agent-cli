---
name: slack-gif-creator
description: 创建适用于 Slack 的动画 GIF 和表情
metadata:
  category: design
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [gif, slack, animation]
  requires-python-packages: "Pillow, imageio"
triggers:
  - GIF
  - gif
  - 动画
  - slack
  - 表情
allowed-tools: [bash]
---
# Slack GIF Creator

Create animated GIFs optimized for Slack sharing.

## Features

- Text animations and effects
- Image sequence to GIF conversion
- Size optimization for Slack limits
- Custom emoji creation

## Requirements

- Python: `Pillow`, `imageio` (auto via `uv run`)
