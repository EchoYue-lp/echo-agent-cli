# EKO 外部 Skill 管理与上游同步

## 内容边界

EKO 不随应用捆绑 Skill，也不从源码树、Tauri resources 或默认 catalog 加载 Skill。
产品运行时只消费两类外部内容：用户安装到 `~/.eko/skills/` 的独立 Skill，以及 Plugin
generation 提供的 Skill。仓库根 `.agents/skills/` 只指导 EKO 开发，不进入产品运行时。

SkillsHub 负责独立 Skill 的安装、启停、卸载、上游记录和 surface 投影；PluginRuntime
负责完整插件包。两者都复用 framework 的 `SkillDocument`、manifest parser 与 validator，
不维护第二套 frontmatter parser 或 activation runtime。

`/skills install` 识别单个 Skill 仓库，也识别带根 `plugin.json` 的 Agent Plugins 1.0 包。
对插件包，它先完整预检 `skills/` 面，再以 staging 目录原子安装并启用其中全部 Skill。
包含 `mcp.json` 的插件包暂不由 Skill 安装入口处理；已有且没有 owner marker 的目标目录
不会被覆盖，必须先显式卸载。

## enabled-skills.json

`~/.eko/enabled-skills.json` 是外部 Skill 启用选择的唯一持久事实。当前 version 3：

```json
{
  "version": 3,
  "skills": {
    "paper-reader": { "enabled": true },
    "my-local-skill": { "enabled": false }
  }
}
```

- `enabled` 决定已安装的外部 Skill 是否进入 Agent runtime。
- 新安装的 Skill 默认启用；首次启动不生成任何默认 Skill 条目。
- 旧文件中的 `category`、`baseline`、generation、operation identity、content identity 与
  repair debt 字段会被忽略，已有条目的 `enabled` 选择继续生效。
- 配置损坏或不可读时回退空集合并记录警告，不猜测用户应启用哪些内容。

每个变更操作都经过同一个 Extension authority：

```text
获取 extension mutation 锁
  -> 读取 enabled-skills.json
  -> 校验外部 Skill 并修改条目
  -> 原子写
  -> reconcile 用户目录 Skill 到所有运行时目标（Plugin generation 独立管理）
  -> 返回 Settled 或 Degraded
```

GUI、TUI、CLI/JSONL 和 channel 使用同一服务。文件已写但某个 runtime target 同步失败时，
配置不回滚；下一次 Skill 操作、应用启动或 workspace load 会重新收敛。typed receipt 会分别
报告 artifact 结果与逐 target runtime settlement，不保存精确重放状态。

## SKILL.md 格式

EKO 只接受 agentskills.io 官方 frontmatter，不引入私有扩展命名空间：

```yaml
---
name: my-skill
description: >-
  说明这个 Skill 做什么以及何时使用。
license: MIT
compatibility: Requires poppler
allowed-tools: shell read_file
metadata:
  category: research
  author: author-name
---
# 完整指令
```

- `name` 必须是 1 至 64 个字符的 kebab-case，并与目录名一致。
- `description` 最长 1024 个字符，路由依据写在这里。
- `allowed-tools` 是空格分隔字符串，不是 YAML 列表。
- `metadata` 必须是 string 到 string 的映射。
- Skill 文件不定义 Hooks；Hooks 属于 application/plugin configuration。
- framework `validate_skill_dir` 是唯一目录校验权威。

## 上游同步

从 Git 安装时，EKO 在 Skill 目录写入 `.eko-skill-source.json`，记录仓库 URL、精确子目录、
revision、内容哈希和同步时间。该记录不进入 `SKILL.md`，也不影响加载。

```bash
/skills check-updates
/skills check-updates paper-reader
/skills sync paper-reader
/skills sync all
/skills sync paper-reader --force
```

同步先克隆到同文件系统的 staging 目录，验证 `SKILL.md`，计算内容哈希，再原子替换当前
Skill。检测到本地修改时默认不覆盖，只有显式 `--force` 才替换。Git 地址只接受 HTTPS；
同步由用户显式触发，使用用户现有凭据，超时为 120 秒，不在后台自动拉取。

## 依赖声明

Python Skill 可以使用 PEP 723 内联依赖：

```python
#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["defusedxml", "lxml"]
# ///
```

系统二进制或 Python 包依赖可写入 string-valued metadata：

```yaml
metadata:
  requires-binaries: "soffice, pdftoppm"
  requires-python-packages: "defusedxml, lxml"
```

EKO 只探测并提示依赖，不自动安装。
