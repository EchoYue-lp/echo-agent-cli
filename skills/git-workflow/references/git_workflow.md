## Git 工作流详细指南

### 分支命名规范

| 前缀 | 用途 | 示例 |
|------|------|------|
| `feature/` | 新功能 | `feature/user-auth` |
| `fix/` | Bug 修复 | `fix/login-crash` |
| `hotfix/` | 紧急修复 | `hotfix/security-patch` |
| `release/` | 发布准备 | `release/v2.1.0` |
| `refactor/` | 重构 | `refactor/db-layer` |
| `docs/` | 文档 | `docs/api-guide` |

### 常用 Git 命令速查

```bash
# 状态查看
git status                    # 工作区状态
git log --oneline -20         # 最近 20 条提交
git diff HEAD~3..HEAD         # 最近 3 次提交的变更
git blame <file>              # 逐行查看作者

# 分支操作
git branch -a                 # 所有分支（含远程）
git checkout -b feature/x     # 创建并切换
git branch -d feature/x       # 删除已合并的分支
git branch -D feature/x       # 强制删除（未合并）

# 提交操作
git add -p                    # 交互式选择暂存内容
git commit --amend            # 修改最后一次提交
git rebase -i HEAD~3          # 交互式变基（整理历史）

# 远程操作
git fetch --all               # 拉取所有远程更新
git pull --rebase             # 拉取并变基（保持线性历史）
git push -u origin feature/x  # 推送并设置上游
```

### 合并策略选择

| 策略 | 命令 | 适用场景 |
|------|------|---------|
| **Squash merge** | `--squash` | 功能分支提交较乱，合并为一条 |
| **Rebase merge** | `--rebase` | 保持线性历史，每个 commit 保留 |
| **Merge commit** | `--no-ff` | 保留合并记录，适合重要分支 |

### Release 流程
```
1. release/vX.Y.Z 分支从 main 切出
2. 在 release 分支上修 bug、改版本号
3. PR 回 main，打 tag: git tag vX.Y.Z
4. cherry-pick 必要修复到 main
```
