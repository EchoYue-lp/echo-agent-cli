# ADR 0033: Skill Catalog Contraction and Official SKILL.md Standardization

## Status

Accepted; catalog and baseline inventory amended on 2026-09-03

## Context

The bundled EKO skill catalog carried routing and runtime semantics in private
frontmatter extension fields (top-level `triggers:`, `hooks:`, `shell:`,
`paths:`, `sandbox:`, `depends_on:`). This is incompatible with the
[agentskills.io specification](https://agentskills.io/specification):

- official top-level fields are only `name`, `description`, `license`,
  `compatibility`, `metadata`, and `allowed-tools` — the latter a
  space-separated string;
- the official `skills-ref validate` flags non-official top-level fields, so
  EKO skills cannot circulate in standard toolchains;
- `metadata` is specified as a string → string mapping and must not carry
  structured extensions.

The catalog also contained duplicated capabilities (global skill-selection
logic, `deep-research` overlapping `web-search`) and hollow guidance
(`writing-skills`).

Migrating the extension fields into a `metadata.echo-agent.*` namespace was
considered and rejected: **only the standard format is allowed** — no private
frontmatter extension concepts.

## Decision

1. **SKILL.md uses official standard fields only.** Allowed top level:
   `name`, `description`, `license`, `compatibility`, `metadata`
   (string → string), `allowed-tools` (space-separated string; omit when
   empty). The framework parser fails closed on non-official top-level
   fields (`deny_unknown_fields` parse error; the loader skips the skill with
   a warning).
2. **No vendor namespace.** `metadata` holds string values only; LLM intent
   routing is **description-driven** — put when-to-use scenarios and keywords
   into `description` (the spec's own guidance). The `SkillDescriptor` fields
   `triggers` / `paths` / `depends_on` / `sandbox` / `shell` remain programmatic
   API surface with no file-based source; the keyword fast path is reserved for
   those programmatic descriptors.
3. **Skill files contain no Hook extension.** The official format has no
   per-skill Hook field or sidecar. Hook actions remain available through the
   host application's HookRegistry and plugin Hook components. The documented
   `HookAction::ActivateSkill` wire name is `activate_skill`.
4. **Validator gate.** The framework provides `validate_skill_markdown` /
   `validate_skill_dir` (the in-process equivalent of `skills-ref validate`);
   the `skills_hub::catalog_gate` test in `echo-agent-app-core` walks
   `skills/`, asserts zero violations across bundled skills, and asserts
   `BUILTIN_SKILL_NAMES` matches the on-disk directory set exactly.
5. **Catalog contraction (41 → 39):**
   - removed `using-superpowers` (duplicated global skill-selection logic);
   - removed `deep-research` (duplicated `web-search`; its unique
     contributions — claim decomposition, synthesis-by-claim, read-before-
     citing — were folded into `web-search`'s deep-research mode);
   - strengthened `writing-skills` (now teaches the official layout and this
     repository's workflow) and `mcp-builder` (added an ordered workflow and
     failure handling).
6. **The evolution subsystem follows the standard**: `SkillDraftGenerator`
   emits standard fields only (trigger patterns stay in curator state);
   `SkillMerger` persists only the merged `allowed-tools` string while the
   trigger/path union stays on the in-memory descriptor.
7. **Canonicalized path boundaries**: `builtin_skills_root()`,
   `ActiveSkillLoadPolicy`, and `reload_skills_from_dir` canonicalize paths
   exactly like the loader, removing symlink prefix mismatches that could
   bypass the activation policy or silently no-op a reload.
8. **ADR 0003 remains a historical snapshot**: its 25-entry / 14-superpowers
   inventory records the catalog at that time and is not retro-edited; the
   live inventory is `/skills list` plus the catalog gate.
9. **Second contraction in 2026-09 (39 → 24):** removed `coding`,
   `translation`, `doc-writing`, and `web-search`, whose behavioral guidance is
   covered by the base prompt, plus 11 vendored design/automation/research
   examples installable from `anthropics/skills`. The default-active set moves
   from 8 to 5 and the methodology baseline from 4 to 1, retaining only
   `verification-before-completion`. Bounded workspace profile prompts and the
   remaining domain Skills own workspace-specific behavior.

## Consequences

- Bundled skills are compatible with the official ecosystem and can be
  validated with `skills-ref validate` or the in-process gate.
- Keyword routing no longer comes from files: the `KeywordClassifier` word
  list is empty and routing relies on the LLM intent classifier reading
  descriptions (DirectAnswer/Fallback without an LLM).
- The bundled catalog contracts first from 41 to 39 and then to 24 in 2026-09;
  `BUILTIN_SKILL_NAMES`, `DEFAULT_ACTIVE_BUILTIN_SKILLS`, the methodology
  baseline, the CHANGELOG, and this ADR stay in sync.
- Third-party user skills written in the old private format fail to parse
  with an explicit discovery diagnostic — per this project's
  no-backward-compatibility rule, no migration shim is provided.
