---
name: writing-skills
description: 创建、编辑和验证技能文件——编写符合 agentskills.io 官方规范的 SKILL.md 指南
metadata:
  category: methodology
  source: superpowers
  upstream-version: 6.0.3
  author: obra
  tags: skill-authoring, documentation
---

# Writing Skills

Guide for creating effective SKILL.md files that follow the agentskills.io specification. Bundled skills use **standard fields only** — no vendor extension namespace.

## Official Frontmatter Layout

Only these top-level fields are official. Anything else at the top level fails validation:

```yaml
---
name: my-skill                  # required: kebab-case, 1-64 chars, matches directory name
description: >-                 # required: ≤1024 chars; say WHAT it does and WHEN to use it
  One-line description with routing keywords.
license: MIT                    # optional
compatibility: Requires poppler # optional: environment requirements, ≤500 chars
allowed-tools: shell read_file  # optional: space-separated string (NOT a YAML list)
metadata:                       # optional: string → string map for extra data
  category: methodology
  author: author-name
---
# Body — full instructions
```

## Field Rules

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Unique kebab-case identifier; must equal the skill directory name |
| `description` | Yes | What + when; drives catalog display and intent routing; ≤1024 chars |
| `allowed-tools` | No | Space-separated string of pre-approved tools; omit when empty |
| `metadata.category` | Recommended in EKO | One of: methodology/development/document/design/research/automation; optional in the official spec |
| `metadata.*` | No | Extra string metadata (source, author, tags, version…) |

Routing is **description-driven**: put the concrete user phrases and scenarios that should
trigger this skill into `description` (the spec's own guidance). Do not invent private
frontmatter fields for routing.

## Hooks

The official Skill file format does not define per-skill Hooks. Keep lifecycle
Hooks in the host application's configuration or plugin Hook component, not in
`SKILL.md` or an adjacent private sidecar.

## Workflow

1. **Clarify the capability** — what task, what inputs, what deliverable. If an existing skill already covers it, extend that skill instead of creating a near-duplicate.
2. **Draft the body first** — frontmatter is metadata; the value is in the instructions. Structure: contract/inputs → ordered steps → failure handling → delivery/quality criteria.
3. **Write progressive disclosure** — keep `SKILL.md` under 500 lines; move deep detail to `references/`, executable helpers to `scripts/`.
4. **Write a routing-ready description** — concrete "when to use" scenarios and user phrases; avoid generic wording that collides with other skills.
5. **Validate** — run the catalog validation gate (`cargo test -p echo-agent-app-core builtin_skill_catalog` in echo-agent-cli, or `SkillDocument::parse` on the file). Fix every violation.
6. **Register** — new builtin skills must be added to `BUILTIN_SKILL_NAMES` in `echo-agent-app-core/src/skills_hub/enabled_skills.rs`.

## Quality Check

- Frontmatter passes official validation (standard fields only, no vendor namespace)
- Body has concrete steps, failure handling, and delivery criteria — not just principles
- Description carries the when-to-use scenarios that route to this skill
- Resources referenced by the body exist in `references/` / `scripts/`
- Anti-patterns documented (what NOT to do)
