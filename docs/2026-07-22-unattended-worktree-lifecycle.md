# Unattended Worktree Lifecycle Repair

## Outcome

EKO no longer creates an `eko-unattended-*` worktree when an unattended Agent
driver starts. The primary Agent remains in the authoritative checkout for
read-only discovery and planning. Workspace mutation is routed through the
existing formal PlanTask path, where a writer Subagent receives an
`eko-fork-*` worktree only when that writer is dispatched.

Successful or no-change writer integration removes the temporary worktree and
branch. Failed integration preserves the worktree for review and releases its
lock. Legacy `eko-unattended-*` resources remain visible through one shared
app-core review API used by GUI and TUI.

Implementation commit: `61c8350`.

## Audit Facts

The live repository audit on 2026-07-22 found:

- 8 linked `eko-unattended-*` worktree directories, each about 6.8 MiB.
- 9 matching branches, including one branch without a linked checkout.
- All 8 linked worktrees at commit `2e4800c`, while local `main` had advanced.
- All 8 linked worktrees still locked with an `in progress` reason.
- The retained resources were not deleted during this repair.

The former lifecycle had six separate defects:

1. `drive_agent_run` created the run worktree before the Agent performed any
   work, despite comments describing lazy creation.
2. The worktree path was attached only to the primary Agent invocation.
3. Formal mutating PlanTasks already used a different writer Subagent lifecycle
   and created `eko-fork-*` worktrees, so the run-level checkout was not the
   authoritative destination for planned writes.
4. The run-level handle was retained unconditionally at the end of the run,
   including runs with no changes.
5. The handle was dropped without releasing its `in progress` lock.
6. Backend merge/discard commands existed, but no product surface called them;
   the merge helper also failed to commit untracked or unstaged work before
   attempting integration.

The resulting directories were therefore usually empty bookkeeping artifacts,
not unfinished Agent edits.

## Industry References

- [Claude Code worktrees](https://code.claude.com/docs/en/worktrees) isolates
  parallel sessions and Subagents, removes clean temporary worktrees, retains
  changed worktrees for review, and releases stale ownership when the work
  ends.
- [OpenAI Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)
  treats managed worktrees as disposable isolated environments while keeping
  the local checkout authoritative; code moves back through an explicit Git
  handoff instead of assuming task completion changed the local checkout.
- [Git worktree](https://git-scm.com/docs/git-worktree) defines linked
  worktrees as separate working trees, indexes, and HEADs sharing one object
  database and ref namespace. Locks protect a worktree from pruning and must
  not outlive the process ownership they represent.

The common pattern is isolation at the actual execution boundary, explicit
integration into the authoritative checkout, automatic removal when there is
nothing to review, and retention only when user-visible work remains.

## Application Boundary

This remains an `echo-agent-cli` responsibility. Branch naming, the local
authoritative checkout, TaskRuntime liveness, review UI, and cleanup policy are
EKO product decisions. The reusable `echo-agent` framework already provides
the generic `WorktreeFactory` and per-invocation working-directory contract and
does not need an EKO-specific lifecycle or projection model.

## New Lifecycle

### New unattended runs

1. `UnattendedWriteMode::Worktree` and `Disabled` hide direct workspace
   mutation, shell, code execution, Git write, and nested background-run tools
   from the unattended primary Agent.
2. Read-only discovery can finish directly without creating a worktree.
3. Work that needs mutation must materialize and execute a formal plan.
4. A mutating PlanTask dispatches a writer Subagent. The existing
   `EkoWorktreeFactory` creates its `eko-fork-*` checkout at that dispatch
   boundary, after the runtime has established that a writer is required.
5. Review and integration stage uncommitted files, validate actual changed
   paths, protect overlapping local dirty files, run `merge-tree` preflight,
   and create the final merge commit.
6. Success, already-integrated, and no-change outcomes clean the writer branch
   and worktree. Failure preserves and unlocks it for review.

Literal creation inside the first file-tool call was rejected because the
Subagent working directory must be fixed before any tool invocation. Writer
dispatch is the earliest reliable application-level boundary and reuses the
already-correct formal execution path instead of introducing another tool
proxy or framework-specific state machine.

`UnattendedWriteMode::InPlace` remains an explicit user choice and keeps direct
tools available.

### Legacy review queue

The legacy manager now:

- lists every `eko-unattended-*` branch, including orphan branches;
- reports active process ownership separately from stale persisted run status;
- reports lock reason, uncommitted files, commits ahead of current `HEAD`, and
  whether reviewable changes exist;
- refuses merge, discard, or cleanup for a process-active run;
- unlocks inactive stale worktrees;
- removes only branches with no uncommitted files and no commits ahead of the
  authoritative checkout;
- preserves every changed entry;
- materializes a temporary checkout when an orphan branch with commits needs
  integration;
- commits untracked and unstaged work before running the same safe integration
  boundary used by writer Subagents.

GUI exposes this as the EKO review queue in the Worktree panel with merge,
discard, and `Clean unchanged`. TUI exposes the same app-core operations as:

```text
/worktrees list
/worktrees cleanup
/worktrees merge <run-id>
/worktrees discard <run-id>
```

## Verification

The repair added regression coverage for direct-tool routing, active-run
protection, stale-lock release, clean cleanup, changed-worktree retention,
untracked-file integration, orphan-branch integration, porcelain lock parsing,
and TUI command projection.

The full Rust workspace, GUI-only feature combination, frontend test suite,
formatters, Clippy, TypeScript build, and production Vite build passed before
the implementation commit.
