# EKO Skill 管理与上游同步

## 概述

EKO SkillsHub 支持从 git remote 安装和更新技能。内置技能随产品升级更新；
用户安装的技能通过上游同步命令管理。

Skill 的分类来自 `SKILL.md` metadata，不由目录深度决定。loader 递归扫描
`skills/**/SKILL.md`，因此仓库同时支持 `skills/<name>/` 和
`skills/<category>/<name>/`。当前清单以 GUI/TUI/CLI 的 `/skills list` 为准，文档不
冻结数量，避免新增或删除 Skill 后出现第二份过期目录。

## 技能分类

| 类型             | 位置                                  | 更新方式                        |
| ---------------- | ------------------------------------- | ------------------------------- |
| **内置技能**     | `<echo-agent-cli>/skills/<category>/` | 随产品版本升级                  |
| **用户安装技能** | `~/.eko/skills/`                      | 显式检查并通过 staging 原子同步 |

## enabled-skills.json

管理技能的启用状态和 baseline 标记，位于 `~/.eko/enabled-skills.json`：

```json
{
  "version": 1,
  "skills": {
    "brainstorming": {
      "category": "methodology",
      "enabled": true,
      "baseline": true
    },
    "docx": { "category": "document", "enabled": false, "baseline": false }
  }
}
```

- `enabled`: 技能是否加载进 agent
- `baseline`: 仅对 methodology 技能有效。`true` = 正文注入 system prompt
- 首次启动自动生成默认配置（核心 4 个方法论 baseline-on）

内置和用户 Skill 使用同一个 framework loader；SkillsHub 只负责 EKO 的安装、启停、
上游记录和 surface 投影，不复制 Skill parser 或 activation runtime。

## 上游同步

从 Git 安装时,EKO 在技能目录写入 `.eko-skill-source.json`,记录仓库 URL、
子目录、revision、内容哈希和同步时间。该记录不进入 `SKILL.md`,也不影响
技能加载。

### 检查更新

```bash
/skills check-updates             # 全部技能
/skills check-updates paper-reader
```

检查通过 `git ls-remote` 获取上游 HEAD。结果区分:已是最新、存在更新、检测到
本地修改、非 Git 安装和远程错误。GUI SkillsPanel 提供同一操作。

### 同步

```bash
/skills sync paper-reader
/skills sync all
/skills sync paper-reader --force
```

同步会先克隆到同文件系统的 staging 目录,验证 `SKILL.md`,计算内容哈希,再原子
替换当前技能。检测到本地修改时默认不覆盖;只有显式 `--force` 才会替换。同步
完成后 CLI、TUI、GUI 和 channel 都会刷新当前 Agent 的技能目录。

## 本地应用约束

- Git 地址只接受 HTTPS,拒绝明文 HTTP、SSH 和 `file://` 等明显错误输入。
- EKO 是用户自己的本地助理,允许用户配置可信的内网 Git 服务。
- 更新和同步均为显式命令,不自动后台拉取。
- Git 操作使用用户现有凭据并设置 120 秒超时。

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
