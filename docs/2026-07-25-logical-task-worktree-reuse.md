# Logical-Task Worktree Reuse

## Outcome

EKO now owns at most one `eko-fork-*` worktree for each logical writer
`PlanTask` in a `TaskRun`. Retry attempts keep distinct Subagent execution ids
for events and audit records, but acquire the same stable worktree identity.

When a Subagent stops, EKO inspects Git state instead of inferring cleanup from
the execution result:

- no uncommitted files and no commits ahead of the authoritative checkout:
  remove the worktree and branch immediately;
- uncommitted files or unique commits: unlock and retain the checkout for retry,
  review, or integration;
- a retry of the same logical task: relock and reuse the retained checkout;
- a locked checkout: reject concurrent reuse because another execution still
  owns it.

`SubagentRun.execution_id` remains `{task_id}:{attempt}`. The new isolation key
is `{run_id}:{task_id}` and is not used as an event identity.

## Root Cause

TaskRuntime correctly generated a new execution id for each retry, but the
framework also used that attempt id as the worktree label. Finalization always
unlocked and retained the checkout. A failed, cancelled, or review-blocked
attempt therefore left one directory, and the next attempt created another.

The bug mixed two lifecycles:

- execution identity is attempt-scoped;
- filesystem isolation is logical-task-scoped.

## Industry References

- [Claude Code worktrees](https://code.claude.com/docs/en/worktrees) uses
  worktrees as session or explicitly selected Subagent environments, removes
  clean temporary checkouts, and retains changed work for inspection.
- [OpenAI Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)
  treats a managed worktree as the environment for a user-visible task and
  hands changes back through Git rather than creating an environment per
  internal retry.
- [Cursor background agents](https://docs.cursor.com/background-agent) and
  [GitHub Copilot coding agent](https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent)
  align an isolated environment and branch with a user-visible task/session.
- [Git worktree](https://git-scm.com/docs/git-worktree) provides the shared ref
  and lock primitives; locks represent active ownership, not completed history.

The cross-product pattern is task/session-level isolation, attempt-level event
history, automatic cleanup for empty environments, and retention only when
reviewable work exists.

## Architecture Boundary

The generic `echo-agent` framework adds only `ExternalRunContext.isolation_id`
and prefers it when naming a worktree or data workspace. This is a reusable
transport primitive and does not encode EKO branch policy.

The application layer owns the EKO-specific decisions:

- derive the stable key from `TaskRun + PlanTask`;
- name and acquire `eko-fork-*` branches;
- inspect Git content at finalization;
- remove clean resources;
- retain, review, and integrate changed resources.

This keeps TaskRuntime and local Git policy out of the reusable framework.

## Verification

Regression coverage proves that the framework prefers `isolation_id` over an
attempt-specific `execution_id`, retries reuse the same checkout and preserve
prior edits, clean finalization removes both checkout and branch, dirty
finalization retains and unlocks the checkout, and a cleaned checkout reaches
the integration boundary as `NoChanges` rather than an error.
