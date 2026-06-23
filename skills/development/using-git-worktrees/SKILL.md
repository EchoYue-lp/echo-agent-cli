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

When starting feature work that needs isolation from the current workspace, use git worktrees to create an isolated workspace.

## When to Use

- Before executing implementation plans
- When working on features that need isolation
- Before making changes that could affect the main workspace

## Process

1. Create a worktree: `git worktree add -b feature/name ../path feature-name`
2. Work in the isolated directory
3. When done, remove: `git worktree remove ../path`
4. Clean up branch if not merged
