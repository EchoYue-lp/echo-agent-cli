---
name: finishing-a-development-branch
description: 实施完成后整理分支——合并、PR 或清理
metadata:
  category: development
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [git, merge, cleanup]
triggers:
  - 完成开发
  - 合并分支
  - finish branch
  - PR
  - 清理分支
allowed-tools: [bash, git]
---

# Finishing a Development Branch

When implementation is complete, all tests pass, and you need to decide how to integrate the work.

## Options

1. **Merge directly** — for small, well-tested changes on personal branches
2. **Create a PR** — for shared branches or changes needing review
3. **Clean up** — discard the branch if the work is no longer needed

## Process

1. Verify all tests pass
2. Run linting and formatting
3. Check git status — no unexpected changes
4. Choose integration method
5. Execute and verify
