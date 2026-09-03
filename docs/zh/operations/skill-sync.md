# EKO Skill 管理与上游同步

## 概述

EKO SkillsHub 支持从 git remote 安装和更新技能。内置技能随产品升级更新；
用户安装的技能通过上游同步命令管理。

`/skills install` 也识别 Agent Plugins 1.0 根 `plugin.json`：先复用 framework
manifest/Skill validator 全量预检，再把 `skills/` 面作为一个 staging 目录原子安装并启用
全部 Skill。每个 Git Skill 保存精确 `skills/<name>` subdir，后续可独立同步；包含
`mcp.json` 的插件包暂不由 Skill 安装入口处理。若目标插件目录已存在，安装会要求先显式
卸载，避免在没有 owner marker 时覆盖用户已有目录。

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

管理技能的启用状态与 baseline，位于
`~/.eko/enabled-skills.json`。当前 version 2 形状如下：

```json
{
  "version": 2,
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
- `baseline`: 仅对 methodology 技能有效；当前只允许
  `verification-before-completion` 正文进入可替换 system-context projection
- 旧文件的 generation、operation identity、content identity 与 repair debt 字段被忽略
- upstream sync 的失败只在当次 typed receipt 中报告，不保留自动重放状态
- 首次启动自动生成默认配置（1 个方法论 baseline-on）

内置和用户 Skill 使用 framework `SkillDocument` 单一解析 API；SkillsHub 只负责 EKO 的
安装、启停、上游记录和 surface 投影，不复制 frontmatter parser 或 activation runtime。

## SKILL.md 官方标准格式

EKO 只接受 agentskills.io 官方 frontmatter，不引入任何私有扩展命名空间：

```yaml
---
name: my-skill                  # 必填：kebab-case，1-64 字符，等于目录名
description: >-                 # 必填：≤1024 字符；写清"做什么"和"何时用"，
  一行描述，包含路由关键词。        # 路由是 description-driven
license: MIT                    # 可选
compatibility: Requires poppler # 可选：环境要求，≤500 字符
allowed-tools: shell read_file  # 可选：空格分隔字符串（不是 YAML 列表）；空则省略
metadata:                       # 可选：string → string 映射
  category: methodology
  author: author-name
---
# 正文——完整指令
```

- Skill 文件不定义 Hooks。Hooks 属于 application/plugin configuration；frontmatter 出现
  `hooks:` 会作为非标准字段直接解析失败。
- "catalog 可发现"与"runtime active"不是同一状态：SkillsHub/catalog 能列出全部随附
  Skill，只有 `enabled-skills.json` 允许的 entry 才注册 descriptor 与 LLM 路由候选；Hooks
  继续由 application/plugin configuration 负责。
- 校验门禁：framework `validate_skill_dir`（`skills-ref validate` 的进程内等价物）；
  `cargo test -p echo-agent-app-core --lib skills_hub::catalog_gate` 遍历 `skills/`
  断言零违规且 `BUILTIN_SKILL_NAMES` 与磁盘一致。

`enabled` 是进程级全局策略，文件是唯一持久启用事实。GUI、TUI、CLI/JSONL 和
channel 都进入 `ExtensionControlService`，使用原子写与同一即时 settlement。
JSONL 输出 journaled typed `ExtensionReceipt`，不把 Skill slash command 交给模型。

enable/disable/refresh 返回 `SkillSyncReceipt`；install、uninstall 和 upstream sync 分别用
`SkillInstallSettlementReceipt`、`SkillUninstallSettlementReceipt` 与
`SkillArtifactSyncReceipt` 保留相同 settlement，不能把 artifact 成功与 runtime degraded
压成一个成功字符串。install receipt 的 `installed_names` 列出单 Skill 或插件包内全部
已安装并启用的 Skill。

## 直写直同步合同(2026-09 简化,ADR 0036)

Extension authority 直接升级现有文件,不建立第二个 Skill store。schema 只保留
平铺的 Skill map(`{category, enabled, baseline}`),原子写;旧文件遗留的
generation / repair debt 字段被直接忽略。配置损坏或不可读时回退默认启用集
(fail-open)并记录 warn 日志。

每个变更操作(enable/disable/install/uninstall/sync/refresh)统一走:

```text
获取 extension mutation 锁
  -> 读取 enabled-skills.json
  -> 修改条目
  -> 原子写
  -> reconcile 所有运行时目标(内置目录 + 用户/插件 Skill)
  -> 返回 Settled 或 Degraded
```

崩溃窗口内(文件已写、运行时未同步完成)的最坏情形,由下一次 skill 操作或应用
启动补齐收敛;不保留精确重放状态。

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
替换当前技能。检测到本地修改时默认不覆盖;只有显式 `--force` 才会替换。同步完成后，
GUI、TUI、CLI/JSONL 和 channel 都通过 Extension authority 刷新 runtime target。refresh
重新读取当前 flat policy 并 reconcile 所有运行时目标，返回同一个 `SkillSyncReceipt`
即时 settlement。

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

`metadata` 的值必须都是字符串。指令写在 Markdown 正文，支持文件放在 Skill 目录；
EKO 不再使用顶层 `version`、`author`、`tags`、`instructions` 或 `resources` 旧字段。

不自动安装——只探测并提示。
