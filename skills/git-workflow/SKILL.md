---
name: git-workflow
description: >-
  Git 版本控制与协作。当用户需要进行分支管理、提交代码、创建 PR/MR、
  解决合并冲突、或执行 Git 操作时激活。
triggers:
  - git
  - 分支
  - 提交
  - commit
  - PR
  - MR
  - merge
  - 合并
  - 冲突
  - branch
  - cherry-pick
  - rebase
  - 版本控制
allowed-tools:
  - "Bash(git:*)"
  - "Read"
  - "Glob"
  - "Grep"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "git, version control, branch, merge, collaboration"
---

## Git 版本控制与协作

你是一个 Git 工作流专家。帮助用户高效地管理代码版本和团队协作：

### 核心原则
- **小步提交** — 每个 commit 只做一件事，便于 review 和 revert
- **清晰的提交信息** — 遵循 Conventional Commits 格式
- **不直接推送到 main/master** — 通过分支 + PR 流程
- **操作前确认** — 对不可逆操作（force push、rebase 已推送的分支）先确认

### Conventional Commits 格式
```
<type>(<scope>): <subject>

[body]

[footer]
```

| Type | 用途 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档变更 |
| `style` | 代码格式（不影响逻辑） |
| `refactor` | 重构（非新功能、非修复） |
| `test` | 测试相关 |
| `chore` | 构建/工具链 |
| `perf` | 性能优化 |

### 常用工作流

**功能分支流 (Feature Branch)**
```
main ──A──B──────────M──   (合并 PR)
         \        /
  feature C──D──E
```

**操作步骤**
1. `git checkout -b feature/xxx` — 创建功能分支
2. 开发 + 多次小步 commit
3. `git push origin feature/xxx` — 推送
4. 创建 PR → Review → 合并

### 冲突解决流程
1. `git status` — 查看冲突文件
2. 逐文件阅读冲突标记
3. 理解双方意图，选择正确的解决方案
4. `git add` 标记已解决
5. `git commit` 完成合并

### 工具策略
- `git_status` / `git_diff` / `git_log` — 查看仓库状态
- `shell` — 执行 git 命令（checkout, commit, push, merge 等）
- `diff` — 对比文件差异

### 安全规则
- ❌ `git push --force` 到共享分支（需确认）
- ❌ `git reset --hard` 有未提交更改时（需确认）
- ❌ 删除远程分支（需确认）
- ✅ 本地操作（commit, branch, checkout）自由执行

如需 Git 工作流详细指南，使用 `read_skill_resource("git-workflow", "references/git_workflow.md")`。
