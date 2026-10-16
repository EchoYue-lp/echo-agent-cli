# echo-agent-cli semantic baseline

This directory records repository-scoped semantic facts for EKO application
conversation collaboration. The initial baseline is intentionally open: it
classifies the whole repository while modeling only the Conversation,
AgentRouter, foreground-turn, deletion, and surface boundaries needed by the
Side Conversation outcome.

Open closure means unmodeled capabilities remain explicit work, not that the
repository has no defects. High-risk changes start with semantic preflight,
map their first diff, and verify the affected boundary before completion.


## Historical source recovery

A Git-versioned source observation must remain recoverable from published main or
an explicit annotated `refs/tags/echo-semantic-source/<full-commit>` reference.
These tags preserve source objects; they are not releases, new audits, test reruns,
or claims that an old snapshot passes current validation. Keep historical
`observed_at` / `revision` and open, needs-review or stale conclusions unchanged.

Prefer an existing main ancestor only when the entire observed source scope has
identical bytes, modes and anchors, with any required source digest equivalence.
Matching a few files is insufficient. The 5cd snapshot's 13 referenced blobs are
absent from main's reachable object closure. The 8bd snapshot has only a partial
match at 6b; its Fork sources and repository source digest differ. Both original
snapshots therefore remain bound to their original commits.

| Commit | Annotated tag object | Source tree |
| --- | --- | --- |
| `5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4` | `4851eb87e5ad3feaf5a95a49b027faf22e5bef67` | `b2952ef7314bceacb78b84594ef3cd8b7c4227a3` |
| `8bd2a6f08117cde61037b583000c71ab820e73ce` | `b5dcc39e82fb2d8d80b74a097fd4c334c758a4bf` | `90550bd392b7705e3b436dad8f51a11a8c050cfc` |

Before publishing such a reference, inspect the old tree and required history for
raw logs, credentials and unrelated artifacts, and inspect that target version's
workflow triggers. These two commits were already public via the task branch and
PR8 history; the checked historical workflows exclude tag pushes and have no
release/deploy entry. Only the two explicit refs were pushed, without force or
bulk tag publication. Do not retarget or delete an evidence tag while maintained
semantic references depend on it.

Cold recovery starts with an empty repository, no alternates or borrowed object
store, and only remote main plus the necessary explicit evidence refs:

```sh
git fetch --no-tags origin \
  refs/heads/main:refs/remotes/origin/main \
  refs/tags/echo-semantic-source/5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4:refs/tags/echo-semantic-source/5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4 \
  refs/tags/echo-semantic-source/8bd2a6f08117cde61037b583000c71ab820e73ce:refs/tags/echo-semantic-source/8bd2a6f08117cde61037b583000c71ab820e73ce
```

Verify the exact tag object, peeled commit/tree, complete object connectivity,
and original source bytes, modes and anchors through the canonical checker.
The premerge cold probe recovered nine historical objects / 32 unique source
identities from main f57 plus these two tags, without task-branch or PR fetches.
This proves source availability only. After squash, repeat cold validation on the
actual resulting main and complete all required semantic and runtime gates.

This uses Git's standard [annotated tags](https://git-scm.com/docs/git-tag) and
[reachable-object retention](https://git-scm.com/docs/git-gc), with publication
side effects checked against [GitHub workflow ref filters](https://docs.github.com/en/actions/writing-workflows/workflow-syntax-for-github-actions).
No new source reader, validator or publication automation is introduced.
