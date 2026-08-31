---
name: slack-gif-creator
description: 创建适用于 Slack 的动画 GIF 和表情
allowed-tools: shell read_artifact
metadata:
  category: design
  source: anthropic
  upstream-version: '1.0'
  author: anthropic
  tags: gif, slack, animation
  requires-python-packages: Pillow, imageio
---
# Slack GIF Creator

Create a short, readable looping GIF optimized for the user's actual Slack use case.

Confirm whether the output is a message GIF, reaction, or custom emoji; establish dimensions, duration, loop behavior, background transparency, text, and target file-size limit. Design for small display: one focal action, high contrast, few frames, and no fine detail or rapid flashing.

Use Pillow/imageio or existing project tooling. Preserve transparency where needed, choose a stable frame duration, optimize palette and changed regions, and avoid unnecessary resolution. For text, verify legibility at Slack display size and use fonts available in the workspace.

Render and inspect the loop, first/last-frame transition, transparency, dimensions, frame rate, and final byte size. Deliver the GIF plus source frames/script and report the verified dimensions, duration, and size.

## Workflow

1. Confirm whether the asset is a message GIF, reaction, or custom emoji and set a byte budget before rendering.
2. Storyboard one readable action with a clear first frame, peak frame, and loop-back frame.
3. Render at the final display size, then optimize changed regions and palette without sacrificing text or transparency.
4. Review the loop at normal speed and reduced preview size; remove flashing, tiny detail, and unreadable captions.

## Delivery checklist

- The output has the requested dimensions, duration, loop mode, and transparency behavior.
- The source script and any fonts/assets are included or identified.
- Final byte size and frame rate are measured, not estimated.
