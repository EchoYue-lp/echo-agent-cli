# ADR 0041: EKO Consumes External Skills Only

## Status

Accepted

## Context

EKO previously maintained a bundled Skill catalog together with default
enablement, a methodology baseline, built-in root resolution, Tauri resource
packaging, source flags, and creator model tools. The local assistant therefore
owned Skill content, release packaging, and runtime policy at once. Every
content change required coordinated Rust, frontend contract, packaging, test,
and documentation updates.

The framework already provides standard `SKILL.md` parsing, validation,
progressive activation, and plugin generations. EKO already supports the user
root `~/.eko/skills/`, Git installation and sync, enablement, removal, and
PluginRuntime. Bundled content is not required for those capabilities; Skill
and creator workflows can be installed independently as Skills or plugins.

## Options

1. Keep and further reduce the bundled catalog. This minimizes migration but
   leaves EKO responsible for content selection, defaults, and releases.
2. Delete content but retain an empty built-in root, baseline, and source flags.
   This leaves unowned dual-track semantics that can attract content again.
3. Remove EKO's built-in Skill authority and retain external Skill and plugin
   capabilities only (chosen).

## Decision

1. Delete the repository-root `skills/` tree and its Tauri resource. EKO no
   longer resolves a built-in Skill root from source, environment, or bundle.
2. Remove default enablement, methodology baseline injection, built-in
   registration policy, catalog gates, and the `is_builtin` / `is_baseline`
   wire and UI semantics.
3. Upgrade `~/.eko/enabled-skills.json` to version 3 with `{enabled}` as the only
   per-entry field. Obsolete category, baseline, and settlement fields are
   ignored while existing external enablement choices remain. Corrupt input
   falls back to an empty set and no default entries are created.
4. SkillsHub lists and manages only independent Skills under `~/.eko/skills/`.
   Plugin Skills continue to come from framework prepared generations. Every
   surface still changes state through one Extension authority and immediately
   reconciles current runtime targets.
5. Remove the three model tools that existed only for the bundled
   `skill-creator` and `plugin-creator`. Existing PluginRuntime scaffold,
   validation, installation, reload, and `/plugins` commands remain available
   for independently installed guidance to use.
6. Framework `SkillDocument`, loading, validation, progressive activation, and
   plugin protocols remain unchanged. This decision narrows only EKO application
   content and policy.
7. Repository `.agents/skills/` files are development instructions, not product
   runtime resources, and are not packaged with EKO.

## Placement and Failure Handling

- Generic Skill and Plugin protocols, parsing, validation, and runtime
  registration belong to `echo-agent`.
- EKO owns the user installation root, enablement file, upstream sync, surface
  receipts, and plugin overlays.
- Concrete Skill instructions, creator guidance, and supporting resources
  belong to independent Skill or plugin repositories.

Enabling a missing Skill fails explicitly; disabling an existing configuration
entry is idempotent. If some runtime target fails after the file commit, the
receipt is `Degraded` and the next operation, startup, or workspace load retries
convergence. Removing bundled content never deletes user-installed Skills.

## Consequences

- This supersedes the bundled activation authority in ADR 0032, the bundled
  catalog portion of ADR 0033, the `{category, enabled, baseline}` schema in
  ADR 0036, and the built-in creator decision in ADR 0038.
- External Skill installation, enablement, removal, sync, and conversation
  activation remain, as does the complete PluginRuntime.
- Documentation and frontend surfaces no longer expose baseline or built-in
  markers; application packages no longer include a `skills/` resource.
- Framework SDK/API and examples require no change because this is an EKO
  product-content boundary reduction.
