# ADR 0038: Built-in Skill Creator and Plugin Creator

## Status

Accepted

## Context

EKO already has standard Skill parsing, discovery, enablement, and a user Skill
root. It also has an Agent Plugins 1.0 manifest, scaffold, validation, install,
and reload runtime. The protocol knowledge was fragmented across code, commands,
and maintainer documentation: `writing-skills` primarily described bundled
catalog maintenance, while PluginRuntime exposed `/plugins init` and
`/plugins validate` without a creator skill that taught the Agent EKO's format.

Mature implementations converge on the same pattern. OpenAI Codex ships
`skill-creator` and `plugin-creator` as system skills that teach directory
contracts, progressive disclosure, scaffolding, and validation. Claude Code
documents a Skill as `SKILL.md` plus optional supporting files and uses the
description for discovery. The Agent Skills specification defines the common
frontmatter contract. These systems teach extension protocols through a Skill
and reuse host file and validation capabilities instead of adding a creator
state machine.

## Options

1. Keep `writing-skills` and add both creators. This preserves the old name but
   leaves three overlapping routing and maintenance surfaces.
2. Replace `writing-skills` with `skill-creator`, add `plugin-creator`, and reuse
   the existing authorities (chosen).
3. Add separate `skill_create` and `plugin_create` stores and executors. This
   would duplicate the existing filesystem, Skill validator, and PluginRuntime
   authorities without a corresponding benefit.

## Decision

- Replace bundled `writing-skills` with `skill-creator`, covering creation,
  updates, resource organization, personal paths, and bundled catalog changes.
- Add `plugin-creator`. EKO uses root `plugin.json` with the Agent Plugins 1.0
  schema, not Codex's private `.codex-plugin/plugin.json` layout.
- Enable both by default so their names and descriptions remain discoverable.
  Neither is a baseline Skill; bodies load only for matching creation work.
- Framework `SkillDocument` / `validate_skill_dir` remains the only Skill format
  authority, and PluginRuntime remains the only plugin scaffold and validation
  authority. The application adds three thin model tools: `skill_validate`,
  `plugin_scaffold`, and `plugin_validate`. They share the same authorities as
  `/plugins init` and `/plugins validate`; neither the skills nor adapters own
  another validator, store, or lifecycle.

## Placement

- Generic mechanisms: Agent Skills parsing, resource discovery, script
  execution, and Agent Plugins manifest/prepared generations stay in
  `echo-agent`.
- EKO policy: `~/.eko/skills`, default enablement, commands, themes, output
  styles, monitors, and install/reload projections stay in `echo-agent-cli`.
- Adapter boundary: creator skills describe existing commands and paths; the
  three model tools only convert parameters/results and call framework or
  PluginRuntime APIs.

## Consequences

- The bundled catalog changes from 24 to 25 Skills (one rename and one
  addition), defaults from 5 to 7, and the baseline remains only
  `verification-before-completion`.
- No `writing-skills` alias is retained. EKO is still in development and does
  not preserve a stale duplicate entry.
- GUI, TUI, CLI, and channels share the same Agent bootstrap and catalog, so no
  surface-specific implementation is required.
- Creator tools are registered by the common Agent factory and stay deferred
  behind `tool_search`, preserving the ordinary first-turn schema budget.

## References

- [Agent Skills specification](https://agentskills.io/specification)
- [Claude Code: Extend Claude with skills](https://code.claude.com/docs/en/skills)
- OpenAI Codex bundled `skill-creator` and `plugin-creator` implementations
  inspected from the local installation and OpenAI skills repository on
  2026-09-04.
