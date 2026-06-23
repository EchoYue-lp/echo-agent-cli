---
name: writing-skills
description: 创建、编辑和验证技能文件——编写 SKILL.md 的指南
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [skill-authoring, documentation]
triggers:
  - 写技能
  - skill
  - SKILL.md
  - 创建技能
  - 新技能
allowed-tools: []
---

# Writing Skills

Guide for creating effective SKILL.md files that follow the agentskills.io specification.

## SKILL.md Structure

```yaml
---
name: my-skill
description: One-line description
metadata:
  category: methodology
  source: superpowers
  upstream-version: "1.0"
  author: author-name
  tags: [tag1, tag2]
triggers:
  - 触发词1
  - trigger word
allowed-tools: []
---
# Body — full instructions
```

## Metadata Fields

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Unique kebab-case identifier |
| `description` | Yes | One-line summary (used in catalog) |
| `metadata.category` | Yes | One of: methodology/development/document/design/research/automation |
| `metadata.source` | No | Upstream origin (superpowers/anthropic/builtin) |
| `triggers` | No | Keywords for KeywordClassifier routing |
| `hooks` | No | Hook rules for lifecycle events |

## Best Practices

- Keep body under 5000 words for activation
- Use clear step-by-step instructions
- Include anti-patterns (what NOT to do)
- Add concrete examples
