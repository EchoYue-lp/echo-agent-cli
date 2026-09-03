# ADR 0036: Skill Policy Simplification — Remove Durable Settlement Machinery

## Status

Accepted (2026-09-03)

Supersedes the durable-settlement half of
[ADR 0032](./0032-enabled-skill-runtime-authority.md); the core stance of
0032 — enabled-skills.json as the runtime activation authority — is retained.

## Context

ADR 0032 introduced a durable desired-state machine for
`~/.eko/enabled-skills.json`: generation CAS (`desired_generation` /
`settled_generation`), operation-idempotency dedup (`operation_identities`),
content fingerprints (`content_identity`), and crash-recovery replay
(`repair_debt` with target failures, artifact removals/syncs/enablements) —
roughly 3,000 lines of implementation plus 16 settlement tests.

EKO is a local, single-user desktop assistant (see AGENTS.md): no concurrent
tenants, no cross-process write contention, no distributed reconciliation.
A corrupted JSON file failed closed and silently disabled every built-in
skill — the same mistake as forcing web-service threat models onto a local
app. The benefit (exact recovery into the "file committed, runtime not yet
settled" intermediate state) is far below the maintenance cost.

## Options

1. Keep the machinery, flip fail-closed to fail-open. Minimal diff, but the
   CAS/debt paths and their tests remain a tax on every future change.
2. Remove it entirely; write-and-reconcile directly (chosen).
3. Keep only operation-identity dedup. The UI already guards double-submits;
   keeping dedup keeps half the machine alive.

## Decision

Option 2:

- `EnabledSkillsConfig` keeps only `{version, skills: {name → {category,
  enabled, baseline}}}`; stale generation/repair_debt fields in existing
  files are ignored by serde (no migration; dev-stage product).
- Parse/read failures fall back to the default active set (fail-open) with a
  warn log.
- All five mutation paths (enable/disable/install/uninstall/sync) share
  `reconcile_skill_runtimes`: resolve the desired set → per-target builtin +
  user/plugin reconcile → immediate receipt.
- `SkillSyncReceipt` shrinks to `{operation_id, idempotent, status,
  target_receipts}`; `idempotent` now means "this operation changed nothing".
- `SkillOperationIdentity`, `SkillRepairDebt`, `SkillRepairTargetDebt`, and
  `SkillArtifactSyncDebt` are deleted along with their TS bindings.
- Kept: the extension mutation mutex, product_data_io settlement flows
  (caller-independent settlement), the generic operation identity in
  subagent_control (not skill-specific), and the curator/skill-authoring loop.

## Rationale

Worst case inside the crash window: JSON written, some agents not yet
reconciled. The next skill operation or app start
(`reconcile_enabled_skills_on_load`) converges the state; one restart, no
precise debt replay required. Trading "tolerable one-restart convergence"
for 3,000 deleted lines fits the local-personal-assistant positioning.

## Impact

- `echo-agent-app-core`: skills_hub/enabled_skills.rs, extension_control/*
  (skills, service, types, tests), extension_commands.rs, state/app_state.rs,
  runtime.rs.
- Frontend SkillSyncReceipt bindings and SkillsPanel settlement toasts.
- Companion changes in the same refactor: runtime-resolved builtin skills
  root, TUI `/skill` + GUI activate button, built-in catalog 39→24.
