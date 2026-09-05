# ADR 0038：内置 Skill Creator 与 Plugin Creator

## 状态

已采纳

## 背景

EKO 已经具备 Skill 标准解析、发现、启用与用户目录，也具备 Agent Plugins 1.0
manifest、scaffold、validate、install 和 reload 运行时。但这些协议知识分散在代码、命令
和维护者文档中：原 `writing-skills` 主要教维护者修改内置 catalog，普通用户要求 Agent
创建个人 Skill 时缺少完整目标路径；PluginRuntime 虽已提供 `/plugins init` 与
`/plugins validate`，catalog 中却没有对应 creator skill。

调研的成熟实现呈现同一模式：OpenAI Codex 随系统提供 `skill-creator` 与
`plugin-creator`，把目录协议、渐进披露、scaffold 和 validation 流程作为可按需加载的
Skill；Claude Code 官方文档也把 Skill 定义为 `SKILL.md` 加可选 supporting files，并由
description 负责发现；Agent Skills 规范把 `name`、`description` 和受限标准 frontmatter
作为跨宿主合同。共同点是“用 Skill 教 Agent 扩展协议，复用宿主已有文件与验证能力”，
而不是再增加一套 creator 状态机。

## 候选方案

1. 保留 `writing-skills`，另加两个 creator：兼容旧名称，但三者语义重叠，模型路由和维护
   会继续分叉。
2. 将 `writing-skills` 收敛为 `skill-creator`，新增 `plugin-creator`，复用现有权威实现
   （采用）。
3. 新增 `skill_create` / `plugin_create` store 与 executor：可形成结构化 API，但会平行复制
   已有文件、Skill validator 与 PluginRuntime 权威，复杂度没有对应收益。

## 决策

- 删除内置 `writing-skills`，由 `skill-creator` 覆盖创建、更新、资源组织、个人路径和
  内置 catalog 维护。
- 新增 `plugin-creator`，明确 EKO 使用根目录 `plugin.json` 与 Agent Plugins 1.0 schema，
  不采用 Codex 私有 `.codex-plugin/plugin.json` 布局。
- 两者默认启用，使名称与 description 始终进入 Agent 可发现 catalog；不加入 baseline，
  正文只在实际创建 Skill/Plugin 时加载。
- Skill 格式继续由 framework `SkillDocument` / `validate_skill_dir` 唯一负责；Plugin 创建
  和验证继续由应用现有 PluginRuntime 唯一负责。应用增加 `skill_validate`、
  `plugin_scaffold`、`plugin_validate` 三个模型可调用薄工具，与 `/plugins init`、
  `/plugins validate` 共用同一 authority。creator skill 与薄工具不拥有第二套 validator、
  store 或 lifecycle。

## 分层

- 通用机制：Agent Skills 文档解析、资源发现、脚本执行与 Agent Plugins manifest/prepared
  generation 属于 `echo-agent` framework。
- EKO 产品策略：个人目录 `~/.eko/skills`、默认启用、命令面、主题、output style、monitor
  和安装/reload 投影属于 `echo-agent-cli`。
- 适配边界：creator skill 描述现有命令与路径；三个模型工具只做参数/结果转换并调用
  framework 或 PluginRuntime，不重新实现其语义。

## 影响

- 内置 catalog 从 24 个变为 25 个（重命名 1 个、新增 1 个），默认启用从 5 个变为
  7 个，baseline 仍只有 `verification-before-completion`。
- 旧 `writing-skills` 名称不保留别名；项目仍处开发阶段，不维持过时的重复入口。
- GUI、TUI、CLI 与 channel 共用同一个 Agent bootstrap 和 catalog，因此无需分别实现。
- creator 工具由共同 Agent factory 注册，首轮保持 deferred，需要时通过 `tool_search`
  暴露，不增加普通对话的初始 schema。

## 参考

- [Agent Skills specification](https://agentskills.io/specification)
- [Claude Code: Extend Claude with skills](https://code.claude.com/docs/en/skills)
- OpenAI Codex 系统内置 `skill-creator` 与 `plugin-creator`（本机安装包与 OpenAI skills
  仓库实现，2026-09-04 调研）
