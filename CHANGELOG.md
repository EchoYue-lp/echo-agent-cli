# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **统一 turn-run 绑定与长程恢复**:每个 store-backed turn 急切绑定内部 TaskRun；
  typed provenance 区分 conversation 与 orchestrated direct run，boot recovery 和
  quiet-wake 使用原子 journal 决策，GUI/TUI/CLI/channel user steer 记录到 exact run，
  planless conversation run 不进入任务 UI，长 Goal artifact 按 revision/SHA-256 原子发布。
  ADR [0037](docs/zh/adr/0037-unified-turn-run-binding.md)。

- **Skill 内容边界收敛（2026-09）**：删除 EKO 仓库内置 `skills/`、默认启用集、
  方法论 baseline、Tauri Skill resource、内置来源字段和 creator 模型工具。产品只消费
  `~/.eko/skills/` 中用户安装的独立 Skill 与 Plugin generation 提供的 Skill；
  `enabled-skills.json` version 3 只保存 `{enabled}`。用户安装、启停、卸载、Git 同步、
  会话激活、framework 标准校验和 PluginRuntime 全部保留。ADR
  [0041](docs/zh/adr/0041-external-only-skills.md)。
- app-core 数据根与 binary 入口统一尊重绝对 `EKO_DATA_DIR`，使隔离测试和本地多实例
  不再误占用真实 `~/.eko` authority。
- `install` 复用 framework manifest validator 识别 Agent Plugins 1.0 `plugin.json`
  标准包，原子安装并启用其全部 `skills/` 子目录，为每项保留精确 Git subdir；含
  `mcp.json` 的包明确提示暂不支持。

- Made framework `TaskStatus` the sole PlanTask execution authority, exposed
  immutable `PlanRevision` artifacts to surfaces, and reduced Todo state to a
  read-only projection with no reverse mutation path.
### Added

- **enabled-skills.json 配置管理**：`EnabledSkillsConfig` 只管理外部 Skill 的
  `enabled` 状态；旧 category/baseline 字段被忽略。
- **TriggerSupervisor bootstrap 装配**: 用 TriggerSupervisor（Keyword +
  LlmIntent（可选）+ Hook slot）替代 ChainedClassifier。
- **SkillsHub 外部目录扫描**：`scan()` 支持用户 Skill 的分类目录，`SkillHubEntry`
  提供 category、source、upstream_version 与依赖状态。
- **前端技能分组展示**：SkillsPanel 按 category 折叠用户安装的 Skill，显示缺依赖提示、
  来源与版本信息。
- **eval match_fn 修真**: 用 `KeywordClassifier` 替代 `String::contains`
  字符串匹配，F1 度量反映生产路由效果。`load_skill_triggers` 支持
  category 子目录扫描。
- **文档**: Skill 分类与上游同步说明现统一维护在
  `docs/{zh,en}/operations/skill-sync.md`。

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
- `SkillHubEntry` / `ExtensionSkillEntry` TypeScript 类型提供 category、
  upstream_version、has_updates 与 missing_dependencies 等产品投影字段。
- SkillsPanel 重构为分组折叠 UI。
- bootstrap 流程新增 TriggerSupervisor。
