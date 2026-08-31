# ADR 0028: Current Product Schema Authority

## Status

Accepted

## Context

EKO is still under active development and does not promise compatibility with
obsolete local schemas or command markers. Several production paths continued
to interpret retired formats after their replacements were already the sole
writers:

- `.eko/AGENTS.md` was renamed or read as auto-promoted rules even though
  `learned-rules.md` is the RulePromoter authority;
- root `.workspace.json` was treated as a readable workspace manifest beside
  `.eko/workspace.json`;
- cron prompts silently removed a `[plan]` prefix even though every prompt now
  enters the same TaskRuntime driver;
- `TaskRuntimeStore::open()` and a production `with_run_id` wrapper remained
  only to preserve old call shapes.

These paths obscured the current product contract and made unrelated user text
or files acquire hidden EKO semantics.

## Decision

1. Auto-promoted rules are read only from `.eko/learned-rules.md`.
   `.eko/AGENTS.md` is neither renamed nor interpreted. Repository-standard
   `AGENTS.md` and `AGENTS.override.md` outside `.eko` remain normal instruction
   sources.
2. `.eko/workspace.json` is the only readable workspace manifest. Open, list,
   detection, logging, deletion, and config discovery no longer read root
   `.workspace.json`.
3. Workspace creation still refuses to overwrite a directory containing
   `.workspace.json`. This is only a data-loss guard; the retired marker is not
   parsed, migrated, deleted, or accepted as a workspace authority.
4. Cron prompts are passed to TaskRuntime exactly as stored; only an
   all-whitespace prompt is rejected. `[plan]` has no marker semantics.
5. `TaskRuntimeStore::new()` is the sole default constructor. The run-id-only
   task-local wrapper is compiled only for tests; production callers provide
   the complete run context.
6. Recovery code that discards an incompatible checkpoint and rebuilds from
   the authoritative journal remains. Legacy worktree cleanup also remains
   because it preserves or safely exposes user Git changes instead of
   interpreting an old execution schema.

## Alternatives Considered

1. Keep all fallbacks until release. Rejected: development complexity would
   define the public contract and allow ambiguous files to affect execution.
2. Delete every historical check. Rejected: refusing to overwrite a directory
   with a retired marker prevents accidental user-data loss, and journal/Git
   recovery protects current authoritative data.
3. Migrate retired files automatically. Rejected: implicit rename or deletion
   changes user files and perpetuates migration code for schemas that were
   never released.

## Consequences

- Product input formats have one current interpretation and one owner.
- Old files remain untouched on disk but no longer affect runtime behavior.
- Current journal and Git recovery safeguards remain available without
  becoming compatibility authorities.
- No framework changes are required; all removed semantics were EKO product
  policy.
