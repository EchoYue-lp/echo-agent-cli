# ADR 0044: EKO Skill Actions Use the Framework Mutation Authority

## Status

Accepted

## Context

The latest framework [ADR 0069](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0069-skill-lifecycle-mutation-authority.md) makes `SkillMutationAuthority` the owner of Skill file and Curator mutations. Former direct Curator methods and unapproved Draft/Merge/Patch calls are no longer application APIs. EKO still exposes explicit GUI and CLI Skill actions, and its ProductData I/O flow must retain work after a caller disconnects.

## Options

1. Reimplement Curator writes in EKO. Rejected: it creates a second lifecycle authority without journal recovery or exact approval.
2. Remove Skill actions. Rejected: ordinary Agent capabilities would disappear from existing surfaces.
3. Keep EKO's user/configuration policy and route exact mutations through the framework authority (chosen).

## Decision

- The framework owns Skill mutation CAS, file projection, Curator projection, audit binding, recovery, and rollback. EKO owns workspace selection, explicit user action or configured auto-draft policy, lifecycle thresholds, and runtime publication.
- `.eko/evolution/skill-change-log.jsonl` is the dedicated durable business ChangeLog bound to the Skill authority. The existing `change-log.jsonl` remains the broader evolution/candidate log; it is not silently rebound to a new Skill journal after historical records exist.
- GUI and CLI generate Drafts through a shared generation-bound candidate load, preview, digest-bound approval, and apply. Explicit merge and patch commands use the same framework preview/apply contract. EKO Curator metadata actions construct complete before/after state and submit it to the same authority.
- Curated Skill publication remains in the owned ProductData I/O lifecycle. Its new async operation is supervised through `ProductDataIoFlow::run_async`; caller drop cannot cancel the committed mutation or skip shutdown settlement.
- A plain `write_memory` now creates a Draft in the latest framework. EKO does not treat that write as an approved hot-memory publication; the existing hot projection changes only after a framework-approved activation.

## Consequences

- The old `CuratorStatus.last_run_at` file field is not mutated by framework Skill operations. EKO's maintenance command reports the exact transitions it applied; the legacy timestamp is not presented as a new settlement receipt.
- Framework examples are unchanged. `echo-website` does not document EKO's local Skill command contract, so no website update is needed.
