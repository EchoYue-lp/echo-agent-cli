---
name: skill-creator
description: Create, update, and validate EKO Agent Skills when the user asks to add reusable instructions, workflows, domain knowledge, or bundled scripts and references.
allowed-tools: shell read_file apply_patch skill_validate
metadata:
  category: methodology
  source: eko
  tags: skill-authoring, skill-validation
---

# Skill Creator

Create skills that encode useful, non-obvious procedures without constraining unrelated work.

## EKO Skill Contract

A skill is a directory whose only required file is `SKILL.md`. Add `scripts/`,
`references/`, or `assets/` only when they directly support the workflow.

```text
skill-name/
|-- SKILL.md
|-- scripts/       optional executable helpers
|-- references/    optional details loaded only when needed
`-- assets/        optional files copied into generated output
```

`SKILL.md` uses the Agent Skills standard. The only allowed top-level fields are
`name`, `description`, `license`, `compatibility`, `allowed-tools`, and
`metadata`.

```yaml
---
name: my-skill
description: What the skill does and the concrete requests that should activate it.
allowed-tools: shell read_file
metadata:
  category: development
---
```

- `name` is lowercase kebab-case, 1-64 characters, and matches the directory.
- `description` is at most 1024 characters and drives discovery. State both
  capability and activation scenarios.
- `allowed-tools` is one space-separated string, not a YAML list.
- `metadata` is an optional string-to-string map. Do not place structured
  runtime policy in it.
- Hooks belong to EKO/plugin configuration, not to `SKILL.md` or a private
  sidecar.

## Workflow

1. Inspect the target and existing skills. Extend an existing skill when it
   already owns the requested behavior.
2. Confirm the output location. For a personal EKO skill, default to
   `~/.eko/skills/<skill-name>/`; use a project path only when the user asks.
3. Write the smallest useful `SKILL.md`. Put shared purpose, decisions, and
   routing in it; move conditional detail to linked references.
4. Add deterministic scripts only when repeated logic or a fragile operation
   benefits from them. Run every added or changed script.
5. Validate the directory with `skill_validate`. If its schema is not visible,
   find it with `tool_search`. In an EKO source checkout, also run the app-core
   `builtin_skill_catalog` test for bundled skills. Fix every violation before
   asking EKO to install or refresh the Skill.
6. Re-read every changed file and report the created path plus any activation
   step. Never overwrite an existing skill with unrelated content.

## Bundled EKO Skills

Only repository maintainers should add a bundled skill under `skills/`. Such a
change must also update `BUILTIN_SKILL_NAMES`; default activation is a separate
product decision. Keep bundled skills within the repository prompt budget and
update the product documentation and changelog.

## Quality Bar

- Instructions change model decisions rather than repeat general knowledge.
- The description is specific enough to avoid unrelated activation.
- References are linked from `SKILL.md`, and every referenced resource exists.
- Failure handling and delivery criteria are explicit where the workflow has
  meaningful failure modes.
- No unfinished placeholders, private frontmatter fields, or byte-sensitive
  assumptions remain.
