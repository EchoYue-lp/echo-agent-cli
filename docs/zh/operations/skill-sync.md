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

管理技能的启用状态、baseline 与 durable settlement，位于
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
  },
  "desired_generation": "12",
  "settled_generation": "12",
  "content_identity": "sha256_...",
  "operation_identities": [
    {
      "operation_id": "7f74...",
      "command_identity": "sha256_...",
      "artifact_name": "paper-reader",
      "content_identity": "sha256_...",
      "generation": "12"
    }
  ],
  "repair_debt": null
}
```

- `enabled`: 技能是否加载进 agent
- `baseline`: 仅对 methodology 技能有效。`true` = 正文注入 system prompt
- `desired_generation`: 已原子提交的期望策略 generation
- `settled_generation`: 已完成 runtime fanout 的 generation
- `content_identity`: Skill policy 与所有 enabled `SKILL.md` 内容的 canonical hash
- `operation_identities`: bounded recent idempotency records；`command_identity` 只哈希该命令的
  参数，不会因其它 Skill 或 artifact 内容变化而误报冲突；install/uninstall 同时记录
  `artifact_name`，使旧 operation retry 能在触碰文件前返回
- `repair_debt`: committed generation 尚未收敛时的 bounded target failures、attempts 与
  artifact removal/sync/enablement actions
- upstream sync 仅把网络、Git 和可恢复 I/O 失败写入自动 repair debt；untracked Skill、
  无法解析的 source record，以及未使用 `--force` 的本地修改只在当次 typed receipt 中报告
- 首次启动自动生成默认配置（核心 4 个方法论 baseline-on）

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

`enabled` 是进程级全局策略，文件是唯一 durable desired fact。GUI、TUI、CLI/JSONL 和
channel 都进入 `ExtensionControlService`，使用 durable-first commit 与同一 settlement。
JSONL 输出 journaled typed `ExtensionReceipt`，不把 Skill slash command 交给模型。

enable/disable/repair 返回 `SkillSyncReceipt`；install、uninstall 和 upstream sync 分别用
`SkillInstallSettlementReceipt`、`SkillUninstallSettlementReceipt` 与
`SkillArtifactSyncReceipt` 保留相同 settlement，不能把 artifact 成功与 runtime degraded
压成一个成功字符串。

## Durable-first settlement 合同

Extension authority 直接升级现有文件，不建立第二个 Skill store。schema 在 Skill map
之外保存：

- monotonic desired generation；
- canonical content identity/hash；
- bounded recent operation identities，用于 duplicate/conflict 判定。

enable、disable、install 后 publication 以及 content-changing sync 使用同一个顺序：

```text
validate request + capture exact workspace generation
  -> canonicalize desired content
  -> detect duplicate/conflicting operation identity
  -> stage JSON beside enabled-skills.json
  -> sync staged file
  -> atomic replace
  -> sync parent directory
  -> publish committed desired generation
  -> fan out through specialist owners
  -> return Settled or Degraded
```

validation 或 durable write 失败是 pre-commit error。文件已经提交后，global seed、workspace 或
AgentPool fanout 失败必须返回 committed-but-degraded，不能用内存 rollback 把 durable commit
包装成“未发生”。

typed Skill receipt 保留：

- operation identity、content identity/hash 和 desired generation；
- durable commit marker、settled generation 与 `Committed`/`Settled`/`Degraded`
  settlement；
- committed `enabled-skills.json` file path；
- 每个 target 的 authority scope、workspace generation、specialist generation、
  settled/degraded status、changed entries 与 error；
- repair debt generation/content identity、attempts 与 artifact
  removals/syncs/enablements；
- 每个 `SkillRepairTargetDebt` 的 target、component、expected/observed generation、reason
  与 retryable。

structured surface 的外层 `ExtensionCommandReceipt` 再携带 request/operation identity 与
captured authority scope。

调用者被取消只丢失等待 future；`ExtensionControlService` 通过 ProductData owned flow
继续持有 accepted operation，直到 terminal settlement。application shutdown 先关闭新的
admission，再 join 已接受工作。

## 幂等与 repair

- 相同 operation identity + 相同 command identity 返回原 receipt 或根据 durable fact 重建
  receipt，即使无关 Skill 已经改变全局 content identity；
- 相同 operation identity + 不同 command identity 返回 typed conflict；
- 相同 content 不推进 generation，但会重试尚未收敛的 target；
- 旧 desired/workspace/specialist generation 不能覆盖更新 generation；
- workspace A -> B -> A 时，host generation 防止旧 A 的迟到结果污染新 A；
- global seed 和每个 workspace 保留最新 generation，existing/future pooled Agent 都从它创建。

repair debt 只表示 durable desired generation 与 observed live generation 的差异。bounded debt
snapshot 与 desired state 同存在 `enabled-skills.json`，但不是第二 authority。下一次 mutation
会先通过同一 coordinator reconcile；GUI/headless startup 在恢复 Agent delivery 前调用 shared
on-load reconcile，workspace create/switch settlement 也执行 repair。disabled artifact 删除失败、
upstream sync 部分失败、install artifact 已提交但 enable policy 尚未提交，分别进入同一 debt 的
`artifact_removals`、`artifact_syncs`、`artifact_enablements` 并由相同 reconcile 重试。单个
target 失败会保留其他 target 的真实 settlement，不把部分发布包装成成功。每次 runtime
fanout 前还会比较 durable generation/content CAS，旧 generation 不得发布到新 target。

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
重新计算 enabled `SKILL.md` content identity；内容变化即使没有改变 enablement，也会推进
desired generation 并返回同一个 `SkillSyncReceipt` settlement。

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
