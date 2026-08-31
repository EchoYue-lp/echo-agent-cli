# ADR 0032: Enabled Skills Own Runtime Activation

## Status

Accepted

## Context

EKO shipped a large bundled Skill catalog and loaded every bundled `SKILL.md`
into each Agent at startup. `enabled-skills.json` only selected methodology
baseline injection, so disabled Skills still registered descriptors,
progressive activation entries, and IntentRouter candidates. A Skill could
therefore be disabled in product state while remaining executable in the Agent
runtime. Per-skill Hooks are not part of the official Skill file format.

The framework already separates discovery/catalog from registration through
`SkillLoadPolicy`; the application owns the product lifecycle file and must
provide the policy.

## Decision

1. `SkillsHub` and install/update commands remain the catalog and artifact
   authority. They may list or install a Skill without activating it.
2. `ActiveSkillLoadPolicy` is the EKO registration policy. For the bundled
   application `skills/` root it reads `enabled-skills.json`; user and plugin
   Skills additionally pass the existing curator/draft/workspace policy.
3. A missing builtin entry uses the small shipped core bundle default. The
   default active bundle is `coding`, `brainstorming`,
   `systematic-debugging`, `verification-before-completion`, `writing-plans`,
   `git-workflow`, `web-search`, and `translation`. All other bundled Skills
   are opt-in.
4. Registration filters before descriptor insertion. Consequently disabled
   Skills do not register progressive activation/resource entries or
   IntentRouter keyword/LLM candidates. Hook configuration remains owned by the
   host application or plugin components.
5. Enable/disable/refresh reconciliation reloads the builtin root on the
   primary Agent, removes newly disallowed entries, and refreshes IntentRouter
   on the primary and every live pooled Agent. Future Agents receive the same
   policy during construction.
6. Dependency declarations remain explicit. A future bundled Skill with a
   dependency must enable the dependency as part of the same durable policy or
   be rejected before registration; activation must not silently bypass the
   enabled set.

## Consequences

- The prompt/catalog surface and runtime capability surface now agree with the
  user's enabled policy.
- The repository may keep rich optional Skills without paying their Hook and
  routing cost in every session.
- Users can still discover and install disabled Skills; enabling is the
  explicit runtime transition.
- Skill policy is product-specific and remains in EKO. The framework keeps the
  generic loader and `SkillLoadPolicy` contract.
