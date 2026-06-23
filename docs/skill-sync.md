# 技能上游同步指南

## 概述

EKO SkillsHub 支持从 git remote 安装和更新技能。内置技能随产品升级更新；
用户安装的技能通过上游同步命令管理。

## 技能分类

| 类型 | 位置 | 更新方式 |
|---|---|---|
| **内置技能** | `<echo-agent-cli>/skills/<category>/` | 随产品版本升级 |
| **用户安装技能** | `~/.echo-agent/skills/` | 上游同步（git pull） |

## enabled-skills.json

管理技能的启用状态和 baseline 标记，位于 `~/.echo-agent/enabled-skills.json`：

```json
{
  "version": 1,
  "skills": {
    "brainstorming": { "category": "methodology", "enabled": true, "baseline": true },
    "docx": { "category": "document", "enabled": false, "baseline": false }
  }
}
```

- `enabled`: 技能是否加载进 agent
- `baseline`: 仅对 methodology 技能有效。`true` = 正文注入 system prompt
- 首次启动自动生成默认配置（核心 4 个方法论 baseline-on）

## 上游同步

> **当前状态:未实现。** 下列 `check-updates` / `sync` 命令尚在规划中,
> 当前版本的 `/skills` 子命令仅支持 `list|search|install|uninstall|info|refresh`。
> 用户安装技能的更新暂需手动:删除后用 `install --git <url>` 重新安装。
> 此段保留作为后续实现的设计参考。

### 规划:检查更新

```bash
echo-agent skill check-updates   # 规划中,尚未实现
```

### 规划:同步

```bash
echo-agent skill sync --source superpowers   # 规划中,尚未实现
```

预期行为:git pull → 复制更新的 SKILL.md/scripts → 更新 last_synced。

## 安全约束(install --git 已实现的部分)

- `install --git <url>` 走 SSRF 校验:只允许 HTTPS URL,拒绝 SSH/git file:///私网 IP
- 安装操作需用户确认
- 不自动后台拉取

## 编写带依赖的技能

### PEP 723 内联依赖（Python）

```python
#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "defusedxml",
#     "lxml",
# ]
# ///
```

`uv run --script` 自动建临时环境装依赖，无需 venv/pip。

### 声明系统二进制依赖

在 SKILL.md frontmatter metadata 中声明：

```yaml
metadata:
  requires-binaries: "soffice, pdftoppm"
  requires-python-packages: "defusedxml, lxml"
```

不自动安装——只探测并提示。
