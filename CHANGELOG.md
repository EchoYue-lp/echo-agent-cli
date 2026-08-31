# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Made framework `TaskStatus` the sole PlanTask execution authority, exposed
  immutable `PlanRevision` artifacts to surfaces, and reduced Todo state to a
  read-only projection with no reverse mutation path.
- **Skill catalog 收缩与官方标准化**: 全部捆绑 `SKILL.md` 迁移到 agentskills.io
  官方标准字段（`allowed-tools` 为空格分隔字符串、`metadata` 仅字符串，Skill 文件不携带
  Hook 扩展，不引入任何私有扩展命名空间）；路由改为
  description-driven。catalog 41 → 39：删除 `using-superpowers`（重复全局选技）
  与 `deep-research`（并入 `web-search` 深度调研模式），补强 `writing-skills` 与
  `mcp-builder`。新增 `skills_hub::catalog_gate` 门禁测试（零违规 +
  `BUILTIN_SKILL_NAMES` 一致）与 `skills_hub::policy_contract` 行为级契约测试
  （disabled 全投影缺席、reload 生效、fail-closed、用户 Skill 不受误伤、同名优先级、
  路径 canonicalize 边界）。ADR
  [0033](docs/zh/adr/0033-skill-catalog-contraction-and-official-frontmatter.md)。
- **Enabled Skill 运行时权威**: `enabled-skills.json` 成为 bundled Skill 的注册
  权威（ADR
  [0032](docs/zh/adr/0032-enabled-skill-runtime-authority.md)）；损坏配置
  fail-closed，builtin/user Skill 来源标记区分，pooled Agent 刷新路径补齐。

### Added

- **技能分类体系**: 6 个 category（methodology / development / document /
  design / research / automation），按 `skills/<category>/<name>/SKILL.md` 组织。
- **技能资产移植与收缩**: superpowers 方法论/工作流 13 个 + Anthropic 领域技能
  15 个 + 现有技能，全部移植到 `skills/<category>/` 目录；经本轮质量收缩后
  最终随附 39 个，见上方 Changed 条目。
- **方法论 baseline 默认挂载**: 核心 4 个方法论（brainstorming /
  systematic-debugging / verification-before-completion / writing-plans）
  的正文在 SessionStart 时注入 system prompt。
- **enabled-skills.json 配置管理**: `EnabledSkillsConfig` 模块，管理技能
  启用状态和 baseline 标记，默认配置自动生成并落盘。
- **TriggerSupervisor bootstrap 装配**: 用 TriggerSupervisor（Keyword +
  LlmIntent（可选）+ Hook slot）替代 ChainedClassifier。
- **SkillsHub category 扫描**: `scan()` 支持 `skills/<category>/<name>/`
  嵌套目录结构，`SkillHubEntry` 新增 category / is_baseline / is_builtin /
  source / upstream_version 字段。
- **前端技能分组展示**: SkillsPanel 按 category 折叠分组，显示 baseline ★
  标记、缺依赖 ⚠️ 提示、来源/版本信息。
- **eval match_fn 修真**: 用 `KeywordClassifier` 替代 `String::contains`
  字符串匹配，F1 度量反映生产路由效果。`load_skill_triggers` 支持
  category 子目录扫描。
- **文档**: Skill 分类与上游同步说明现统一维护在 `docs/skill-sync.md`。

### Changed

- Conversation follow-ups now have a single application-owned durable ingress
  contract in the existing ChatEventLog reducer. Revisioned attempts project
  persisted, mailbox-accepted, drained, settled, deferred, and recovery-required
  receipts without introducing another mailbox or driver; surface migration is
  staged behind this core authority.
- GUI, TUI, CLI, and channel active steering now use the framework tracked
  receipt (`MailboxAccepted -> Drained -> TurnSettled`) through one
  SubagentControl adapter. Cold Conversation Agent delivery carries the
  framework initial-input receipt through the shared chat driver; router
  delivery records expose the same typed phase, outcome, and drained facts.
  Terminal-before-drain and restart-after-drain remain non-replayable.
- Channel now carries framework sender-scoped sessions through EKO AgentPool,
  TaskRun, cache, foreground control, exact resume, bounded outbound rendering,
  and bidirectional canonical tool identity quarantine. Framework session
  timeout/reset now close old key admission, await exact foreground/lease
  settlement, retire the old cached Agent, reclaim its exact persisted runtime,
  and rotate model/checkpoint/cache identity while preserving stable product
  history and TaskRun state. Channel TaskRuntime and attachment/compression file
  work now use the bounded store/product-data owners; aggregate product deletion
  clears every runtime incarnation before retiring the stable transcript.
  Product-data blocking work is now owned by one per-application service that
  survives caller drop and is sealed/joined by application shutdown; the
  process-global primitive only limits concurrency.
- `SkillInfo` TypeScript 类型新增 category / is_baseline / is_builtin /
  upstream_version / has_updates / missing_dependencies 字段。
- SkillsPanel 重构为分组折叠 UI。
- bootstrap 流程新增 Step 5b（baseline 注入）和 Step 12（TriggerSupervisor）。
