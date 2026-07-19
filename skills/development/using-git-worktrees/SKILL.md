---
name: using-git-worktrees
description: 使用 git worktree 隔离功能开发——确保在独立工作区中工作
metadata:
  category: development
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [git, worktree, isolation]
triggers:
  - worktree
  - git worktree
  - 工作区隔离
  - 独立分支
allowed-tools: [bash, git]
---

# Using Git Worktrees

Use git worktrees when isolated writes materially protect the current workspace or enable safe parallel development. Follow repository-specific worktree rules and paths.

## When to Use

- Parallel writer tasks with disjoint ownership
- Risky or long-lived feature work that should not mix with current uncommitted changes
- Cross-repository work where each repository needs a dedicated branch/worktree

## Process

1. Inspect status, branches, existing worktrees, ignore rules, and the repository's required worktree location.
2. Create a clearly named branch/worktree without overwriting an existing path. Record any temporary dependency-path changes.
3. Work and verify inside the worktree. Do not modify the main checkout or another subagent's worktree.
4. Before integration, merge/reconcile the current main branch as required and restore portable relative dependency paths.
5. Verify the integrated result, then remove/prune the worktree and delete the branch only after confirming no unique work remains.

Do not assume the example command or directory layout fits every repository. Never delete a worktree or branch merely because it is not currently checked out.
