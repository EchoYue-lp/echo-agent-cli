---
name: finishing-a-development-branch
description: 实施完成后整理分支——合并、PR 或清理
allowed-tools: shell read_file read_artifact git_*
metadata:
  category: development
  source: superpowers
  upstream-version: 6.0.3
  author: obra
  tags: git, merge, cleanup
---

# Finishing a Development Branch

Use only when implementation and required verification are complete and the user has asked to integrate, publish, or clean up the branch.

## Options

1. **Merge directly** — for small, well-tested changes on personal branches
2. **Create a PR** — for shared branches or changes needing review
3. **Clean up** — remove worktree/branch only after confirming the work is integrated or explicitly unwanted

## Process

1. Verify all tests pass
2. Run linting and formatting
3. Check git status — no unexpected changes
4. Check repository-specific merge, signing, worktree, and push rules
5. Choose the authorized integration method; ask before publishing or discarding work when authorization is not explicit
6. Execute and verify the resulting branch/PR/remote state

Never use destructive reset/checkout or force-push as cleanup shortcuts. Preserve unrelated changes and report any verification that could not be completed.
